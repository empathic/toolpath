use assert_cmd::Command;
use std::path::PathBuf;

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn render_md(example: &str) -> String {
    let output = Command::cargo_bin("path")
        .unwrap()
        .args(["p", "render", "md", "--input"])
        .arg(examples_dir().join(example))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "failed for {example}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

macro_rules! snapshot_test {
    ($name:ident, $file:expr) => {
        #[test]
        fn $name() {
            insta::assert_snapshot!(render_md($file));
        }
    };
}

// Steps (7)
snapshot_test!(render_md_step_01_minimal, "step-01-minimal.json");
snapshot_test!(render_md_step_02_agent, "step-02-agent.json");
snapshot_test!(render_md_step_03_formatter, "step-03-formatter.json");
snapshot_test!(
    render_md_step_04_human_refinement,
    "step-04-human-refinement.json"
);
snapshot_test!(render_md_step_05_dead_end, "step-05-dead-end.json");
snapshot_test!(render_md_step_06_signed, "step-06-signed.json");
snapshot_test!(render_md_step_07_merge, "step-07-merge.json");

// Paths (4)
snapshot_test!(render_md_path_01_pr, "path-01-pr.path.json");
snapshot_test!(
    render_md_path_02_local_session,
    "path-02-local-session.path.json"
);
snapshot_test!(render_md_path_03_signed_pr, "path-03-signed-pr.path.json");
snapshot_test!(
    render_md_path_04_exploration,
    "path-04-exploration.path.json"
);

// Graphs (1)
snapshot_test!(render_md_graph_01_release, "graph-01-release.json");

// Compacted agent-coding-session (synthetic, inline: no examples/ document
// carries a `conversation.compact` step). The transcript renderer must mark
// the boundary at both detail levels instead of silently dropping it.
const COMPACTED_DOC: &str = r#"{
  "graph": {"id": "g-compacted"},
  "paths": [{
    "path": {"id": "sess-compacted", "head": "s3"},
    "meta": {"kind": "https://toolpath.net/kinds/agent-coding-session/v1.2.0", "source": "claude-code", "title": "Compacted session"},
    "steps": [
      {"step": {"id": "s1", "actor": "human:user", "timestamp": "2026-06-20T10:00:00Z"},
       "change": {"claude-code://sess-compacted": {"structural": {"type": "conversation.append", "role": "user", "text": "refactor the auth module"}}}},
      {"step": {"id": "s2", "parents": ["s1"], "actor": "agent:claude", "timestamp": "2026-06-20T10:01:00Z"},
       "change": {"claude-code://sess-compacted": {"structural": {"type": "conversation.append", "role": "assistant", "text": "Reading the auth code first.", "tool_uses": [{"id": "t1", "name": "Read", "input": {"file_path": "auth.rs"}, "category": "file_read", "result": {"content": "fn login() {}", "is_error": false}}]}}}},
      {"step": {"id": "c1", "parents": ["s2"], "actor": "tool:claude-code", "timestamp": "2026-06-20T10:30:00Z"},
       "change": {"claude-code://sess-compacted": {"structural": {"type": "conversation.compact", "trigger": "manual", "pre_tokens": 25450, "summary": "Earlier turns refactored the auth module.", "kept": ["s2"]}}}},
      {"step": {"id": "s3", "parents": ["c1"], "actor": "human:user", "timestamp": "2026-06-20T10:31:00Z"},
       "change": {"claude-code://sess-compacted": {"structural": {"type": "conversation.append", "role": "user", "text": "now add session validation"}}}}
    ]
  }]
}"#;

fn render_md_doc(doc: &str, detail: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("doc.json");
    std::fs::write(&input, doc).unwrap();
    let output = Command::cargo_bin("path")
        .unwrap()
        .args(["p", "render", "md", "--detail", detail, "--input"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn render_md_compacted_session_summary() {
    insta::assert_snapshot!(render_md_doc(COMPACTED_DOC, "summary"));
}

#[test]
fn render_md_compacted_session_full() {
    insta::assert_snapshot!(render_md_doc(COMPACTED_DOC, "full"));
}
