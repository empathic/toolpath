use assert_cmd::Command;
use std::path::PathBuf;

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn render_md(example: &str) -> String {
    #[allow(deprecated)]
    let output = Command::cargo_bin("path")
        .unwrap()
        .args(["render", "md", "--input"])
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
