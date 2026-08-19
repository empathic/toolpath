//! `path resume --remote`: push a Claude session to a remote host
//! over ssh and attach to it under tmux.
//!
//! The local host does all toolpath work: it resolves the input,
//! projects the conversation in memory, stamps it with a minted
//! session id and the remote project directory, and ships the JSONL
//! over ssh's stdin. The remote does not run `path`.
//!
//! Every path is a pure function of (document, absolute remote project
//! path, remote home). Each preflight fact can veto with a clear
//! error, and none rewrites an input. Preflight is read-only: the
//! first remote write is the ship step.
//!
//! Transport is the user's `ssh` binary from the search path. Remote
//! scope is POSIX sh remotes; every remote argument passes
//! `sh_quote`.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::{ExecStrategy, ResumeArgs, ensure_path_with_agent, resolve_input};
use crate::harness::Harness;

/// Entry point for `path resume --remote`.
pub fn run_remote(
    args: &ResumeArgs,
    exec: &dyn ExecStrategy,
    local_home: Option<&Path>,
    local_cwd: &Path,
    search_path: &[PathBuf],
    stdin_is_tty: bool,
) -> Result<()> {
    let remote = args
        .remote
        .as_deref()
        .expect("run_remote requires --remote");

    // ssh parses a leading-dash destination as an option.
    if remote.is_empty() || remote.starts_with('-') {
        bail!("--remote must be an ssh destination such as user@host (got {remote:?})");
    }

    if !stdin_is_tty && !args.dry_run {
        bail!("`path resume --remote` needs an interactive terminal: stdin is not a TTY");
    }
    if let Some(h) = args.harness
        && h != Harness::Claude
    {
        bail!(
            "remote resume supports claude only (got --harness {})",
            h.name()
        );
    }

    let (graph, source_harness) = resolve_input(args)?;
    let path = ensure_path_with_agent(&graph)?;
    if args.harness.is_none() && source_harness != Some(Harness::Claude) {
        bail!(
            "remote resume supports claude only; the document's source is {}. \
             Pass `--harness claude` to force a Claude projection.",
            source_harness.map(|h| h.name()).unwrap_or("unknown")
        );
    }

    let conv = crate::projection::build_claude_conversation(path)?;
    if conv.session_id.is_empty() {
        bail!("projected session has no id");
    }
    let tmux_name = tmux_session_name(&conv.session_id);

    let dir = remote_dir_spec(args.cwd.as_deref(), local_cwd, local_home)?;

    let ssh = find_binary("ssh", search_path)
        .ok_or_else(|| anyhow::anyhow!("`ssh` not found on PATH"))?;
    let ssh = SshRunner {
        binary: ssh,
        remote,
    };

    // One batched, read-only preflight call. The reattach probe rides
    // along because it is read-only too.
    let script = preflight_script(&dir, &tmux_name);
    let output = ssh.run(&script, None)?;
    if !output.status.success() {
        bail!(
            "ssh to {} failed ({}):\n{}",
            remote,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }
    let pf = parse_preflight(&output)?;

    let home = validate_captured_path(&pf.home, "remote $HOME", &output)?;
    if pf.claude.is_empty() {
        let probed: Vec<String> = CLAUDE_PROBE_LOCATIONS
            .iter()
            .map(|p| format!("~/{p}"))
            .collect();
        bail!(
            "claude not found on {remote}; probed PATH, {}",
            probed.join(", ")
        );
    }
    let claude = validate_captured_path(&pf.claude, "remote claude path", &output)?;
    if !pf.tmux_ok {
        bail!("tmux not found on {remote}");
    }
    let project_path = resolved_project_path(&dir, &home);
    validate_remote_cwd(&project_path)?;
    match pf.pwd.as_deref() {
        None => bail!(
            "project directory {project_path} does not exist on {remote}; \
             create it or pass -C"
        ),
        Some(physical) if physical != project_path => bail!(
            "project directory {project_path} is not physical on {remote} \
             (it resolves to {physical}); pass the physical path: -C {physical}"
        ),
        Some(_) => {}
    }

    let attach_cmd = attach_command(&tmux_name);
    let attach_args = vec!["-t".to_string(), remote.to_string(), attach_cmd.clone()];

    if pf.session_live {
        eprintln!(
            "Remote tmux session {tmux_name} is live on {remote}. \
             Attaching without re-shipping: the session keeps the \
             content from its original push."
        );
        if args.dry_run {
            eprintln!(
                "  attach:  {} -t {} {}",
                ssh.binary.display(),
                remote,
                sh_quote(&attach_cmd)
            );
            eprintln!("Dry run: nothing was written or launched.");
            return Ok(());
        }
        return exec.exec(&ssh.binary.to_string_lossy(), &attach_args, local_cwd);
    }

    // Canonical means key-sorted: serde_json::Value's map is a
    // BTreeMap, where a direct Graph::to_json would serialize HashMap
    // fields in per-instance random order and break id stability.
    let canonical_json = serde_json::to_value(&graph)
        .and_then(|v| serde_json::to_string(&v))
        .context("serialize document")?;
    let remote_id = mint_remote_id(&canonical_json);
    if remote_id == conv.session_id {
        bail!("minted remote session id equals the source session id");
    }
    if !is_uuid_shaped(&remote_id) {
        bail!("minted remote session id is not UUID-shaped: {remote_id}");
    }

    // Slug rules live in toolpath-claude.
    let resolver = toolpath_claude::PathResolver::new().with_home(home.as_str());
    let slug_dir = path_to_string(&resolver.project_dir(&project_path)?)?;
    let target = path_to_string(&resolver.conversation_file(&project_path, &remote_id)?)?;

    let ship_cmd = ship_command(&slug_dir, &target);
    let launch_cmd = launch_command(&tmux_name, &project_path, &claude, &remote_id);

    if args.dry_run {
        eprintln!("Remote resume plan for {remote}:");
        eprintln!("  remote home:   {home}");
        eprintln!("  claude:        {claude}");
        eprintln!("  project dir:   {project_path}");
        eprintln!("  session id:    {remote_id}");
        eprintln!("  session file:  {target}");
        eprintln!("  tmux session:  {tmux_name}");
        eprintln!("  ship:    ssh {} {}", remote, sh_quote(&ship_cmd));
        eprintln!("  launch:  ssh {} {}", remote, sh_quote(&launch_cmd));
        eprintln!("  attach:  ssh -t {} {}", remote, sh_quote(&attach_cmd));
        eprintln!("Dry run: nothing was written or launched.");
        return Ok(());
    }

    let mut conv = conv;
    conv.set_session_id_and_cwd(&remote_id, &project_path);
    let jsonl = crate::projection::serialize_jsonl(&conv)?;

    eprintln!("Shipping session {remote_id} to {remote}:{target}");
    let shipped = ssh.run(&ship_cmd, Some(jsonl.as_bytes()))?;
    if !shipped.status.success() {
        bail!(
            "shipping the session to {} failed ({}):\n{}",
            remote,
            shipped.status,
            String::from_utf8_lossy(&shipped.stderr).trim_end()
        );
    }

    eprintln!("Launching {tmux_name} in {project_path}");
    let launched = ssh.run(&launch_cmd, None)?;
    if !launched.status.success() {
        bail!(
            "launching tmux session {} on {} failed ({}):\n{}",
            tmux_name,
            remote,
            launched.status,
            String::from_utf8_lossy(&launched.stderr).trim_end()
        );
    }

    exec.exec(&ssh.binary.to_string_lossy(), &attach_args, local_cwd)
}

// ── Remote directory ─────────────────────────────────────────────────

/// The remote project directory before the remote `$HOME` is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteDir {
    /// `-C` value, verbatim.
    Explicit(String),
    /// Local cwd relative to the local home; empty means `$HOME` itself.
    HomeRelative(String),
}

fn remote_dir_spec(
    cwd: Option<&Path>,
    local_cwd: &Path,
    local_home: Option<&Path>,
) -> Result<RemoteDir> {
    if let Some(p) = cwd {
        let s = p
            .to_str()
            .context("-C must be valid UTF-8")?
            .trim_end_matches('/');
        let s = if s.is_empty() { "/" } else { s };
        validate_remote_cwd(s)?;
        return Ok(RemoteDir::Explicit(s.to_string()));
    }
    let local_home =
        local_home.context("cannot determine the local home directory; pass -C <remote-dir>")?;
    match home_swap_suffix(local_cwd, local_home) {
        Some(suffix) => Ok(RemoteDir::HomeRelative(suffix)),
        None => bail!(
            "the local cwd {} is not under the local home {}; pass -C <remote-dir>",
            local_cwd.display(),
            local_home.display()
        ),
    }
}

/// Local cwd relative to the local home, as a `/`-joined suffix.
/// `None` when the cwd is not under the home.
pub(crate) fn home_swap_suffix(local_cwd: &Path, local_home: &Path) -> Option<String> {
    let rel = local_cwd.strip_prefix(local_home).ok()?;
    Some(rel.to_str()?.to_string())
}

pub(crate) fn resolved_project_path(dir: &RemoteDir, remote_home: &str) -> String {
    match dir {
        RemoteDir::Explicit(p) => p.clone(),
        RemoteDir::HomeRelative(suffix) if suffix.is_empty() => remote_home.to_string(),
        RemoteDir::HomeRelative(suffix) => {
            format!("{}/{}", remote_home.trim_end_matches('/'), suffix)
        }
    }
}

pub(crate) fn validate_remote_cwd(s: &str) -> Result<()> {
    if !s.starts_with('/') {
        bail!("the remote project directory must be absolute (got {s})");
    }
    if s.contains('\n') {
        bail!("the remote project directory must be a single line");
    }
    if s.split('/').any(|c| c == "..") {
        bail!("the remote project directory must not contain `..` (got {s})");
    }
    Ok(())
}

// ── Preflight ────────────────────────────────────────────────────────

/// Locations probed for `claude` when `command -v` finds nothing,
/// relative to the remote home. An ssh exec channel runs a non-login
/// shell whose PATH lacks the user's profile additions, so common
/// install locations get a direct probe. The preflight script and
/// the not-found error both derive from this list.
const CLAUDE_PROBE_LOCATIONS: [&str; 3] = [
    ".local/bin/claude",
    ".claude/local/claude",
    ".npm-global/bin/claude",
];

// Tags for the fact lines the remote scripts print, one
// `<tag>=<value>` line per fact. `TP_` abbreviates toolpath and keeps
// the tags distinct from any real remote output.
const TAG_HOME: &str = "TP_HOME";
const TAG_CLAUDE: &str = "TP_CLAUDE";
const TAG_TMUX: &str = "TP_TMUX";
const TAG_PWD: &str = "TP_PWD";
const TAG_SESSION: &str = "TP_SESSION";
/// Emit and parse order of the preflight fact lines. The script
/// prints one `<tag>=<value>` line per entry; the parser reads them
/// back in this order.
const PREFLIGHT_TAGS: [&str; 5] = [TAG_HOME, TAG_CLAUDE, TAG_TMUX, TAG_PWD, TAG_SESSION];

#[derive(Debug)]
pub(crate) struct PreflightFacts {
    pub(crate) home: String,
    pub(crate) claude: String,
    pub(crate) tmux_ok: bool,
    /// Physical path from `pwd -P`, `None` when the dir is missing.
    pub(crate) pwd: Option<String>,
    pub(crate) session_live: bool,
}

/// One POSIX-sh script gathering every preflight fact as `TP_*=` lines.
/// Read-only: it makes no change on the remote.
pub(crate) fn preflight_script(dir: &RemoteDir, tmux_name: &str) -> String {
    let dir_expr = match dir {
        RemoteDir::Explicit(p) => sh_quote(p),
        RemoteDir::HomeRelative(suffix) if suffix.is_empty() => "\"$HOME\"".to_string(),
        RemoteDir::HomeRelative(suffix) => format!("\"$HOME\"{}", sh_quote(&format!("/{suffix}"))),
    };
    let probes: String = CLAUDE_PROBE_LOCATIONS
        .iter()
        .map(|p| format!("if [ -z \"$c\" ] && [ -x \"$HOME/{p}\" ]; then c=\"$HOME/{p}\"; fi\n"))
        .collect();
    format!(
        r#"set -u
printf '{home}=%s\n' "$HOME"
c=''
if command -v claude >/dev/null 2>&1; then c=$(command -v claude); fi
{probes}printf '{claude}=%s\n' "$c"
if command -v tmux >/dev/null 2>&1; then t=ok; else t=missing; fi
printf '{tmux}=%s\n' "$t"
if cd {dir_expr} 2>/dev/null; then p=$(pwd -P); else p=''; fi
printf '{pwd}=%s\n' "$p"
if [ "$t" = ok ] && tmux has-session -t {name} 2>/dev/null; then s=live; else s=none; fi
printf '{session}=%s\n' "$s"
"#,
        home = TAG_HOME,
        claude = TAG_CLAUDE,
        tmux = TAG_TMUX,
        pwd = TAG_PWD,
        session = TAG_SESSION,
        probes = probes,
        dir_expr = dir_expr,
        // A bare `-t <name>` matches any session whose name starts
        // with <name>, so a session named `path-x-foo` would read as
        // a live `path-x`. The `=` prefix requires the exact name.
        name = sh_quote(&format!("={tmux_name}")),
    )
}

/// Parse the five `TP_*=` lines. Anything else (a registration notice,
/// a MOTD on stdout, a partial run) errors with the output verbatim,
/// so a banner cannot become a path component.
pub(crate) fn parse_preflight(output: &Output) -> Result<PreflightFacts> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    let tags: Vec<String> = PREFLIGHT_TAGS.iter().map(|t| format!("{t}=")).collect();
    if lines.len() != tags.len()
        || lines
            .iter()
            .zip(&tags)
            .any(|(l, t)| !l.starts_with(t.as_str()))
    {
        bail!(
            "unexpected preflight output from the remote (a login banner or notice?); \
             output was:\n{}",
            preflight_transcript(output)
        );
    }
    let val = |i: usize| lines[i][tags[i].len()..].to_string();
    let pwd = val(3);
    Ok(PreflightFacts {
        home: val(0),
        claude: val(1),
        tmux_ok: val(2) == "ok",
        pwd: if pwd.is_empty() { None } else { Some(pwd) },
        session_live: val(4) == "live",
    })
}

fn preflight_transcript(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut s = stdout.trim_end().to_string();
    if !stderr.trim().is_empty() {
        s.push_str("\n(stderr) ");
        s.push_str(stderr.trim_end());
    }
    s
}

/// A value captured from the remote may only become a path component if
/// it is a non-empty single line starting with `/`.
fn validate_captured_path(value: &str, what: &str, output: &Output) -> Result<String> {
    if value.is_empty() || !value.starts_with('/') || value.contains('\n') {
        bail!(
            "{what} is not an absolute single-line path; preflight output was:\n{}",
            preflight_transcript(output)
        );
    }
    Ok(value.to_string())
}

// ── Remote commands ──────────────────────────────────────────────────

pub(crate) fn ship_command(slug_dir: &str, target: &str) -> String {
    format!(
        "umask 077; mkdir -p {} && cat > {}",
        sh_quote(slug_dir),
        sh_quote(target)
    )
}

pub(crate) fn launch_command(
    tmux_name: &str,
    cwd: &str,
    claude_path: &str,
    session_id: &str,
) -> String {
    // The inner string is the command tmux hands to `sh -c`, so it is
    // quoted twice: once for that inner shell, once for the remote
    // shell that parses the whole tmux invocation.
    let inner = format!(
        "env LANG=C.UTF-8 {} -r {}",
        sh_quote(claude_path),
        sh_quote(session_id)
    );
    format!(
        "tmux new-session -d -s {} -c {} {}",
        sh_quote(tmux_name),
        sh_quote(cwd),
        sh_quote(&inner)
    )
}

/// The `=` target prefix pins tmux to an exact session-name match;
/// `-d` detaches any stale client. Fresh launch and reattach both end
/// in this command.
pub(crate) fn attach_command(tmux_name: &str) -> String {
    format!(
        "tmux attach-session -d -t {}",
        sh_quote(&format!("={tmux_name}"))
    )
}

// ── Small pure helpers ───────────────────────────────────────────────

/// POSIX single-quote escaping: the only special character inside
/// single quotes is the single quote itself.
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `path-<short8>`: the first 8 characters of the source session id,
/// with anything outside `[A-Za-z0-9_-]` replaced by `-`.
pub fn tmux_session_name(source_session_id: &str) -> String {
    let short: String = source_session_id
        .chars()
        .take(8)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("path-{short}")
}

/// A UUID formatted from the SHA-256 of the document JSON.
/// `Builder::from_random_bytes` forces the version-4 and variant
/// bits. Callers pass the canonical serialization of the parsed
/// document, so unchanged content mints the same id across
/// invocations and input shapes.
pub fn mint_remote_id(doc_json: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(doc_json.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    uuid::Builder::from_random_bytes(b).into_uuid().to_string()
}

pub(crate) fn is_uuid_shaped(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(len, p)| p.len() == *len && p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn find_binary(name: &str, search_path: &[PathBuf]) -> Option<PathBuf> {
    search_path
        .iter()
        .map(|d| d.join(name))
        .find(|c| c.is_file())
}

fn path_to_string(p: &Path) -> Result<String> {
    Ok(p.to_str()
        .context("remote path is not valid UTF-8")?
        .to_string())
}

// ── ssh transport ────────────────────────────────────────────────────

struct SshRunner<'a> {
    binary: PathBuf,
    remote: &'a str,
}

impl SshRunner<'_> {
    /// Run `ssh <remote> <command>` with stdin fed from `stdin` (or
    /// closed), capturing stdout and stderr.
    fn run(&self, command: &str, stdin: Option<&[u8]>) -> Result<Output> {
        use std::io::Write;
        let mut cmd = Command::new(&self.binary);
        cmd.arg(self.remote)
            .arg(command)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = spawn_retrying_busy(&mut cmd)
            .with_context(|| format!("spawn {}", self.binary.display()))?;
        // Feed stdin from a thread while the parent drains stdout and
        // stderr: a write-then-drain sequence deadlocks once either
        // side outgrows a pipe buffer. A write error (the remote
        // exited before reading) is dropped so the status and stderr
        // in the output carry the real failure.
        let writer = match stdin {
            Some(bytes) => {
                let mut pipe = child.stdin.take().context("child stdin unavailable")?;
                let bytes = bytes.to_vec();
                Some(std::thread::spawn(move || pipe.write_all(&bytes)))
            }
            None => None,
        };
        let output = child
            .wait_with_output()
            .with_context(|| format!("wait for {}", self.binary.display()))?;
        if let Some(w) = writer {
            let _ = w.join();
        }
        Ok(output)
    }
}

/// `spawn` with a retry on `ExecutableFileBusy`. Parallel test threads
/// can hold a freshly written ssh shim open across another thread's
/// fork-to-exec window; retrying is harmless in production.
fn spawn_retrying_busy(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    let mut delay = std::time::Duration::from_millis(5);
    for _ in 0..5 {
        match cmd.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(delay);
                delay *= 2;
            }
            other => return other,
        }
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sh_quote ─────────────────────────────────────────────────────

    #[test]
    fn sh_quote_plain_and_empty() {
        assert_eq!(sh_quote("/a/b"), "'/a/b'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn sh_quote_embedded_single_quote() {
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn sh_quote_round_trips_through_sh() {
        let nasty = "$(rm -rf ~); `x`; $HOME 'quoted' \"double\" \\ * ? ; & | > <";
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {}", sh_quote(nasty)))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), nasty);
    }

    // ── tmux session name ────────────────────────────────────────────

    #[test]
    fn tmux_name_takes_first_8_chars() {
        assert_eq!(
            tmux_session_name("abcd1234-5678-90ab-cdef-000000000000"),
            "path-abcd1234"
        );
    }

    #[test]
    fn tmux_name_sanitizes_and_accepts_short_ids() {
        assert_eq!(tmux_session_name("a.b:c/d!"), "path-a-b-c-d-");
        assert_eq!(tmux_session_name("ab"), "path-ab");
    }

    // ── remote id minting ────────────────────────────────────────────

    #[test]
    fn mint_is_idempotent_and_input_sensitive() {
        let a = mint_remote_id("{\"doc\":1}");
        let b = mint_remote_id("{\"doc\":1}");
        let c = mint_remote_id("{\"doc\":2}");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn mint_is_uuid_shaped_with_forced_bits() {
        let id = mint_remote_id("anything");
        assert!(is_uuid_shaped(&id), "not uuid-shaped: {id}");
        assert_eq!(&id[14..15], "4", "version nibble: {id}");
        assert!(
            matches!(&id[19..20], "8" | "9" | "a" | "b"),
            "variant nibble: {id}"
        );
    }

    #[test]
    fn canonical_serialization_sorts_map_keys() {
        // Minted-id stability requires serde_json::Value to sort map
        // keys. Any dependency that enables serde_json's
        // `preserve_order` feature switches Value to insertion order
        // for the whole workspace and changes every minted id.
        let v: serde_json::Value = serde_json::from_str(r#"{"zeta":1,"alpha":2,"mid":3}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"alpha":2,"mid":3,"zeta":1}"#
        );
    }

    #[test]
    fn mint_differs_from_a_source_session_id() {
        let doc = "{\"session\":\"11111111-2222-4333-8444-555555555555\"}";
        assert_ne!(mint_remote_id(doc), "11111111-2222-4333-8444-555555555555");
    }

    #[test]
    fn uuid_shape_check() {
        assert!(is_uuid_shaped("0a1b2c3d-0000-4000-8000-000000000000"));
        assert!(!is_uuid_shaped("0a1b2c3d-0000-4000-8000-00000000000"));
        assert!(!is_uuid_shaped("not-a-uuid"));
        assert!(!is_uuid_shaped("../../../../etc/passwd"));
    }

    // ── home swap ────────────────────────────────────────────────────

    #[test]
    fn home_swap_maps_subdirectories() {
        assert_eq!(
            home_swap_suffix(Path::new("/home/r/work/proj"), Path::new("/home/r")),
            Some("work/proj".to_string())
        );
    }

    #[test]
    fn home_swap_of_home_itself_is_empty() {
        assert_eq!(
            home_swap_suffix(Path::new("/home/r"), Path::new("/home/r")),
            Some(String::new())
        );
    }

    #[test]
    fn home_swap_outside_home_is_none() {
        assert_eq!(
            home_swap_suffix(Path::new("/srv/data"), Path::new("/home/r")),
            None
        );
    }

    #[test]
    fn resolved_project_path_joins_home_and_suffix() {
        let dir = RemoteDir::HomeRelative("work/proj".to_string());
        assert_eq!(
            resolved_project_path(&dir, "/home/exedev"),
            "/home/exedev/work/proj"
        );
        let home_only = RemoteDir::HomeRelative(String::new());
        assert_eq!(
            resolved_project_path(&home_only, "/home/exedev"),
            "/home/exedev"
        );
        let explicit = RemoteDir::Explicit("/data/proj".to_string());
        assert_eq!(
            resolved_project_path(&explicit, "/home/exedev"),
            "/data/proj"
        );
    }

    // ── remote cwd validation ────────────────────────────────────────

    #[test]
    fn remote_cwd_must_be_absolute_without_dotdot() {
        assert!(validate_remote_cwd("/home/exedev/proj").is_ok());
        assert!(validate_remote_cwd("relative/dir").is_err());
        assert!(validate_remote_cwd("/home/../etc").is_err());
        assert!(validate_remote_cwd("/a\n/b").is_err());
    }

    // ── preflight parse ──────────────────────────────────────────────

    fn output_with_stdout(stdout: &str) -> Output {
        use std::os::unix::process::ExitStatusExt;
        Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn preflight_parses_tagged_lines() {
        let out = output_with_stdout(
            "TP_HOME=/home/exedev\nTP_CLAUDE=/usr/bin/claude\nTP_TMUX=ok\nTP_PWD=/home/exedev/proj\nTP_SESSION=none\n",
        );
        let pf = parse_preflight(&out).unwrap();
        assert_eq!(pf.home, "/home/exedev");
        assert_eq!(pf.claude, "/usr/bin/claude");
        assert!(pf.tmux_ok);
        assert_eq!(pf.pwd.as_deref(), Some("/home/exedev/proj"));
        assert!(!pf.session_live);
    }

    #[test]
    fn preflight_missing_dir_and_live_session_decode() {
        let out = output_with_stdout(
            "TP_HOME=/h\nTP_CLAUDE=\nTP_TMUX=missing\nTP_PWD=\nTP_SESSION=live\n",
        );
        let pf = parse_preflight(&out).unwrap();
        assert!(pf.claude.is_empty());
        assert!(!pf.tmux_ok);
        assert!(pf.pwd.is_none());
        assert!(pf.session_live);
    }

    #[test]
    fn preflight_banner_is_rejected_verbatim() {
        let banner = "Please complete registration at https://exe.dev to continue.";
        let err = parse_preflight(&output_with_stdout(banner)).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("banner"), "actual: {s}");
        assert!(s.contains(banner), "verbatim output missing: {s}");
    }

    #[test]
    fn preflight_script_is_read_only_and_quotes_the_dir() {
        let dir = RemoteDir::Explicit("/data/it's".to_string());
        let script = preflight_script(&dir, "path-abcd1234");
        assert!(script.contains("pwd -P"));
        assert!(script.contains("'/data/it'\\''s'"));
        assert!(script.contains("has-session -t '=path-abcd1234'"));
        for verb in ["mkdir", ">", "rm ", "touch"] {
            assert!(
                !script.contains(&format!("\n{verb}")),
                "preflight must be read-only; found {verb}"
            );
        }
    }

    #[test]
    fn preflight_script_home_relative_dir_uses_remote_home() {
        let dir = RemoteDir::HomeRelative("work/proj".to_string());
        let script = preflight_script(&dir, "path-x");
        assert!(
            script.contains(r#"cd "$HOME"'/work/proj'"#),
            "script:\n{script}"
        );
        let home_only = preflight_script(&RemoteDir::HomeRelative(String::new()), "path-x");
        assert!(home_only.contains(r#"cd "$HOME" "#), "script:\n{home_only}");
    }

    // ── remote command construction ──────────────────────────────────

    #[test]
    fn ship_command_makes_dir_and_writes_under_umask() {
        let cmd = ship_command(
            "/home/e/.claude/projects/-home-e-proj",
            "/home/e/.claude/projects/-home-e-proj/abc.jsonl",
        );
        assert_eq!(
            cmd,
            "umask 077; mkdir -p '/home/e/.claude/projects/-home-e-proj' && \
             cat > '/home/e/.claude/projects/-home-e-proj/abc.jsonl'"
        );
    }

    #[test]
    fn attach_command_pins_the_exact_session_name() {
        assert_eq!(
            attach_command("path-abcd1234"),
            "tmux attach-session -d -t '=path-abcd1234'"
        );
    }

    #[test]
    fn launch_command_nests_quoting_for_tmux() {
        let cmd = launch_command("path-abcd1234", "/home/e/proj", "/usr/bin/claude", "id-1");
        assert_eq!(
            cmd,
            "tmux new-session -d -s 'path-abcd1234' -c '/home/e/proj' \
             'env LANG=C.UTF-8 '\\''/usr/bin/claude'\\'' -r '\\''id-1'\\'''"
        );
    }
}
