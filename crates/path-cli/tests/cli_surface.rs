//! The `path` CLI surface, pinned against the goldens so a breaking change to a documented
//! command fails by name.
//!
//! Three layers, all hermetic (empty environment, a temp `HOME` and `TMPDIR`, `TZ=UTC`, a fixed
//! terminal width, no network):
//!
//! * **Help tree** (`help@<group>.snap`): every command and subcommand the binary exposes, found by
//!   walking `--help` recursively, with its full help text. A rename, a removed flag or a changed
//!   default is a named snapshot diff.
//! * **CLI page vs binary** (`cli_page_vs_binary.snap`): the command tree documented in
//!   `site/pages/cli.md` against the one the binary exposes — documented-but-missing,
//!   present-but-undocumented, and per-command flag differences. The diff is pinned, so the page and
//!   the binary can only drift apart in a reviewed change.
//! * **Behaviour** (`behaviour@<command>.snap`): each documented workflow and plumbing command run
//!   over the committed examples and golden sessions, with stdout (normalised) and exit code.
//!
//! Covered elsewhere and not repeated here: `path query` with the site's example and the
//! quick-start flow (`p cache sync`, `share`, `resume`, the plugin) have their own tests; `share`,
//! `p export pathbase`, `auth login` and `resume` flag rules are in `tests/integration.rs`;
//! `p render md` is in `tests/render_md_snapshots.rs`; the writers' bytes are pinned by
//! `toolpath-goldens` (`projection@…`).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run the built `path` in an empty environment rooted at `home`.
fn run_in(home: &Path, cwd: Option<&Path>, args: &[&str], stdin: Option<&str>) -> Run {
    let tmp = home.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_path"));
    cmd.env_clear()
        .env("HOME", home)
        .env("TMPDIR", &tmp)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("PATH", "/usr/bin:/bin")
        .env("TZ", "UTC")
        .env("COLUMNS", "100")
        .env("NO_COLOR", "1")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let mut child = cmd.spawn().expect("spawn path");
    if let Some(s) = stdin {
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn run(args: &[&str]) -> Run {
    let home = tempfile::tempdir().unwrap();
    run_in(home.path(), None, args, None)
}

/// Replace what differs between runs: temp paths, wall-clock stamps, tracker ids.
fn normalise(s: &str, home: Option<&Path>) -> String {
    let mut s = s.to_string();
    if let Some(h) = home {
        // macOS hands out /var/… and /private/var/… for the same temp dir.
        let h = h.to_string_lossy().into_owned();
        let private = format!("/private{h}");
        let slug = |p: &str| p.replace(['/', '_', '.'], "-");
        s = s
            .replace(&slug(&private), "<home-slug>")
            .replace(&slug(&h), "<home-slug>")
            .replace(&private, "<home>")
            .replace(&h, "<home>");
    }
    s = replace_iso(&s, "<timestamp>");
    s = replace_track(&s, "track-<stamp>-<pid>");
    s
}

/// ISO-8601 UTC stamps (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) become `with`.
fn replace_iso(s: &str, with: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let d = |k: usize| k < chars.len() && chars[k].is_ascii_digit();
    while i < chars.len() {
        let ok = i + 19 <= chars.len()
            && (0..4).all(|k| d(i + k))
            && chars[i + 4] == '-'
            && d(i + 5)
            && d(i + 6)
            && chars[i + 7] == '-'
            && d(i + 8)
            && d(i + 9)
            && chars[i + 10] == 'T'
            && d(i + 11)
            && d(i + 12)
            && chars[i + 13] == ':'
            && d(i + 14)
            && d(i + 15)
            && chars[i + 16] == ':'
            && d(i + 17)
            && d(i + 18);
        if ok {
            let mut j = i + 19;
            if j < chars.len() && chars[j] == '.' {
                j += 1;
                while d(j) {
                    j += 1;
                }
            }
            if j < chars.len() && chars[j] == 'Z' {
                out.push_str(with);
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn replace_track(s: &str, with: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("track-") {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos + 6..];
        let b = tail.as_bytes();
        let ok = b.len() >= 16
            && b[..8].iter().all(u8::is_ascii_digit)
            && b[8] == b'T'
            && b[9..15].iter().all(u8::is_ascii_digit)
            && b[15] == b'-';
        if ok {
            let mut j = 16;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > 16 {
                out.push_str(with);
                rest = &tail[j..];
                continue;
            }
        }
        out.push_str("track-");
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn snapshot_settings() -> insta::Settings {
    let mut s = insta::Settings::clone_current();
    s.set_snapshot_path("snapshots");
    s.set_prepend_module_to_snapshot(false);
    s
}

// ── Help tree ──────────────────────────────────────────────────────────────

/// The subcommand names a `--help` page lists under "Commands:".
fn subcommands(help: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_cmds = false;
    for line in help.lines() {
        if line.trim_end() == "Commands:" {
            in_cmds = true;
            continue;
        }
        if in_cmds {
            if line.trim().is_empty() || !line.starts_with(' ') {
                in_cmds = false;
                continue;
            }
            if let Some(name) = line.split_whitespace().next()
                && name != "help"
            {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Every command path the binary exposes, with its `--help` text.
fn help_tree() -> BTreeMap<Vec<String>, String> {
    let mut tree = BTreeMap::new();
    let mut stack: Vec<Vec<String>> = vec![vec![]];
    while let Some(cmd) = stack.pop() {
        let mut args: Vec<&str> = cmd.iter().map(String::as_str).collect();
        args.push("--help");
        let r = run(&args);
        assert_eq!(
            r.code,
            0,
            "`path {} --help` failed: {}",
            cmd.join(" "),
            r.stderr
        );
        for sub in subcommands(&r.stdout) {
            let mut next = cmd.clone();
            next.push(sub);
            stack.push(next);
        }
        tree.insert(cmd, r.stdout);
    }
    tree
}

#[test]
fn help_tree_is_pinned() {
    let tree = help_tree();
    // One snapshot per top-level group, so a change is named by its group.
    let mut groups: BTreeMap<String, String> = BTreeMap::new();
    for (cmd, help) in &tree {
        let group = match cmd.as_slice() {
            [] => "path".to_string(),
            [one] => one.clone(),
            [p, two, ..] if p == "p" => format!("p-{two}"),
            [first, ..] => first.clone(),
        };
        let body = groups.entry(group).or_default();
        body.push_str(&format!(
            "===== path {}\n{}\n",
            cmd.join(" "),
            help.trim_end()
        ));
    }
    let settings = snapshot_settings();
    settings.bind(|| {
        for (group, body) in &groups {
            insta::assert_snapshot!(format!("help@{group}"), body);
        }
    });
}

// ── CLI page vs binary ─────────────────────────────────────────────────────

/// The command tree in `site/pages/cli.md`'s "## Commands" block: command path → flags it lists.
fn documented_tree() -> BTreeMap<Vec<String>, BTreeSet<String>> {
    let page = std::fs::read_to_string(repo_root().join("site/pages/cli.md")).unwrap();
    let start = page
        .find("## Commands")
        .expect("cli.md has a Commands section");
    let block_start = page[start..].find("```").unwrap() + start + 3;
    let block_start = page[block_start..].find('\n').unwrap() + block_start + 1;
    let block_end = page[block_start..].find("```").unwrap() + block_start;
    let block = &page[block_start..block_end];

    let mut tree: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut last: Option<Vec<String>> = None;
    for raw in block.lines() {
        if raw.trim() == "path" || raw.trim().is_empty() {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let text = raw.trim();
        let content = text.split('#').next().unwrap_or("").trim();
        let first = content.split_whitespace().next().unwrap_or("");
        let is_cmd = !first.is_empty()
            && first
                .chars()
                .next()
                .map(|c| c.is_ascii_lowercase())
                .unwrap_or(false)
            && first.chars().all(|c| c.is_ascii_lowercase() || c == '-');
        // A command line starts at a nesting column (multiples of 2) with a lowercase word; a
        // continuation line (flags only) belongs to the last command.
        if is_cmd && indent % 2 == 0 && indent <= 6 {
            while stack.last().map(|(i, _)| *i >= indent).unwrap_or(false) {
                stack.pop();
            }
            stack.push((indent, first.to_string()));
            let path: Vec<String> = stack.iter().map(|(_, n)| n.clone()).collect();
            tree.entry(path.clone()).or_default();
            // `auth  login | status | whoami | logout [--url URL]` lists subcommands inline; the
            // flags on that line belong to each of them, not to the group.
            let inline: Vec<String> = if content.contains('|') {
                content[first.len()..]
                    .split('|')
                    .filter_map(|sub| sub.split_whitespace().next())
                    .filter(|s| s.chars().all(|c| c.is_ascii_lowercase()) && !s.is_empty())
                    .map(str::to_string)
                    .collect()
            } else {
                Vec::new()
            };
            if inline.is_empty() {
                for f in flags_in(content) {
                    tree.entry(path.clone()).or_default().insert(f);
                }
            } else {
                for sub in inline {
                    let mut p = path.clone();
                    p.push(sub);
                    let e = tree.entry(p).or_default();
                    for f in flags_in(content) {
                        e.insert(f);
                    }
                }
            }
            last = Some(path);
        } else if let Some(p) = &last {
            for f in flags_in(content) {
                tree.entry(p.clone()).or_default().insert(f);
            }
        }
    }
    tree
}

fn flags_in(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || c == '[' || c == ']' || c == '|')
        .filter(|t| t.starts_with('-') && t.len() > 1)
        .map(|t| {
            t.trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .split('=')
                .next()
                .unwrap()
                .to_string()
        })
        .filter(|t| t != "-" && t != "--")
        .collect()
}

/// The flags a `--help` page lists, by long name (a short alias maps to its long form), minus the
/// ones every command carries; plus the short → long map for the page's short spellings.
fn flags_of(help: &str) -> (BTreeSet<String>, BTreeMap<String, String>) {
    let global = ["--help", "--pretty", "--picker", "--version"];
    let mut longs = BTreeSet::new();
    let mut shorts = BTreeMap::new();
    for line in help.lines() {
        let t = line.trim_start();
        if !t.starts_with('-') {
            continue;
        }
        let toks: Vec<&str> = t
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|x| x.starts_with('-') && x.len() > 1 && !x.starts_with("---"))
            .take(2)
            .collect();
        let long = toks
            .iter()
            .find(|x| x.starts_with("--"))
            .map(|x| x.split('=').next().unwrap().to_string());
        let short = toks
            .iter()
            .find(|x| !x.starts_with("--"))
            .map(|x| x.to_string());
        match (short, long) {
            (Some(sh), Some(lo)) => {
                shorts.insert(sh, lo.clone());
                if !global.contains(&lo.as_str()) {
                    longs.insert(lo);
                }
            }
            (None, Some(lo)) => {
                if !global.contains(&lo.as_str()) {
                    longs.insert(lo);
                }
            }
            (Some(sh), None) if sh != "-h" && sh != "-V" => {
                longs.insert(sh);
            }
            _ => {}
        }
    }
    (longs, shorts)
}

#[test]
fn cli_page_vs_binary() {
    let documented = documented_tree();
    let binary: BTreeMap<Vec<String>, String> = help_tree()
        .into_iter()
        // `path d` exists only under the `develop` feature this test is built with.
        .filter(|(k, _)| k.first().map(String::as_str) != Some("d"))
        .collect();

    let mut out = String::new();
    let name = |k: &Vec<String>| format!("path {}", k.join(" "));

    out.push_str("documented on the CLI page but not in the binary:\n");
    for k in documented.keys() {
        if !binary.contains_key(k) {
            out.push_str(&format!("  {}\n", name(k)));
        }
    }
    out.push_str("\nin the binary but not on the CLI page:\n");
    for k in binary.keys() {
        if k.is_empty() {
            continue;
        }
        if !documented.contains_key(k) {
            out.push_str(&format!("  {}\n", name(k)));
        }
    }
    out.push_str("\nflags that differ (documented commands only):\n");
    for (k, doc_flags) in &documented {
        let Some(help) = binary.get(k) else { continue };
        // A group's own page lists no flags of its own; only compare leaves and documented lists.
        if doc_flags.is_empty() && !super_leaf(help) {
            continue;
        }
        let (bin_flags, shorts) = flags_of(help);
        let doc_flags: BTreeSet<String> = doc_flags
            .iter()
            .map(|f| shorts.get(f).cloned().unwrap_or_else(|| f.clone()))
            .collect();
        let missing: Vec<_> = doc_flags.difference(&bin_flags).cloned().collect();
        let extra: Vec<_> = bin_flags.difference(&doc_flags).cloned().collect();
        if !missing.is_empty() {
            out.push_str(&format!(
                "  {}: documented, not accepted: {}\n",
                name(k),
                missing.join(" ")
            ));
        }
        if !extra.is_empty() {
            out.push_str(&format!(
                "  {}: accepted, not documented: {}\n",
                name(k),
                extra.join(" ")
            ));
        }
    }
    out.push_str("\nglobal flags every command accepts, not on the page: --picker\n");

    let settings = snapshot_settings();
    settings.bind(|| insta::assert_snapshot!("cli_page_vs_binary", out));
}

/// A help page with no "Commands:" section is a leaf command.
fn super_leaf(help: &str) -> bool {
    subcommands(help).is_empty()
}

// ── Behaviour ──────────────────────────────────────────────────────────────

fn examples() -> PathBuf {
    repo_root().join("examples")
}

/// JSON output with its object keys sorted. Some commands emit object keys in hash order, so
/// the same input prints differently from run to run (`p merge`, `p query ancestors` over a
/// `structural` change); the snapshot pins the content, and the qualifier record on the CLI page
/// records the ordering.
fn canonical_json(s: &str) -> Option<String> {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let sorted: BTreeMap<String, Value> =
                    m.iter().map(|(k, v)| (k.clone(), sort(v))).collect();
                Value::Object(sorted.into_iter().collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    let v: Value = serde_json::from_str(s.trim()).ok()?;
    serde_json::to_string_pretty(&sort(&v)).ok()
}

fn transcript(label: &str, args: &[&str], r: &Run, home: Option<&Path>) -> String {
    format!(
        "$ path {}\nexit: {}\n--- stdout\n{}\n--- stderr\n{}\n# {label}\n",
        args.join(" "),
        r.code,
        normalise(
            &canonical_json(&r.stdout).unwrap_or_else(|| r.stdout.trim_end().to_string()),
            home
        ),
        normalise(r.stderr.trim_end(), home),
    )
}

#[test]
fn haiku_exits_zero() {
    let r = run(&["haiku"]);
    assert_eq!(r.code, 0);
    assert!(!r.stdout.trim().is_empty(), "haiku printed nothing");
}

#[test]
fn kind_lists_and_prints_the_current_kind() {
    let list = run(&["kind"]);
    let schema = run(&["kind", "agent-coding-session"]);
    assert_eq!(schema.code, 0);
    let v: Value = serde_json::from_str(&schema.stdout).expect("kind prints JSON");
    // The unversioned selector resolves to the current version, which is the one producers write.
    assert_eq!(
        v["$id"],
        "https://toolpath.net/kinds/agent-coding-session/v1.1.0/schema.json"
    );
    let settings = snapshot_settings();
    settings.bind(|| {
        insta::assert_snapshot!(
            "behaviour@kind",
            transcript("list bundled kinds", &["kind"], &list, None)
        )
    });
}

#[test]
fn auth_status_without_credentials() {
    let home = tempfile::tempdir().unwrap();
    let r = run_in(home.path(), None, &["auth", "status"], None);
    let settings = snapshot_settings();
    settings.bind(|| {
        insta::assert_snapshot!(
            "behaviour@auth-status",
            transcript(
                "no credentials in HOME",
                &["auth", "status"],
                &r,
                Some(home.path())
            )
        )
    });
}

#[test]
fn validate_accepts_valid_and_rejects_invalid() {
    let home = tempfile::tempdir().unwrap();
    let valid = examples().join("path-04-exploration.path.json");
    let invalid = home.path().join("invalid.path.json");
    // A step whose parent does not exist in its path.
    std::fs::write(
        &invalid,
        r#"{"graph":{"id":"g"},"paths":[{"path":{"id":"p","head":"s2"},"steps":[{"step":{"id":"s2","parents":["missing"],"actor":"human:a","timestamp":"2026-01-01T00:00:00Z"},"change":{"a.txt":{"raw":"@@ -1 +1 @@\n-a\n+b"}}}]}]}"#,
    )
    .unwrap();
    let no_actor = home.path().join("no-actor.path.json");
    std::fs::write(
        &no_actor,
        r#"{"graph":{"id":"g"},"paths":[{"path":{"id":"p","head":"s1"},"steps":[{"step":{"id":"s1","timestamp":"2026-01-01T00:00:00Z"},"change":{"a.txt":{"raw":"@@ -1 +1 @@\n-a\n+b"}}}]}]}"#,
    )
    .unwrap();
    let ok = run_in(
        home.path(),
        None,
        &["p", "validate", "--input", valid.to_str().unwrap()],
        None,
    );
    let missing_actor = run_in(
        home.path(),
        None,
        &["p", "validate", "--input", no_actor.to_str().unwrap()],
        None,
    );
    assert_ne!(
        missing_actor.code, 0,
        "a step without an actor must not validate"
    );
    let bad = run_in(
        home.path(),
        None,
        &["p", "validate", "--input", invalid.to_str().unwrap()],
        None,
    );
    assert_eq!(ok.code, 0, "{}", ok.stderr);
    let settings = snapshot_settings();
    settings.bind(|| {
        insta::assert_snapshot!(
            "behaviour@p-validate",
            format!(
                "{}\n{}",
                transcript("a committed example", &["p", "validate", "--input", "examples/path-04-exploration.path.json"], &ok, Some(home.path())),
                transcript("a step with a dangling parent (the RFC resolves parents within the path; validate does not check it)", &["p", "validate", "--input", "<home>/invalid.path.json"], &bad, Some(home.path())),
            ) + &transcript("a step without the required actor", &["p", "validate", "--input", "<home>/no-actor.path.json"], &missing_actor, Some(home.path()))
        )
    });
}

#[test]
fn query_ancestors_and_dead_ends_on_the_exploration_example() {
    let home = tempfile::tempdir().unwrap();
    let input = examples().join("path-04-exploration.path.json");
    let input = input.to_str().unwrap();
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(input).unwrap()).unwrap();
    let head = doc["paths"][0]["path"]["head"]
        .as_str()
        .unwrap()
        .to_string();
    let anc = run_in(
        home.path(),
        None,
        &[
            "p",
            "query",
            "ancestors",
            "--input",
            input,
            "--step-id",
            &head,
        ],
        None,
    );
    // The CLI page's "Find abandoned approaches" workflow, verbatim.
    let dead = run_in(
        home.path(),
        None,
        &[
            "query",
            "--input",
            input,
            "--no-sync",
            "map(select(.dead_end)) | map(.step.id // .step.step.id)",
        ],
        None,
    );
    let settings = snapshot_settings();
    settings.bind(|| {
        insta::assert_snapshot!(
            "behaviour@query-exploration",
            format!(
                "{}\n{}",
                transcript(
                    "ancestors of the head",
                    &[
                        "p",
                        "query",
                        "ancestors",
                        "--input",
                        "examples/path-04-exploration.path.json",
                        "--step-id",
                        &head
                    ],
                    &anc,
                    Some(home.path())
                ),
                transcript(
                    "the CLI page's dead-end query, ids only",
                    &[
                        "query",
                        "--input",
                        "examples/path-04-exploration.path.json",
                        "--no-sync",
                        "'map(select(.dead_end)) | map(.step.id // .step.step.id)'"
                    ],
                    &dead,
                    Some(home.path())
                )
            )
        )
    });
}

#[test]
fn merge_and_render_dot() {
    let home = tempfile::tempdir().unwrap();
    let a = examples().join("path-01-pr.path.json");
    let b = examples().join("path-04-exploration.path.json");
    let merged = run_in(
        home.path(),
        None,
        &[
            "p",
            "merge",
            a.to_str().unwrap(),
            b.to_str().unwrap(),
            "--title",
            "Release v2",
            "--pretty",
        ],
        None,
    );
    assert_eq!(merged.code, 0, "{}", merged.stderr);
    let dot = run_in(
        home.path(),
        None,
        &["p", "render", "dot", "--input", b.to_str().unwrap()],
        None,
    );
    assert_eq!(dot.code, 0, "{}", dot.stderr);
    let settings = snapshot_settings();
    settings.bind(|| {
        insta::assert_snapshot!(
            "behaviour@p-merge",
            transcript(
                "two committed examples into one graph",
                &[
                    "p",
                    "merge",
                    "examples/path-01-pr.path.json",
                    "examples/path-04-exploration.path.json",
                    "--title",
                    "'Release v2'",
                    "--pretty"
                ],
                &merged,
                Some(home.path())
            )
        );
        insta::assert_snapshot!(
            "behaviour@p-render-dot",
            transcript(
                "the exploration example as DOT",
                &[
                    "p",
                    "render",
                    "dot",
                    "--input",
                    "examples/path-04-exploration.path.json"
                ],
                &dot,
                Some(home.path())
            )
        );
    });
}

/// The CLI page's "Track changes in real time" workflow. The page's example passes
/// `--session /tmp/session.json`; `track init` instead prints the session file it created
/// (`$TMPDIR/track-<stamp>-<pid>.path.jsonl`), which this test uses — see the qualifier record on
/// that example.
#[test]
fn track_workflow_from_the_cli_page() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let mut log = String::new();
    let init = run_in(
        h,
        None,
        &[
            "p",
            "track",
            "init",
            "--file",
            "src/main.rs",
            "--actor",
            "human:alex",
            "--title",
            "Refactoring auth",
        ],
        Some("fn main() {}\n"),
    );
    assert_eq!(init.code, 0, "{}", init.stderr);
    let session = init.stdout.trim().to_string();
    assert!(
        session.ends_with(".path.jsonl"),
        "init prints the session file: {session}"
    );
    log.push_str(&transcript(
        "init (initial content on stdin)",
        &[
            "p",
            "track",
            "init",
            "--file",
            "src/main.rs",
            "--actor",
            "human:alex",
            "--title",
            "'Refactoring auth'",
        ],
        &init,
        Some(h),
    ));
    for (seq, parent, body) in [
        ("1", "0", "fn main() { a() }\n"),
        ("2", "1", "fn main() { a(); b() }\n"),
    ] {
        let r = run_in(
            h,
            None,
            &[
                "p",
                "track",
                "step",
                "--session",
                &session,
                "--seq",
                seq,
                "--parent-seq",
                parent,
            ],
            Some(body),
        );
        assert_eq!(r.code, 0, "{}", r.stderr);
        log.push_str(&transcript(
            "step (current content on stdin)",
            &[
                "p",
                "track",
                "step",
                "--session",
                "<session>",
                "--seq",
                seq,
                "--parent-seq",
                parent,
            ],
            &r,
            Some(h),
        ));
    }
    for args in [
        vec![
            "p",
            "track",
            "annotate",
            "--session",
            &session,
            "--intent",
            "Extract helper",
        ],
        vec![
            "p",
            "track",
            "note",
            "--session",
            &session,
            "--intent",
            "Split main",
        ],
        vec!["p", "track", "visit", "--session", &session, "--seq", "1"],
    ] {
        let r = run_in(h, None, &args, None);
        assert_eq!(r.code, 0, "{}", r.stderr);
        let shown: Vec<&str> = args
            .iter()
            .map(|a| if *a == session { "<session>" } else { a })
            .collect();
        log.push_str(&transcript("", &shown, &r, Some(h)));
    }
    let export = run_in(
        h,
        None,
        &["p", "track", "export", "--session", &session, "--pretty"],
        None,
    );
    assert_eq!(export.code, 0, "{}", export.stderr);
    log.push_str(&transcript(
        "export",
        &["p", "track", "export", "--session", "<session>", "--pretty"],
        &export,
        Some(h),
    ));
    let list = run_in(
        h,
        None,
        &[
            "p",
            "track",
            "list",
            "--session-dir",
            h.join("tmp").to_str().unwrap(),
        ],
        None,
    );
    log.push_str(&transcript(
        "list",
        &["p", "track", "list", "--session-dir", "<home>/tmp"],
        &list,
        Some(h),
    ));
    let out = h.join("done.path.json");
    let close = run_in(
        h,
        None,
        &[
            "p",
            "track",
            "close",
            "--session",
            &session,
            "--output",
            out.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(close.code, 0, "{}", close.stderr);
    log.push_str(&transcript(
        "close",
        &[
            "p",
            "track",
            "close",
            "--session",
            "<session>",
            "--output",
            "<home>/done.path.json",
        ],
        &close,
        Some(h),
    ));
    // The closed document is a valid sealed graph.
    let v = run_in(
        h,
        None,
        &["p", "validate", "--input", out.to_str().unwrap()],
        None,
    );
    assert_eq!(
        v.code, 0,
        "closed track session is not valid: {}{}",
        v.stdout, v.stderr
    );
    let settings = snapshot_settings();
    settings.bind(|| insta::assert_snapshot!("behaviour@p-track", log));
}

/// A deterministic git repo: two branches, fixed authors and dates.
fn git_repo(dir: &Path) {
    let git = |args: &[&str], date: &str| {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Alex")
            .env("GIT_AUTHOR_EMAIL", "alex@example.com")
            .env("GIT_COMMITTER_NAME", "Alex")
            .env("GIT_COMMITTER_EMAIL", "alex@example.com")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            st.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&st.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"], "2026-01-01T00:00:00Z");
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(&["add", "."], "2026-01-01T00:00:00Z");
    git(
        &["commit", "-q", "-m", "Start the file"],
        "2026-01-01T00:00:00Z",
    );
    std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
    git(
        &["commit", "-q", "-am", "Add a second line"],
        "2026-01-01T00:01:00Z",
    );
    git(
        &["checkout", "-q", "-b", "feature/auth"],
        "2026-01-01T00:02:00Z",
    );
    std::fs::write(dir.join("b.txt"), "auth\n").unwrap();
    git(&["add", "."], "2026-01-01T00:02:00Z");
    git(&["commit", "-q", "-m", "Add auth"], "2026-01-01T00:02:00Z");
    git(&["checkout", "-q", "main"], "2026-01-01T00:03:00Z");
}

#[test]
fn import_git_single_and_multi_branch() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("cli_surface: no git on PATH; skipping the git import workflows");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let repo = home.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_repo(&repo);
    // `run_in` clears PATH to /usr/bin:/bin; libgit2 needs no git binary, so this is hermetic.
    let one = run_in(
        home.path(),
        Some(&repo),
        &[
            "p",
            "import",
            "git",
            "--repo",
            ".",
            "--branch",
            "main",
            "--no-cache",
            "--pretty",
        ],
        None,
    );
    let two = run_in(
        home.path(),
        Some(&repo),
        &[
            "p",
            "import",
            "git",
            "--repo",
            ".",
            "--branch",
            "main",
            "--branch",
            "feature/auth",
            "--no-cache",
            "--pretty",
        ],
        None,
    );
    assert_eq!(one.code, 0, "{}", one.stderr);
    assert_eq!(two.code, 0, "{}", two.stderr);
    let g: Value = serde_json::from_str(&two.stdout).unwrap();
    assert_eq!(
        g["paths"].as_array().map(Vec::len),
        Some(2),
        "multi-branch import is a Graph with one Path per branch"
    );
    let settings = snapshot_settings();
    settings.bind(|| {
        insta::assert_snapshot!(
            "behaviour@p-import-git",
            format!(
                "{}\n{}",
                transcript(
                    "the page's \"See what happened in this repo\"",
                    &[
                        "p",
                        "import",
                        "git",
                        "--repo",
                        ".",
                        "--branch",
                        "main",
                        "--no-cache",
                        "--pretty"
                    ],
                    &one,
                    Some(home.path())
                ),
                transcript(
                    "the page's \"Multi-branch import\"",
                    &[
                        "p",
                        "import",
                        "git",
                        "--repo",
                        ".",
                        "--branch",
                        "main",
                        "--branch",
                        "feature/auth",
                        "--no-cache",
                        "--pretty"
                    ],
                    &two,
                    Some(home.path())
                )
            )
        );
    });
}

// ── Per-harness commands over the golden sessions ─────────────────────────

#[derive(Clone, Copy)]
enum H {
    Claude,
    Codex,
    Copilot,
    Pi,
}

impl H {
    fn name(self) -> &'static str {
        match self {
            H::Claude => "claude",
            H::Codex => "codex",
            H::Copilot => "copilot",
            H::Pi => "pi",
        }
    }
    fn fixture(self) -> PathBuf {
        repo_root().join(format!("test-fixtures/{}/convo.jsonl", self.name()))
    }
}

fn lines(input: &str) -> Vec<Value> {
    input
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

/// Place a golden session where `h`'s adapter finds it inside `home` (the same layout
/// `path d baseline` uses); returns the session id and, for project-scoped harnesses, the project.
fn place(h: H, home: &Path) -> (String, Option<String>) {
    let input = std::fs::read_to_string(h.fixture()).unwrap();
    let ls = lines(&input);
    let write = |p: PathBuf| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, &input).unwrap();
    };
    match h {
        H::Codex => {
            let meta = ls.iter().find(|v| v["type"] == "session_meta").unwrap()["payload"].clone();
            let id = meta["id"].as_str().unwrap().to_string();
            let ts = meta["timestamp"].as_str().unwrap();
            let (date, time) = ts.split_once('T').unwrap();
            let time = time.split('.').next().unwrap().replace(':', "-");
            write(
                home.join(".codex/sessions")
                    .join(date.replace('-', "/"))
                    .join(format!("rollout-{date}T{time}-{id}.jsonl")),
            );
            (id, None)
        }
        H::Copilot => {
            let sid = ls
                .iter()
                .find_map(|v| v.get("data")?.get("sessionId").cloned())
                .unwrap();
            let sid = sid.as_str().unwrap().to_string();
            write(
                home.join(".copilot/session-state")
                    .join(&sid)
                    .join("events.jsonl"),
            );
            (sid, None)
        }
        H::Pi => {
            let head = ls.iter().find(|v| v["type"] == "session").unwrap();
            let cwd = head["cwd"].as_str().unwrap().to_string();
            let enc = format!("--{}--", cwd.trim_start_matches('/').replace('/', "-"));
            let ts = head["timestamp"].as_str().unwrap().replace([':', '.'], "-");
            let id = head["id"].as_str().unwrap().to_string();
            write(
                home.join(".pi/agent/sessions")
                    .join(enc)
                    .join(format!("{ts}_{id}.jsonl")),
            );
            (id, Some(cwd))
        }
        H::Claude => {
            let cwd = ls
                .iter()
                .find_map(|v| v.get("cwd").cloned())
                .unwrap()
                .as_str()
                .unwrap()
                .to_string();
            let sid = ls
                .iter()
                .find_map(|v| v.get("sessionId").cloned())
                .unwrap()
                .as_str()
                .unwrap()
                .to_string();
            let slug = cwd.replace(['/', '_', '.'], "-");
            write(
                home.join(".claude/projects")
                    .join(slug)
                    .join(format!("{sid}.jsonl")),
            );
            (sid, Some(cwd))
        }
    }
}

/// What a derived document is, in a few stable numbers.
fn doc_summary(json: &str) -> String {
    let v: Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => return format!("not JSON: {e}"),
    };
    let paths = v["paths"].as_array().cloned().unwrap_or_default();
    let mut out = format!("paths: {}\n", paths.len());
    for p in &paths {
        let steps = p["steps"].as_array().cloned().unwrap_or_default();
        let actors: BTreeSet<String> = steps
            .iter()
            .filter_map(|s| s["step"]["actor"].as_str().map(str::to_string))
            .collect();
        out.push_str(&format!(
            "  kind: {}\n  steps: {}\n  actors: {}\n  head is a step: {}\n",
            p["meta"]["kind"].as_str().unwrap_or("-"),
            steps.len(),
            actors.into_iter().collect::<Vec<_>>().join(", "),
            steps.iter().any(|s| s["step"]["id"] == p["path"]["head"])
        ));
    }
    out
}

#[test]
fn per_harness_list_derive_import_show() {
    let mut out = String::new();
    for h in [H::Claude, H::Codex, H::Copilot, H::Pi] {
        let home = tempfile::tempdir().unwrap();
        let (sid, project) = place(h, home.path());
        let name = h.name();
        out.push_str(&format!("===== {name}\n"));

        let mut list_args = vec!["p", "list", name, "--json"];
        if let Some(p) = &project
            && matches!(h, H::Claude)
        {
            list_args.extend(["--project", p.as_str()]);
        }
        let list = run_in(home.path(), None, &list_args, None);
        out.push_str(&format!(
            "$ path {}\nexit: {}\nlists the session: {}\n",
            list_args
                .join(" ")
                .replace(project.as_deref().unwrap_or("\u{0}"), "<project>"),
            list.code,
            list.stdout.contains(&sid)
        ));

        let mut derive_args = vec!["p", "derive", name];
        if let Some(p) = &project {
            derive_args.extend(["--project", p.as_str()]);
        }
        derive_args.push("--all");
        let derive = run_in(home.path(), None, &derive_args, None);
        out.push_str(&format!(
            "$ path {}\nexit: {}\n{}",
            derive_args
                .join(" ")
                .replace(project.as_deref().unwrap_or("\u{0}"), "<project>"),
            derive.code,
            doc_summary(&derive.stdout)
        ));

        let mut import_args = vec!["p", "import", name];
        if let Some(p) = &project {
            import_args.extend(["--project", p.as_str()]);
        }
        import_args.extend(["--session", sid.as_str(), "--no-cache"]);
        let import = run_in(home.path(), None, &import_args, None);
        out.push_str(&format!(
            "$ path {}\nexit: {}\nsame document as derive: {}\n",
            import_args
                .join(" ")
                .replace(project.as_deref().unwrap_or("\u{0}"), "<project>")
                .replace(&sid, "<session>"),
            import.code,
            doc_summary(&import.stdout) == doc_summary(&derive.stdout)
        ));

        let mut show_args = vec!["show", name];
        if let Some(p) = &project {
            show_args.extend(["--project", p.as_str()]);
        }
        show_args.extend(["--session", sid.as_str()]);
        let show = run_in(home.path(), None, &show_args, None);
        let head: Vec<&str> = show.stdout.lines().take(12).collect();
        out.push_str(&format!(
            "$ path {}\nexit: {}\n--- first lines\n{}\n\n",
            show_args
                .join(" ")
                .replace(project.as_deref().unwrap_or("\u{0}"), "<project>")
                .replace(&sid, "<session>"),
            show.code,
            normalise(&head.join("\n"), Some(home.path()))
        ));
    }
    let settings = snapshot_settings();
    settings.bind(|| insta::assert_snapshot!("behaviour@per-harness", out));
}

#[test]
fn export_project_incept_cache() {
    // One golden document (the Claude session, derived) through every writer the CLI exposes.
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let (_, project) = place(H::Claude, h);
    let project = project.unwrap();
    let derive = run_in(
        h,
        None,
        &["p", "derive", "claude", "--project", &project, "--all"],
        None,
    );
    assert_eq!(derive.code, 0, "{}", derive.stderr);
    let doc = h.join("doc.path.json");
    std::fs::write(&doc, &derive.stdout).unwrap();
    let doc = doc.to_str().unwrap();

    let mut out = String::new();
    for writer in [
        "claude", "gemini", "pi", "codex", "opencode", "copilot", "cursor",
    ] {
        let dest = h.join(format!("out-{writer}"));
        std::fs::create_dir_all(&dest).unwrap();
        let file = dest.join("session.out");
        let r = run_in(
            h,
            None,
            &[
                "p",
                "export",
                writer,
                "--input",
                doc,
                "--output",
                file.to_str().unwrap(),
            ],
            None,
        );
        let written = std::fs::metadata(&file)
            .map(|m| m.len() > 0)
            .unwrap_or(false);
        out.push_str(&format!(
            "$ path p export {writer} --input <doc> --output <file>\nexit: {}\nwrote a non-empty file: {written}\n{}\n",
            r.code,
            if r.code == 0 { String::new() } else { format!("stderr: {}\n", normalise(r.stderr.trim(), Some(h))) }
        ));
    }

    let proj_out = h.join("projected.jsonl");
    let p = run_in(
        h,
        None,
        &[
            "p",
            "project",
            "claude",
            "--input",
            doc,
            "--output",
            proj_out.to_str().unwrap(),
        ],
        None,
    );
    out.push_str(&format!(
        "$ path p project claude --input <doc> --output <file>\nexit: {}\nwrote a non-empty file: {}\n\n",
        p.code,
        std::fs::metadata(&proj_out).map(|m| m.len() > 0).unwrap_or(false)
    ));

    let incept_project = h.join("incept-proj");
    std::fs::create_dir_all(&incept_project).unwrap();
    let i = run_in(
        h,
        None,
        &[
            "p",
            "incept",
            "claude",
            "--input",
            doc,
            "--project",
            incept_project.to_str().unwrap(),
        ],
        None,
    );
    out.push_str(&format!(
        "$ path p incept claude --input <doc> --project <dir>\nexit: {}\n--- stdout\n{}\n--- stderr\n{}\n\n",
        i.code,
        normalise(i.stdout.trim(), Some(h)),
        normalise(i.stderr.trim(), Some(h))
    ));

    // Cache: an import that writes the cache, then ls and rm.
    let imp = run_in(
        h,
        None,
        &["p", "import", "claude", "--project", &project, "--all"],
        None,
    );
    out.push_str(&format!(
        "$ path p import claude --project <project> --all\nexit: {}\n",
        imp.code
    ));
    let ls = run_in(h, None, &["p", "cache", "ls"], None);
    let ids: Vec<String> = ls
        .stdout
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .filter(|t| t.contains("claude"))
        .collect();
    out.push_str(&format!(
        "$ path p cache ls\nexit: {}\nentries naming claude: {}\n",
        ls.code,
        ids.len()
    ));
    if let Some(id) = ids.first() {
        let rm = run_in(h, None, &["p", "cache", "rm", id], None);
        let ls2 = run_in(h, None, &["p", "cache", "ls"], None);
        out.push_str(&format!(
            "$ path p cache rm <id>\nexit: {}\ngone after rm: {}\n",
            rm.code,
            !ls2.stdout.contains(id.as_str())
        ));
    }
    let settings = snapshot_settings();
    settings.bind(|| insta::assert_snapshot!("behaviour@writers-and-cache", out));
}
