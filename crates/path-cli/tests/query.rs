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

// ── Dead ends ────────────────────────────────────────────────────────

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

// ── Streaming executor (no flag; must match whole-array semantics) ────

/// Two docs with token-bearing steps whose maxima live in *different* files,
/// so a correct top-N must merge across files, not pick a per-file winner.
fn token_sandbox() -> tempfile::TempDir {
    let doc = |id: &str, tokens: u64| {
        format!(
            r#"{{"graph":{{"id":"g"}},"paths":[{{"path":{{"id":"{id}","head":"s1"}},
              "meta":{{"kind":"https://toolpath.net/kinds/agent-coding-session/v1.0.0","source":"claude"}},
              "steps":[{{"step":{{"id":"s1","actor":"agent:x","timestamp":"2026-06-20T10:00:00Z"}},
                "change":{{"c://{id}/s1":{{"structural":{{"type":"conversation.append","role":"assistant","text":"x","token_usage":{{"input_tokens":{tokens}}}}}}}}}}}]}}]}}"#
        )
    };
    let cfg = tempfile::tempdir().unwrap();
    seed(cfg.path(), "claude-lo", &doc("lo", 100));
    seed(cfg.path(), "claude-hi", &doc("hi", 900));
    seed(cfg.path(), "claude-mid", &doc("mid", 500));
    cfg
}

#[test]
fn top_n_merges_across_files() {
    let cfg = token_sandbox();
    // Global top-1 by input tokens is the `hi` doc (900), even though it lives
    // in a different file than `lo`/`mid`. Streamed decompose must find it.
    query(
        cfg.path(),
        ["map({t: .change[].structural.token_usage.input_tokens}) | sort_by(-.t) | .[0].t"],
    )
    .success()
    .stdout(predicate::str::starts_with("900"));
}

#[test]
fn scalar_reduction_sums_across_files() {
    let cfg = token_sandbox();
    // 3 docs, 1 step each.
    query(cfg.path(), ["length"])
        .success()
        .stdout(predicate::str::starts_with("3"));
    // Sum of input tokens across all files: 100 + 900 + 500 = 1500.
    query(
        cfg.path(),
        ["[.[].change[].structural.token_usage.input_tokens] | add"],
    )
    .success()
    .stdout(predicate::str::starts_with("1500"));
}

#[test]
fn explain_env_reports_the_plan() {
    let cfg = token_sandbox();
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .env("TOOLPATH_QUERY_EXPLAIN", "1")
        .args(["query", "map(.step.id) | sort_by(.) | .[:2]"])
        .assert()
        .success()
        .stderr(predicate::str::contains("decompose"));
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .env("TOOLPATH_QUERY_EXPLAIN", "1")
        .args(["query", ".[] | select(.dead_end)"])
        .assert()
        .success()
        .stderr(predicate::str::contains("stream per file"));
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .env("TOOLPATH_QUERY_EXPLAIN", "1")
        .args(["query", "group_by(.cache_id)"])
        .assert()
        .success()
        .stderr(predicate::str::contains("slurp"));
}

// ── Robustness ───────────────────────────────────────────────────────

#[test]
fn malformed_doc_is_skipped_with_warning() {
    let cfg = sandbox();
    seed(cfg.path(), "claude-broken", "{ not json");
    // A corrupt file encountered during the whole-cache *scan* is skipped with
    // a warning (not an error): the good docs still load (6 steps).
    query(cfg.path(), ["length"])
        .success()
        .stdout(predicate::str::starts_with("6"))
        .stderr(predicate::str::contains("warning: skipping"));
}

#[test]
fn explicit_missing_input_errors() {
    // But an explicitly named `--input` that won't read is a hard error, not a
    // silent skip returning a wrong answer.
    let cfg = sandbox();
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["query", "--input", "/no/such/file.json", "length"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("read"));
}

#[test]
fn missing_id_errors() {
    let cfg = sandbox();
    query(cfg.path(), ["--id", "does-not-exist", "length"])
        .failure()
        .stderr(predicate::str::contains("no cached document with id"));
}

#[test]
fn id_intersected_with_source_is_empty_not_an_error() {
    // claude-sess1 exists, so `--source git --id claude-sess1` is an empty
    // intersection — not a false "no cached document" report.
    let cfg = sandbox();
    query(
        cfg.path(),
        ["--source", "git", "--id", "claude-sess1", "length"],
    )
    .success()
    .stdout(predicate::str::starts_with("0"));
}

#[test]
fn same_basename_inputs_keep_distinct_cache_ids() {
    let cfg = sandbox();
    let d1 = cfg.path().join("proj1");
    let d2 = cfg.path().join("proj2");
    std::fs::create_dir_all(&d1).unwrap();
    std::fs::create_dir_all(&d2).unwrap();
    std::fs::write(d1.join("doc.json"), CLAUDE_DOC).unwrap();
    std::fs::write(d2.join("doc.json"), GIT_DOC).unwrap();
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .arg("query")
        .arg("--input")
        .arg(d1.join("doc.json"))
        .arg("--input")
        .arg(d2.join("doc.json"))
        .arg("[.[].cache_id] | unique | length")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("2"));
}

#[test]
fn explicit_corrupt_input_errors() {
    let cfg = sandbox();
    let bad = cfg.path().join("bad.json");
    std::fs::write(&bad, "{ not json").unwrap();
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .arg("query")
        .arg("--input")
        .arg(&bad)
        .arg("length")
        .assert()
        .failure();
}

#[test]
fn stdin_accepts_jsonl() {
    // Stdin must accept the `.path.jsonl` form too, not only
    // canonical JSON (a file `--input` already handles both by extension).
    let jsonl = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/path-04-exploration.path.jsonl");
    let file_out = cmd()
        .arg("query")
        .arg("--input")
        .arg(&jsonl)
        .arg("length")
        .assert()
        .success();
    let file_stdout = String::from_utf8(file_out.get_output().stdout.clone()).unwrap();
    cmd()
        .args(["query", "--input", "-", "length"])
        .write_stdin(std::fs::read(&jsonl).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::diff(file_stdout));
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
        .stdout(predicate::str::contains("v1.1.0"))
        .stdout(predicate::str::contains("v1.2.0"));
}

#[test]
fn kind_prints_newest_schema() {
    cmd()
        .args(["kind", "agent-coding-session"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "kinds/agent-coding-session/v1.2.0/schema.json",
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
