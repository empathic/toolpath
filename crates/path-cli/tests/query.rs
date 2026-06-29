//! Integration tests for `path query` and `path kind`.
//!
//! Each invocation runs against a throwaway `$TOOLPATH_CONFIG_DIR` sandbox so
//! the cache is hermetic. Fixtures: one `agent-coding-session` doc (a kind the
//! binary bundles a spec for) and one generic git-PR doc (a kind it does not),
//! both of which must remain queryable.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;

fn cmd() -> Command {
    Command::cargo_bin("path").unwrap()
}

/// Write `json` into `<cfg>/documents/<id>.json`, creating the dir.
fn seed(cfg: &Path, id: &str, json: &str) {
    let docs = cfg.join("documents");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join(format!("{id}.json")), json).unwrap();
}

/// An agent-coding-session graph: s1(user, "RefCell") → s2(assistant, 60k
/// input tokens, failed Bash, touches cmd_resume.rs) → s3(head); s2a is a
/// dead-end branch off s1.
const CLAUDE_DOC: &str = r#"{
  "graph": {"id": "g1"},
  "paths": [{
    "path": {"id": "sess-1", "base": {"uri": "file:///work/repo"}, "head": "s3"},
    "meta": {"kind": "https://toolpath.net/kinds/agent-coding-session/v1.1.0", "source": "claude", "title": "Add path query"},
    "steps": [
      {"step": {"id": "s1", "actor": "human:alex", "timestamp": "2026-06-20T10:00:00Z"},
       "change": {"agent://claude/s1": {"structural": {"type": "conversation.append", "role": "user", "text": "use a RefCell here"}}}},
      {"step": {"id": "s2", "parents": ["s1"], "actor": "agent:claude-code", "timestamp": "2026-06-20T10:01:00Z"},
       "change": {"src/cmd_resume.rs": {"structural": {"type": "conversation.append", "role": "assistant", "text": "done", "token_usage": {"input_tokens": 60000, "output_tokens": 412}, "tool_uses": [{"name": "Bash", "result": {"is_error": true}}]}}}},
      {"step": {"id": "s2a", "parents": ["s1"], "actor": "agent:claude-code", "timestamp": "2026-06-20T10:02:00Z"},
       "change": {"src/dead.rs": {"raw": "@@ dead @@"}}},
      {"step": {"id": "s3", "parents": ["s2"], "actor": "human:alex", "timestamp": "2026-06-20T10:03:00Z"},
       "change": {"src/cmd_resume.rs": {"raw": "@@ final @@"}}}
    ]
  }]
}"#;

/// A generic git-PR graph with no `meta.kind` — a kind the binary bundles no
/// spec for. Must still load and be queryable.
const GIT_DOC: &str = r#"{
  "graph": {"id": "g2"},
  "paths": [{
    "path": {"id": "pr-42", "base": {"uri": "github:org/repo", "ref": "abc"}, "head": "c2"},
    "steps": [
      {"step": {"id": "c1", "actor": "human:bob", "timestamp": "2026-06-21T09:00:00Z"},
       "change": {"README.md": {"raw": "@@ -1 +1 @@\n-old\n+new"}}},
      {"step": {"id": "c2", "parents": ["c1"], "actor": "human:bob", "timestamp": "2026-06-21T09:05:00Z"},
       "change": {"src/cmd_resume.rs": {"raw": "@@ pr change @@"}}}
    ]
  }]
}"#;

/// A full sandbox with both fixtures seeded.
fn sandbox() -> tempfile::TempDir {
    let cfg = tempfile::tempdir().unwrap();
    seed(cfg.path(), "claude-sess1", CLAUDE_DOC);
    seed(cfg.path(), "git-pr42", GIT_DOC);
    cfg
}

fn query<'a>(cfg: &Path, args: impl IntoIterator<Item = &'a str>) -> assert_cmd::assert::Assert {
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg)
        .arg("query")
        .args(args)
        .assert()
}

// ── The four motivating examples ─────────────────────────────────────

#[test]
fn steps_mentioning_refcell() {
    let cfg = sandbox();
    query(
        cfg.path(),
        [r#"[.[] | select(any(.. | strings; test("RefCell"))) | .step.id]"#],
    )
    .success()
    .stdout(predicate::str::contains("s1"))
    .stdout(predicate::str::contains("s2").not());
}

#[test]
fn steps_touching_a_file_across_sessions() {
    // cmd_resume.rs is touched in both the claude doc (s2, s3) and the git doc
    // (c2): the dedup-by-cache_id rollup returns both sessions.
    let cfg = sandbox();
    query(
        cfg.path(),
        [r#"[.[] | select(any(.change | keys[]; endswith("cmd_resume.rs"))) | .cache_id] | unique"#],
    )
    .success()
    .stdout(predicate::str::contains("claude-sess1"))
    .stdout(predicate::str::contains("git-pr42"));
}

#[test]
fn turns_over_50k_input_tokens() {
    let cfg = sandbox();
    query(
        cfg.path(),
        [r#"[.[] | select(any(.change[].structural.token_usage; .input_tokens > 50000)) | .step.id]"#],
    )
    .success()
    .stdout(predicate::str::contains("s2"));
}

#[test]
fn failed_bash_in_claude_sessions() {
    let cfg = sandbox();
    query(
        cfg.path(),
        [
            "--source",
            "claude",
            r#"[.[] | select(any(.change[].structural.tool_uses[]?; .name == "Bash" and .result.is_error)) | .step.id]"#,
        ],
    )
    .success()
    .stdout(predicate::str::contains("s2"));
}

// ── Rollups ──────────────────────────────────────────────────────────

#[test]
fn top_n_by_tokens() {
    let cfg = sandbox();
    query(
        cfg.path(),
        [r#"map({step: .step.id, tokens: ([.change[].structural.token_usage // empty | (.input_tokens//0)+(.output_tokens//0)] | add // 0)}) | sort_by(-.tokens) | .[0].step"#],
    )
    .success()
    .stdout(predicate::str::contains("s2"));
}

#[test]
fn step_count_per_source() {
    let cfg = sandbox();
    // The git doc has no source; group_by puts it under null.
    query(
        cfg.path(),
        ["group_by(.path.meta.source) | map({source: .[0].path.meta.source, steps: length}) | length"],
    )
    .success()
    .stdout(predicate::str::starts_with("2"));
}

// ── Dead ends (the former `dead-ends` subcommand, now a jaq form) ─────

#[test]
fn dead_ends_as_jaq_form() {
    let cfg = sandbox();
    query(cfg.path(), ["[.[] | select(.dead_end) | .step.id]"])
        .success()
        .stdout(predicate::str::contains("s2a"))
        .stdout(predicate::str::contains("s1").not());
}

// ── File selection ───────────────────────────────────────────────────

#[test]
fn source_selects_by_prefix() {
    let cfg = sandbox();
    // Only the claude doc's 4 steps.
    query(cfg.path(), ["--source", "claude", "length"])
        .success()
        .stdout(predicate::str::starts_with("4"));
}

#[test]
fn id_selects_one_document() {
    let cfg = sandbox();
    query(cfg.path(), ["--id", "git-pr42", "length"])
        .success()
        .stdout(predicate::str::starts_with("2"));
}

#[test]
fn whole_cache_loads_both_docs() {
    let cfg = sandbox();
    query(cfg.path(), ["length"])
        .success()
        .stdout(predicate::str::starts_with("6"));
}

// ── Content scoping ──────────────────────────────────────────────────

#[test]
fn kind_prefix_match_v1_keeps_session() {
    let cfg = sandbox();
    query(cfg.path(), ["--kind", "agent-coding-session/v1", "length"])
        .success()
        .stdout(predicate::str::starts_with("4"));
}

#[test]
fn kind_v2_matches_nothing() {
    let cfg = sandbox();
    query(cfg.path(), ["--kind", "agent-coding-session/v2", "length"])
        .success()
        .stdout(predicate::str::starts_with("0"));
}

// ── Robustness ───────────────────────────────────────────────────────

#[test]
fn malformed_doc_is_skipped_with_warning() {
    let cfg = sandbox();
    seed(cfg.path(), "claude-broken", "{ not json");
    // The good docs still load (6 steps); the broken one warns on stderr.
    query(cfg.path(), ["length"])
        .success()
        .stdout(predicate::str::starts_with("6"))
        .stderr(predicate::str::contains("warning: skipping"));
}

#[test]
fn empty_result_exits_zero() {
    let cfg = sandbox();
    query(cfg.path(), [r#"map(select(.step.id == "nope"))"#])
        .success()
        .stdout(predicate::str::contains("[]"));
}

#[test]
fn deterministic_step_order_within_a_path() {
    let cfg = sandbox();
    // Document order is s1, s2, s2a, s3 — stable across runs.
    query(cfg.path(), ["--id", "claude-sess1", "[.[].step.id]", "-c"])
        .success()
        .stdout(predicate::str::contains(r#"["s1","s2","s2a","s3"]"#));
}

#[test]
fn compact_json_when_piped() {
    let cfg = sandbox();
    // assert_cmd's stdout is not a TTY, so output is compact (no pretty
    // newlines inside the object).
    query(cfg.path(), ["--id", "git-pr42", ".[0] | {id: .step.id}"])
        .success()
        .stdout(predicate::str::diff("{\"id\":\"c1\"}\n"));
}

#[test]
fn invalid_filter_exits_one() {
    let cfg = sandbox();
    query(cfg.path(), ["map(select("])
        .failure()
        .stderr(predicate::str::contains("jq filter"));
}

#[test]
fn raw_prints_strings_unquoted() {
    let cfg = sandbox();
    // `-r` on a stream of strings: each line is the raw value, no JSON quotes.
    query(
        cfg.path(),
        ["--id", "git-pr42", "[.[].step.id] | sort | .[]", "-r"],
    )
    .success()
    .stdout(predicate::str::diff("c1\nc2\n"));
}

#[test]
fn raw_leaves_non_strings_as_json() {
    let cfg = sandbox();
    // jq parity: `-r` only affects string outputs; numbers/objects stay JSON.
    query(cfg.path(), ["--id", "git-pr42", "length", "-r"])
        .success()
        .stdout(predicate::str::diff("2\n"));
    query(
        cfg.path(),
        ["--id", "git-pr42", ".[0] | {id: .step.id}", "-r"],
    )
    .success()
    .stdout(predicate::str::diff("{\"id\":\"c1\"}\n"));
}

#[test]
fn raw_unescapes_string_content() {
    let cfg = sandbox();
    // A string containing a newline prints with a real newline under -r,
    // not the two-character escape `\n`. (The filter yields a literal, so the
    // scoped docs are irrelevant here.)
    query(cfg.path(), [r#"["one\ntwo"] | .[]"#, "-r"])
        .success()
        .stdout(predicate::str::diff("one\ntwo\n"));
}

// ── path kind ────────────────────────────────────────────────────────

#[test]
fn kind_lists_bundled_kinds() {
    cmd()
        .arg("kind")
        .assert()
        .success()
        .stdout(predicate::str::contains("agent-coding-session"))
        .stdout(predicate::str::contains("v1.1.0"));
}

#[test]
fn kind_prints_newest_schema() {
    cmd()
        .args(["kind", "agent-coding-session"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "kinds/agent-coding-session/v1.1.0/schema.json",
        ));
}

#[test]
fn kind_pins_specific_version() {
    cmd()
        .args(["kind", "agent-coding-session/v1.0.0"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "kinds/agent-coding-session/v1.0.0/schema.json",
        ));
}

#[test]
fn kind_unknown_errors() {
    cmd()
        .args(["kind", "no-such-kind"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Bundled kinds"));
}
