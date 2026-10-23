//! Known defect, pinned: the thread row `path resume` writes into Codex's
//! `state_5.sqlite` names its rollout by absolute path. Codex resumes a
//! thread by appending to `threads.rollout_path`, so a copied `CODEX_HOME`
//! (a cell directory, a backup, another machine's home) still appends to
//! the original's rollout. Seen in the 2026-10-15 resume matrix: the v2
//! codex cell's rollout grew from 33 to 93 lines when a copy was resumed.
//!
//! The input is the `codex` golden, derived and resumed into a temp home
//! whose `state_5.sqlite` has the threads table codex-cli 0.162.1 creates
//! (`test-fixtures/codex/state_5-threads-0.162.1.sql`). The snapshot
//! `codex_home_copy__rollout_path.snap` is the measurement: a fix (a path
//! relative to CODEX_HOME, or one the copy resolves to itself) shows as a
//! diff there; accept it and retire the qualifier record on
//! `register_codex_thread`.

#![cfg(not(target_os = "emscripten"))]

use std::path::Path;

use path_cli::cmd_resume::{RecordingExec, run_with_strategy};
use path_cli::harness::Harness;
use toolpath_golden::golden;

mod support;
use support::*;

const THREADS_SCHEMA: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test-fixtures/codex/state_5-threads-0.162.1.sql"
);

#[test]
fn a_copied_codex_home_still_names_the_original_rollout() {
    let _env = env_lock();
    let home = ScopedHome::new();
    let _path = ScopedPath::with_binary("codex");
    let cwd = tempfile::tempdir().unwrap();

    let codex_home = home.home_dir().join(".codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    rusqlite::Connection::open(codex_home.join("state_5.sqlite"))
        .unwrap()
        .execute_batch(&std::fs::read_to_string(THREADS_SCHEMA).unwrap())
        .unwrap();

    // The codex golden, as `path p derive codex` would give it.
    let rollout = cwd.path().join("rollout.jsonl");
    std::fs::write(&rollout, golden("codex").input()).unwrap();
    let session = toolpath_codex::RolloutReader::read_session(&rollout).unwrap();
    let derived = toolpath_codex::derive::derive_path(
        &session,
        &toolpath_codex::derive::DeriveConfig { project_path: None },
    );
    let doc = write_path_to_temp(cwd.path(), derived);

    let recorder = RecordingExec::default();
    run_with_strategy(args_explicit(doc, cwd.path(), Harness::Codex), &recorder).unwrap();
    let thread_id = recorder.captured().args[1].clone();

    // Copy CODEX_HOME the way a cell or a backup is made.
    let copy = home.home_dir().join("codex-copy");
    copy_dir(&codex_home, &copy);

    let rollout_path = |db: &Path| -> String {
        rusqlite::Connection::open(db)
            .unwrap()
            .query_row(
                "SELECT rollout_path FROM threads WHERE id = ?1",
                [&thread_id],
                |r| r.get(0),
            )
            .unwrap()
    };
    let original = rollout_path(&codex_home.join("state_5.sqlite"));
    let in_copy = rollout_path(&copy.join("state_5.sqlite"));
    let place = |p: &str| {
        if p.starts_with(&copy.display().to_string()) {
            "the copy"
        } else if p.starts_with(&codex_home.display().to_string()) {
            "the original CODEX_HOME"
        } else {
            "elsewhere"
        }
    };

    let measured = serde_json::json!({
        "original_row_is_absolute": Path::new(&original).is_absolute(),
        "copied_row_points_into": place(&in_copy),
        "copied_row_equals_original": in_copy == original,
    });
    insta::assert_snapshot!(
        "rollout_path",
        serde_json::to_string_pretty(&measured).unwrap()
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
