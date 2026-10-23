//! "path query runs one jq filter across every session on the machine,
//! whichever agent wrote it" (toolpath.net), and the site's own example.
//!
//! Two pins, both through the real `path` binary in a temp HOME:
//!
//! * `query@site-example`: the cache holds one document per golden set (the
//!   pinned `document@<set>` snapshots: all seven harnesses, nine sessions),
//!   and the site's exact filter runs over all of them. Every harness's model
//!   must show up as its own `agent:` group.
//! * `query@cache-sync`: the site's other step, `path p cache sync`, over
//!   real session files placed where each harness keeps them (Claude Code,
//!   Codex, Copilot, pi, Gemini CLI); the sync is run twice and must be
//!   incremental, then the same filter runs over what it ingested. opencode
//!   and Cursor keep sessions in SQLite stores this test does not build, so
//!   their sync is not covered here (the first pin covers their documents).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The example on toolpath.net, verbatim.
const SITE_FILTER: &str = "group_by(.step.actor) | map({actor: .[0].step.actor, output_tokens: ([.[].change[]?.structural.token_usage.output_tokens // 0] | add)})";

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).expect("json")
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn path_cmd(home: &Path) -> Command {
    let mut c = Command::cargo_bin("path").unwrap();
    c.env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("TOOLPATH_CONFIG_DIR", home.join(".toolpath"))
        .current_dir(home);
    c
}

fn query(home: &Path) -> Value {
    let out = path_cmd(home)
        .args(["query", SITE_FILTER])
        .output()
        .expect("run path query");
    assert!(
        out.status.success(),
        "path query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("path query prints JSON")
}

fn snapshot_body(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    let mut parts = text.splitn(3, "---\n");
    parts.next();
    parts.next();
    parts.next().unwrap().to_string()
}

fn agent_groups(result: &Value) -> BTreeSet<String> {
    result
        .as_array()
        .expect("the site filter returns an array")
        .iter()
        .filter_map(|g| g["actor"].as_str())
        .filter(|a| a.starts_with("agent:"))
        .map(str::to_string)
        .collect()
}

#[test]
fn site_example_over_every_harness() {
    let home = tempfile::tempdir().unwrap();
    let docs = home.path().join(".toolpath/documents");
    std::fs::create_dir_all(&docs).unwrap();
    let snaps = root().join("crates/toolpath-goldens/snapshots");
    let mut sources = BTreeSet::new();
    let mut expected_agents = BTreeSet::new();
    for entry in std::fs::read_dir(&snaps).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(set) = name
            .strip_prefix("document@")
            .and_then(|n| n.strip_suffix(".snap"))
        else {
            continue;
        };
        let body = snapshot_body(&entry.path());
        let doc: Value = serde_json::from_str(&body).unwrap();
        for p in doc["paths"].as_array().unwrap() {
            sources.insert(p["meta"]["source"].as_str().unwrap_or("").to_string());
            for s in p["steps"].as_array().unwrap() {
                if let Some(a) = s["step"]["actor"]
                    .as_str()
                    .filter(|a| a.starts_with("agent:"))
                {
                    expected_agents.insert(a.to_string());
                }
            }
        }
        std::fs::write(docs.join(format!("{set}.json")), body).unwrap();
    }
    assert_eq!(
        sources.len(),
        7,
        "all seven harnesses are in the cache: {sources:?}"
    );

    let result = query(home.path());
    assert_eq!(
        agent_groups(&result),
        expected_agents,
        "every harness's model is its own group"
    );
    insta::assert_snapshot!("query@site-example", pretty(&result));
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// The first JSON line of `input` for which `f` returns something.
fn pick(input: &[u8], f: impl Fn(&Value) -> Option<Value>) -> Value {
    String::from_utf8_lossy(input)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| f(&v))
        .expect("fixture carries the identity its harness needs")
}

fn str_of(v: &Value) -> String {
    v.as_str().expect("a string").to_string()
}

/// Real session files, where each harness keeps them (the goldens' layout).
fn place_sessions(home: &Path) {
    let read = |rel: &str| std::fs::read(root().join(rel)).unwrap();

    let claude = read("goldens/claude-session/input.jsonl");
    let cwd = str_of(&pick(&claude, |v| v.get("cwd").cloned()));
    let sid = str_of(&pick(&claude, |v| v.get("sessionId").cloned()));
    let slug = cwd.replace(['/', '_', '.'], "-");
    write(
        &home
            .join(".claude/projects")
            .join(slug)
            .join(format!("{sid}.jsonl")),
        &claude,
    );

    let codex = read("test-fixtures/codex/convo.jsonl");
    let meta = pick(&codex, |v| {
        (v["type"] == "session_meta").then(|| v["payload"].clone())
    });
    let (id, ts) = (str_of(&meta["id"]), str_of(&meta["timestamp"]));
    let (date, time) = ts.split_once('T').unwrap();
    let time = time.split('.').next().unwrap().replace(':', "-");
    write(
        &home
            .join(".codex/sessions")
            .join(date.replace('-', "/"))
            .join(format!("rollout-{date}T{time}-{id}.jsonl")),
        &codex,
    );

    let copilot = read("test-fixtures/copilot/convo.jsonl");
    let sid = str_of(&pick(&copilot, |v| {
        v.get("data")?.get("sessionId").cloned()
    }));
    write(
        &home
            .join(".copilot/session-state")
            .join(sid)
            .join("events.jsonl"),
        &copilot,
    );

    let pi = read("test-fixtures/pi/convo.jsonl");
    let head = pick(&pi, |v| (v["type"] == "session").then(|| v.clone()));
    let cwd = str_of(&head["cwd"]);
    let enc = format!("--{}--", cwd.trim_start_matches('/').replace('/', "-"));
    let ts = str_of(&head["timestamp"]).replace([':', '.'], "-");
    write(
        &home
            .join(".pi/agent/sessions")
            .join(enc)
            .join(format!("{ts}_{}.jsonl", str_of(&head["id"]))),
        &pi,
    );

    // Gemini CLI 0.47: tmp/<sha256(project)>/chats/session-*.jsonl, found by
    // the slot's .project_root marker.
    let gemini = read("test-fixtures/gemini/convo.jsonl");
    let header = pick(&gemini, |v| v.get("sessionId").map(|_| v.clone()));
    let project = "/work/gemini-project";
    let slot = hex::encode(Sha256::digest(project.as_bytes()));
    let dir = home.join(".gemini/tmp").join(slot);
    let start = str_of(&header["startTime"])[..16].replace(':', "-");
    let sid = str_of(&header["sessionId"]);
    write(&dir.join(".project_root"), project.as_bytes());
    write(
        &dir.join("chats")
            .join(format!("session-{start}-{}.jsonl", &sid[..8])),
        &gemini,
    );
}

fn sync(home: &Path) -> String {
    let out = path_cmd(home)
        .args(["p", "cache", "sync"])
        .output()
        .expect("run path p cache sync");
    assert!(
        out.status.success(),
        "cache sync failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The per-harness summary lines, wherever they are printed.
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| l.contains(" new, "))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    lines.sort();
    lines.join("\n")
}

#[test]
fn cache_sync_then_query_over_real_session_files() {
    let home = tempfile::tempdir().unwrap();
    place_sessions(home.path());
    let first = sync(home.path());
    let second = sync(home.path());
    for harness in ["claude", "codex", "copilot", "pi", "gemini"] {
        assert!(
            first
                .lines()
                .any(|l| l.starts_with(&format!("{harness} 1 new"))),
            "{harness} ingested on the first sync:\n{first}"
        );
        assert!(
            second
                .lines()
                .any(|l| l.starts_with(&format!("{harness} 0 new, 0 updated, 1 unchanged"))),
            "{harness} unchanged on the second sync (incremental):\n{second}"
        );
    }
    let result = query(home.path());
    assert_eq!(
        agent_groups(&result).len(),
        5,
        "one model per synced harness: {result}"
    );
    insta::assert_snapshot!(
        "query@cache-sync",
        pretty(&serde_json::json!({ "first_sync": first, "second_sync": second, "query": result }))
    );
}
