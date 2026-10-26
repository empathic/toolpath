//! `webmock run <agent> record|replay`: one headless agent run against the record/replay server.
//!
//! What the scratch driver used to do by hand, per agent: start a server (two for copilot: its
//! model API and the GitHub API it asks who the user is), copy a cell (`home/`, `project/`, `sid`)
//! into a temporary work directory, point the agent at the server with that agent's own switch,
//! give it a fake credential in replay, run it with an empty environment plus a hermetic `HOME`/XDG,
//! then stop the servers and report how every request was served (EXACT, LOOSE or MISS).
//!
//! The work directory is masked to the placeholder the committed cassettes carry
//! ([`placeholder`]), so a run in any directory keys the same as the recording did.
//!
//! Record mode forwards to an upstream that must be given explicitly (`--upstream`, and
//! `--github-upstream` for copilot) and takes credentials only from variables named with
//! `--pass-env`; secret headers are never written ([`crate::SECRET_HEADERS`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

/// The agents a cell can be run for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Pi,
    Copilot,
    Opencode,
    Gemini,
    /// Not an agent: `path share` against the server (`{base}` in the arguments).
    Pathbase,
}

impl FromStr for Agent {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "claude" => Agent::Claude,
            "codex" => Agent::Codex,
            "pi" => Agent::Pi,
            "copilot" => Agent::Copilot,
            "opencode" => Agent::Opencode,
            "gemini" => Agent::Gemini,
            "pathbase" => Agent::Pathbase,
            _ => bail!(
                "unknown agent {s:?} (claude, codex, pi, copilot, opencode, gemini, pathbase)"
            ),
        })
    }
}

impl Agent {
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Pi => "pi",
            Agent::Copilot => "copilot",
            Agent::Opencode => "opencode",
            Agent::Gemini => "gemini",
            Agent::Pathbase => "pathbase",
        }
    }

    /// The binary looked up on `PATH` when `--bin` is not given.
    pub fn binary(self) -> &'static str {
        match self {
            Agent::Pathbase => "path",
            a => a.name(),
        }
    }

    /// The servers this agent talks to: `api` (its model API) and, for copilot, `github`
    /// (the GitHub API it reads the signed-in user from). All are served from one cassette directory.
    pub fn listeners(self) -> &'static [&'static str] {
        match self {
            Agent::Copilot => &["api", "github"],
            _ => &["api"],
        }
    }
}

/// Where the recording ran, as the committed cassettes name it: the masked form of the work
/// directory. gemini, pi and opencode ran in place in their matrix cell (their stores are keyed by
/// the absolute project path); the others ran in a work copy.
pub fn placeholder(agent: Agent) -> String {
    match agent {
        Agent::Gemini => "<scratch>/resume-matrix/v4-cell-codex-to-gemini".into(),
        Agent::Pi => "<scratch>/resume-matrix/v2-cell-codex-to-pi".into(),
        Agent::Opencode => "<scratch>/resume-matrix/gate-cell-codex-to-opencode".into(),
        Agent::Pathbase => "<scratch>/resume-matrix/src-codex".into(),
        a => format!("<scratch>/webmock/cells/{}/work", a.name()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Record,
    Replay,
}

/// The variables every agent runs with: a hermetic home and XDG tree under `home`.
pub fn hermetic_env(home: &Path) -> Vec<(String, String)> {
    let h = home.display().to_string();
    vec![
        ("HOME".into(), h.clone()),
        ("TMPDIR".into(), h.clone()),
        ("XDG_CONFIG_HOME".into(), format!("{h}/.config")),
        ("XDG_DATA_HOME".into(), format!("{h}/.local/share")),
        ("XDG_STATE_HOME".into(), format!("{h}/.local/state")),
        ("XDG_CACHE_HOME".into(), format!("{h}/.cache")),
        ("CLAUDE_CONFIG_DIR".into(), format!("{h}/.claude")),
        ("CODEX_HOME".into(), format!("{h}/.codex")),
        ("COPILOT_HOME".into(), format!("{h}/.copilot")),
        ("TOOLPATH_CONFIG_DIR".into(), format!("{h}/.toolpath")),
    ]
}

/// What pointing one agent at the server takes: variables to set and files to write in its home.
#[derive(Debug, Default, PartialEq)]
pub struct Prepared {
    pub env: Vec<(String, String)>,
    pub files: Vec<(PathBuf, String)>,
}

/// An unsigned JWT pi accepts: it reads the ChatGPT account id out of the token before any request.
fn fake_pi_jwt() -> String {
    use base64::Engine;
    let b = |v: &Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string().as_bytes())
    };
    format!(
        "{}.{}.webmock",
        b(&json!({"alg": "none"})),
        b(
            &json!({"https://api.openai.com/auth": {"chatgpt_account_id": "webmock-replay"}, "exp": 9999999999u64})
        )
    )
}

fn read_json(path: &Path) -> Result<Value> {
    let t = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&t).with_context(|| format!("parse {}", path.display()))
}

/// The switch, fake credentials (replay) and config files for `agent`, whose home is `home`, given
/// each listener's base URL (`bases["api"]`, and `bases["github"]` for copilot).
pub fn prepare(
    agent: Agent,
    mode: RunMode,
    home: &Path,
    bases: &BTreeMap<String, String>,
) -> Result<Prepared> {
    let base = |n: &str| {
        bases
            .get(n)
            .cloned()
            .ok_or_else(|| anyhow!("no {n} listener"))
    };
    let replay = mode == RunMode::Replay;
    let mut p = Prepared::default();
    let mut env = |k: &str, v: String| p.env.push((k.to_string(), v));
    match agent {
        Agent::Claude => {
            env("ANTHROPIC_BASE_URL", base("api")?);
            for k in [
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
                "DISABLE_TELEMETRY",
                "DISABLE_ERROR_REPORTING",
                "DISABLE_AUTOUPDATER",
            ] {
                env(k, "1".into());
            }
            if replay {
                env(
                    "CLAUDE_CODE_OAUTH_TOKEN",
                    "sk-ant-oat01-webmock-replay-no-such-token".into(),
                );
            }
        }
        Agent::Codex => bail!(
            "codex: ChatGPT-auth codex refuses a plain-http backend, and this server has no TLS \
             listener yet (the prototype used a local CA: CODEX_CA_CERTIFICATE)"
        ),
        Agent::Copilot => {
            env("COPILOT_API_URL", base("api")?);
            env("COPILOT_DEBUG_GITHUB_API_URL", base("github")?);
            env("OTEL_SDK_DISABLED", "true".into());
            if replay {
                env(
                    "COPILOT_GITHUB_TOKEN",
                    "gho_webmockreplaynosuchtoken".into(),
                );
            }
        }
        Agent::Pi => {
            let dir = home.join(".pi/agent");
            p.files.push((
                dir.join("models.json"),
                json!({"providers": {"openai-codex": {"baseUrl": format!("{}/backend-api", base("api")?)}}}).to_string(),
            ));
            p.files.push((
                dir.join("settings.json"),
                json!({"defaultProvider": "openai-codex", "defaultModel": "gpt-5.6-terra", "transport": "sse"}).to_string(),
            ));
            if replay {
                let auth_path = dir.join("auth.json");
                let mut auth = if auth_path.exists() {
                    read_json(&auth_path)?
                } else {
                    json!({})
                };
                // pi 0.84 reads a credential only when it says what kind it is.
                auth["openai-codex"]["type"] = Value::String("oauth".into());
                auth["openai-codex"]["access"] = Value::String(fake_pi_jwt());
                auth["openai-codex"]["refresh"] = Value::String("webmock-replay".into());
                auth["openai-codex"]["expires"] = json!(9999999999000u64);
                p.files.push((auth_path, auth.to_string()));
            }
        }
        Agent::Opencode => {
            p.files.push((
                home.join(".config/opencode/opencode.json"),
                json!({"$schema": "https://opencode.ai/config.json", "provider": {"opencode": {"options": {"baseURL": format!("{}/zen/v1", base("api")?)}}}}).to_string(),
            ));
            if replay {
                let auth_path = home.join(".local/share/opencode/auth.json");
                let mut auth = if auth_path.exists() {
                    read_json(&auth_path)?
                } else {
                    json!({})
                };
                auth["opencode"]["key"] = Value::String("webmock-replay-no-such-key".into());
                p.files.push((auth_path, auth.to_string()));
            }
        }
        Agent::Gemini => {
            env("GEMINI_CLI_HOME", home.display().to_string());
            env("GOOGLE_GEMINI_BASE_URL", base("api")?);
            env("GEMINI_CLI_DISABLE_TELEMETRY", "1".into());
            env("NO_BROWSER", "true".into());
            env("TERM", "dumb".into());
            p.files.push((
                home.join(".gemini/settings.json"),
                json!({"security": {"auth": {"selectedType": "gemini-api-key"}}}).to_string(),
            ));
            if replay {
                p.files.push((
                    home.join(".gemini/.env"),
                    "GEMINI_API_KEY=webmock-replay-no-such-key\n".into(),
                ));
            }
        }
        Agent::Pathbase => {}
    }
    Ok(p)
}

/// `{sid}`, `{project}`, `{home}` and `{base}` in the agent's arguments.
pub fn expand_args(args: &[String], vars: &BTreeMap<&str, String>) -> Vec<String> {
    args.iter()
        .map(|a| {
            let mut s = a.clone();
            for (k, v) in vars {
                s = s.replace(&format!("{{{k}}}"), v);
            }
            s
        })
        .collect()
}

/// How the server answered each request, from its log lines.
#[derive(Debug, Default, PartialEq)]
pub struct Tally {
    pub exact: usize,
    pub loose: usize,
    pub miss: usize,
    pub recorded: usize,
}

pub fn tally(lines: &[String]) -> Tally {
    let mut t = Tally::default();
    for l in lines {
        let w = l.split_whitespace().next().unwrap_or("");
        match w {
            "EXACT" => t.exact += 1,
            "LOOSE" => t.loose += 1,
            "MISS" => t.miss += 1,
            "recorded" => t.recorded += 1,
            _ => {}
        }
    }
    t
}

/// Parsed `webmock run` arguments.
#[derive(Debug)]
pub struct Opts {
    pub agent: Agent,
    pub mode: RunMode,
    pub cell: PathBuf,
    pub cassettes: PathBuf,
    pub in_place: bool,
    pub recorded_as: Option<String>,
    pub masks: Vec<(String, String)>,
    pub upstream: Option<String>,
    pub github_upstream: Option<String>,
    pub pass_env: Vec<String>,
    pub bin: Option<PathBuf>,
    pub keep: bool,
    pub args: Vec<String>,
}

pub const USAGE: &str = "usage: webmock run <agent> record|replay --cell <dir> [--cassettes <dir>] \
[--in-place] [--recorded-as <path>] [--mask <live>=<placeholder>]... [--upstream <url>] \
[--github-upstream <url>] [--pass-env <NAME>]... [--bin <path>] [--keep] -- <agent args>\n\
agent args may use {sid} {project} {home} {base}";

pub fn parse(args: &[String]) -> Result<Opts> {
    let mut it = args.iter();
    let agent: Agent = it.next().ok_or_else(|| anyhow!(USAGE))?.parse()?;
    let mode = match it.next().map(String::as_str) {
        Some("record") => RunMode::Record,
        Some("replay") => RunMode::Replay,
        _ => bail!(USAGE),
    };
    let mut o = Opts {
        agent,
        mode,
        cell: PathBuf::new(),
        cassettes: Path::new(env!("CARGO_MANIFEST_DIR")).join("cassettes"),
        in_place: false,
        recorded_as: None,
        masks: Vec::new(),
        upstream: None,
        github_upstream: None,
        pass_env: Vec::new(),
        bin: None,
        keep: false,
        args: Vec::new(),
    };
    let mut cell = None;
    while let Some(a) = it.next() {
        let mut val = || {
            it.next()
                .cloned()
                .ok_or_else(|| anyhow!("{a} needs a value"))
        };
        match a.as_str() {
            "--cell" => cell = Some(PathBuf::from(val()?)),
            "--cassettes" => o.cassettes = PathBuf::from(val()?),
            "--in-place" => o.in_place = true,
            "--recorded-as" => o.recorded_as = Some(val()?),
            "--mask" => {
                let v = val()?;
                let (l, p) = v
                    .split_once('=')
                    .ok_or_else(|| anyhow!("--mask takes <live>=<placeholder>"))?;
                o.masks.push((l.to_string(), p.to_string()));
            }
            "--upstream" => o.upstream = Some(val()?),
            "--github-upstream" => o.github_upstream = Some(val()?),
            "--pass-env" => o.pass_env.push(val()?),
            "--bin" => o.bin = Some(PathBuf::from(val()?)),
            "--keep" => o.keep = true,
            "--" => {
                o.args = it.cloned().collect();
                break;
            }
            other => bail!("unknown option {other}\n{USAGE}"),
        }
    }
    o.cell = cell.ok_or_else(|| anyhow!("--cell is required\n{USAGE}"))?;
    if o.mode == RunMode::Record {
        if o.upstream.is_none() {
            bail!("record mode forwards to a real upstream: give it explicitly with --upstream");
        }
        if agent == Agent::Copilot && o.github_upstream.is_none() {
            bail!("copilot records two upstreams: also give --github-upstream");
        }
    }
    Ok(o)
}

/// Copy `from` into `to` (a directory tree; symlinks are copied as links).
pub fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from).with_context(|| format!("read {}", from.display()))? {
        let e = e?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let ft = e.file_type()?;
        if ft.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)?;
        } else if ft.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// What one run did.
#[derive(Debug)]
pub struct Outcome {
    pub rc: Option<i32>,
    pub seconds: u64,
    pub tally: Tally,
    /// The server's per-request lines (`EXACT …`, `LOOSE …`, `MISS …`, `recorded …`).
    pub lines: Vec<String>,
    /// Where the agent's stdout/stderr and the run's cassette copy are (kept with `--keep`).
    pub run_dir: PathBuf,
}

/// Start the servers, prepare the cell, run the agent, stop the servers.
pub async fn run(o: Opts) -> Result<Outcome> {
    use std::sync::{Arc, Mutex};

    let agent = o.agent;
    let bin = match &o.bin {
        Some(b) => b.clone(),
        None => which(agent.binary())
            .ok_or_else(|| anyhow!("{} not on PATH (use --bin)", agent.binary()))?,
    };
    let run_dir = std::env::temp_dir().join(format!(
        "webmock-run-{}-{}-{}",
        agent.name(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&run_dir)?;

    // The work copy (or the cell itself), and the placeholder it was recorded under.
    let work = if o.in_place {
        o.cell.canonicalize()?
    } else {
        let w = run_dir.join("work");
        copy_tree(&o.cell, &w)?;
        w
    };
    let home = work.join("home");
    let project = work.join("project");
    std::fs::create_dir_all(&project)?;
    let sid = std::fs::read_to_string(work.join("sid"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let mut masks = vec![(
        work.display().to_string(),
        o.recorded_as.clone().unwrap_or_else(|| placeholder(agent)),
    )];
    masks.extend(o.masks.iter().cloned());

    // Cassettes: replay serves a copy (misses and replayed bodies land there, not in the repo);
    // record writes into the cassette directory itself.
    let src = o.cassettes.join(agent.name());
    let store_dir = match o.mode {
        RunMode::Replay => {
            let d = run_dir.join("cassettes").join(agent.name());
            copy_tree(&src, &d).with_context(|| format!("cassettes {}", src.display()))?;
            d
        }
        RunMode::Record => src,
    };
    let store = Arc::new(crate::Store::open(&store_dir)?);

    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut bases = BTreeMap::new();
    let mut handles = Vec::new();
    for l in agent.listeners() {
        let mode = match o.mode {
            RunMode::Replay => crate::Mode::Replay,
            RunMode::Record => crate::Mode::Record {
                upstream: match *l {
                    "github" => o.github_upstream.clone(),
                    _ => o.upstream.clone(),
                }
                .expect("checked in parse")
                .trim_end_matches('/')
                .to_string(),
            },
        };
        let sink = lines.clone();
        let tag = format!("{}/{l}", agent.name());
        let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |line: &str| {
            eprintln!("[webmock/{tag}] {line}");
            sink.lock().unwrap().push(line.to_string());
        });
        let (addr, h) = crate::serve(
            crate::Config {
                agent: agent.name().to_string(),
                mode,
                store: store.clone(),
                log,
                masks: masks.clone(),
            },
            0,
        )
        .await?;
        bases.insert(l.to_string(), format!("http://{addr}"));
        handles.push(h);
    }

    let prep = prepare(agent, o.mode, &home, &bases)?;
    for (path, text) in &prep.files {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path, text)?;
    }
    let vars = BTreeMap::from([
        ("sid", sid),
        ("project", project.display().to_string()),
        ("home", home.display().to_string()),
        ("base", bases["api"].clone()),
    ]);
    let args = expand_args(&o.args, &vars);

    let mut cmd = std::process::Command::new(&bin);
    cmd.args(&args)
        .current_dir(&project)
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:/usr/bin:/bin",
                bin.parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
        )
        .envs(hermetic_env(&home))
        .envs(prep.env.iter().cloned())
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(run_dir.join("agent.out"))?)
        .stderr(std::fs::File::create(run_dir.join("agent.err"))?);
    if o.mode == RunMode::Record {
        for k in &o.pass_env {
            if let Ok(v) = std::env::var(k) {
                cmd.env(k, v);
            }
        }
    }
    let t0 = std::time::Instant::now();
    let status = tokio::task::spawn_blocking(move || cmd.status()).await??;
    let seconds = t0.elapsed().as_secs();
    for h in handles {
        h.abort();
    }
    let lines = lines.lock().unwrap().clone();
    Ok(Outcome {
        rc: status.code(),
        seconds,
        tally: tally(&lines),
        lines,
        run_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bases(agent: Agent) -> BTreeMap<String, String> {
        agent
            .listeners()
            .iter()
            .enumerate()
            .map(|(i, l)| (l.to_string(), format!("http://127.0.0.1:{}", 9000 + i)))
            .collect()
    }

    fn env_of(p: &Prepared, k: &str) -> Option<String> {
        p.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn record_needs_an_explicit_upstream_and_copilot_needs_both() {
        let e = parse(&s(&["claude", "record", "--cell", "/c"])).unwrap_err();
        assert!(e.to_string().contains("--upstream"), "{e}");
        let e = parse(&s(&[
            "copilot",
            "record",
            "--cell",
            "/c",
            "--upstream",
            "https://api.githubcopilot.com",
        ]))
        .unwrap_err();
        assert!(e.to_string().contains("--github-upstream"), "{e}");
        let o = parse(&s(&[
            "claude", "replay", "--cell", "/c", "--", "-p", "{sid}",
        ]))
        .unwrap();
        assert_eq!(o.args, s(&["-p", "{sid}"]));
        assert!(
            parse(&s(&["claude", "replay"])).is_err(),
            "--cell is required"
        );
        assert!(parse(&s(&["nope", "replay", "--cell", "/c"])).is_err());
    }

    #[test]
    fn masks_parse_as_live_equals_placeholder() {
        let o = parse(&s(&[
            "pi",
            "replay",
            "--cell",
            "/c",
            "--mask",
            "/tmp/x=<scratch>",
            "--in-place",
        ]))
        .unwrap();
        assert_eq!(o.masks, vec![("/tmp/x".into(), "<scratch>".into())]);
        assert!(o.in_place);
    }

    #[test]
    fn each_agent_gets_its_own_switch_and_a_fake_credential_only_in_replay() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let c = prepare(Agent::Claude, RunMode::Replay, h, &bases(Agent::Claude)).unwrap();
        assert_eq!(
            env_of(&c, "ANTHROPIC_BASE_URL").unwrap(),
            "http://127.0.0.1:9000"
        );
        assert!(
            env_of(&c, "CLAUDE_CODE_OAUTH_TOKEN")
                .unwrap()
                .contains("webmock-replay")
        );
        let c = prepare(Agent::Claude, RunMode::Record, h, &bases(Agent::Claude)).unwrap();
        assert!(
            env_of(&c, "CLAUDE_CODE_OAUTH_TOKEN").is_none(),
            "record never invents a credential"
        );

        let p = prepare(Agent::Copilot, RunMode::Replay, h, &bases(Agent::Copilot)).unwrap();
        assert_eq!(
            env_of(&p, "COPILOT_API_URL").unwrap(),
            "http://127.0.0.1:9000"
        );
        assert_eq!(
            env_of(&p, "COPILOT_DEBUG_GITHUB_API_URL").unwrap(),
            "http://127.0.0.1:9001"
        );
        assert!(
            env_of(&p, "COPILOT_GITHUB_TOKEN")
                .unwrap()
                .starts_with("gho_webmockreplay")
        );

        let g = prepare(Agent::Gemini, RunMode::Replay, h, &bases(Agent::Gemini)).unwrap();
        assert_eq!(
            env_of(&g, "GOOGLE_GEMINI_BASE_URL").unwrap(),
            "http://127.0.0.1:9000"
        );
        assert!(
            g.files
                .iter()
                .any(|(p, t)| p.ends_with(".gemini/.env") && t.contains("webmock-replay"))
        );

        let o = prepare(Agent::Opencode, RunMode::Replay, h, &bases(Agent::Opencode)).unwrap();
        let (_, cfg) = o
            .files
            .iter()
            .find(|(p, _)| p.ends_with("opencode/opencode.json"))
            .unwrap();
        assert!(cfg.contains("http://127.0.0.1:9000/zen/v1"));

        assert!(
            prepare(Agent::Codex, RunMode::Replay, h, &bases(Agent::Codex)).is_err(),
            "no TLS listener yet"
        );
    }

    #[test]
    fn pi_keeps_its_auth_file_and_swaps_in_an_unsigned_jwt() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".pi/agent");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            r#"{"openai-codex":{"type":"oauth","access":"real","refresh":"real","expires":1},"other":{"k":1}}"#,
        )
        .unwrap();
        let p = prepare(Agent::Pi, RunMode::Replay, home.path(), &bases(Agent::Pi)).unwrap();
        let (_, auth) = p
            .files
            .iter()
            .find(|(p, _)| p.ends_with("auth.json"))
            .unwrap();
        let v: Value = serde_json::from_str(auth).unwrap();
        assert_eq!(v["other"]["k"], 1, "other entries kept");
        assert_eq!(v["openai-codex"]["type"], "oauth");
        let jwt = v["openai-codex"]["access"].as_str().unwrap();
        assert_eq!(jwt.split('.').count(), 3);
        assert!(!auth.contains("\"real\""), "the real token is gone");
        let (_, models) = p
            .files
            .iter()
            .find(|(p, _)| p.ends_with("models.json"))
            .unwrap();
        assert!(models.contains("http://127.0.0.1:9000/backend-api"));
    }

    #[test]
    fn placeholders_name_where_each_recording_ran() {
        assert_eq!(
            placeholder(Agent::Claude),
            "<scratch>/webmock/cells/claude/work"
        );
        assert_eq!(
            placeholder(Agent::Copilot),
            "<scratch>/webmock/cells/copilot/work"
        );
        assert!(placeholder(Agent::Gemini).ends_with("v4-cell-codex-to-gemini"));
    }

    #[test]
    fn args_expand_and_lines_tally() {
        let vars = BTreeMap::from([("sid", "s1".to_string()), ("base", "http://b".to_string())]);
        assert_eq!(
            expand_args(&s(&["-r", "{sid}", "--url={base}", "{other}"]), &vars),
            s(&["-r", "s1", "--url=http://b", "{other}"])
        );
        let t = tally(&s(&[
            "EXACT POST /v1/messages -> 200",
            "LOOSE GET /x",
            "MISS POST /y",
            "  jsonrpc id 1 -> 2 in the replayed answer",
            "listening on http://127.0.0.1:1",
        ]));
        assert_eq!(
            t,
            Tally {
                exact: 1,
                loose: 1,
                miss: 1,
                recorded: 0
            }
        );
    }

    #[test]
    fn a_cell_is_copied_whole() {
        let from = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(from.path().join("home/.claude")).unwrap();
        std::fs::write(from.path().join("home/.claude/x.jsonl"), "{}").unwrap();
        std::fs::write(from.path().join("sid"), "abc\n").unwrap();
        let to = tempfile::tempdir().unwrap();
        copy_tree(from.path(), &to.path().join("work")).unwrap();
        assert_eq!(
            std::fs::read_to_string(to.path().join("work/sid")).unwrap(),
            "abc\n"
        );
        assert!(to.path().join("work/home/.claude/x.jsonl").is_file());
    }
}
