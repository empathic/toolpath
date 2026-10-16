//! The view: every `goldens/<set>/input.jsonl` through the pipeline, one
//! snapshot per stage in `snapshots/<stage>@<set>[@<agent>].snap`.
//!
//! `cargo test -p toolpath-goldens` checks the snapshots; a change fails on
//! the first stage that differs, named by its snapshot, with insta's diff
//! naming the line. `cargo insta review` (or `INSTA_UPDATE=always`) accepts
//! a change you meant; `git diff crates/toolpath-goldens/snapshots/` is the
//! review. `INSTA_GLOB_FILTER='codex/*'` runs one set.
//!
//! Stages, in pipeline order: `ir` (the adapter's conversation view),
//! `document` (the toolpath graph), `projection@<set>@<agent>` (the JSONL
//! `path p export <agent>` writes; Copilot's in canonical form),
//! `round_trip@<set>@<agent>` (what survived reading that projection back),
//! `shed` (events, bytes and fields lost at each stage, including the
//! same-format round trip) and `known_defects` (see below).
//!
//! A diff in `known_defects@<set>.snap` means the defect it pins is FIXED or
//! WORSE; read it. A rename that disappears, a foreign type whose count
//! falls to nothing, a written cwd that becomes the source cwd: fixed, accept
//! the snapshot and retire the defect's record. A new rename, a new foreign
//! type or a higher count: a regression on top of the defect, do not accept.

use std::path::Path;

use serde_json::json;
use toolpath_goldens::leaks::leaks;
use toolpath_goldens::model::Set;
use toolpath_goldens::pipeline::run;

/// Where the sets live: `goldens/<name>/input.jsonl` at the repo root.
const GOLDENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../goldens");

#[test]
fn every_set_matches_its_snapshots() {
    let real_home = std::env::var("HOME").expect("HOME is set");
    // The test owns HOME: every store an adapter might resolve points into a
    // directory that is empty, deleted afterwards, and a leak needle below.
    let home = tempfile::tempdir().expect("temp HOME");
    point_every_store_at(home.path());

    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("../snapshots");
    settings.set_prepend_module_to_snapshot(false);
    // The IR carries hash maps; without this their key order changes per
    // process and `ir` would never be stable.
    settings.set_sort_maps(true);
    settings.bind(|| {
        insta::glob!(GOLDENS, "*/input.jsonl", |input| {
            let set = Set::from_input_file(input).expect("readable input.jsonl");
            let stages = run(&set, home.path()).unwrap_or_else(|e| panic!("{}: {e}", set.name));

            // Nothing about this machine may enter a golden: not the real
            // home directory, not the scratch directory the adapters read
            // from.
            let bytes = serde_json::to_vec(&stages).expect("stages serialise");
            let needles = [real_home.as_str(), &home.path().display().to_string()];
            let found = leaks(&bytes, &needles);
            assert!(found.is_empty(), "{}: output mentions {found:?}", set.name);

            // `<stage>@<set>.snap`, with the input's identity in the header.
            let mut per_set = insta::Settings::clone_current();
            per_set.set_snapshot_suffix(&set.name);
            per_set.set_info(&json!({
                "agent": set.agent,
                "adapter": set.agent.adapter_name(),
                "input_sha256": set.input_sha256(),
            }));
            per_set.bind(|| {
                insta::assert_json_snapshot!("ir", stages.ir);
                insta::assert_json_snapshot!("document", stages.document);
                for (agent, text) in &stages.projections {
                    let mut per_agent = insta::Settings::clone_current();
                    per_agent.set_snapshot_suffix(format!("{}@{agent}", set.name));
                    per_agent.bind(|| {
                        insta::assert_snapshot!("projection", text);
                        insta::assert_json_snapshot!("round_trip", stages.round_trips[agent]);
                    });
                }
                insta::assert_json_snapshot!("shed", stages.shed);
                insta::assert_json_snapshot!("known_defects", stages.known_defects);
            });
        });
    });
}

/// HOME and every per-agent store the adapters resolve, inside `home`. Set
/// once, before any adapter runs, in a test binary with this one test.
fn point_every_store_at(home: &Path) {
    let vars = [
        ("HOME", home.to_path_buf()),
        ("TMPDIR", home.to_path_buf()),
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_DATA_HOME", home.join(".local/share")),
        ("XDG_STATE_HOME", home.join(".local/state")),
        ("XDG_CACHE_HOME", home.join(".cache")),
        ("CLAUDE_CONFIG_DIR", home.join(".claude")),
        ("CODEX_HOME", home.join(".codex")),
        ("COPILOT_HOME", home.join(".copilot")),
        ("TOOLPATH_CONFIG_DIR", home.join(".toolpath")),
    ];
    for (k, v) in vars {
        // SAFETY: the test binary has one test and no other thread is
        // running yet; the pipeline itself never reads the environment.
        unsafe { std::env::set_var(k, v) };
    }
}
