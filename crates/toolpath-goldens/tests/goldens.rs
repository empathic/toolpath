//! The view: every set through the pipeline, one snapshot per stage in
//! `snapshots/<stage>@<set>[@<agent>].snap`.
//!
//! Inputs come from one place each. `test-fixtures/<agent>/convo.*` are the
//! real sessions every crate shares (one per agent; the set is named after
//! the agent). `goldens/<name>/input.*` are the live captures
//! (`claude-session`, from `path goldens capture-live`) and the derived
//! inputs (`claude-compacted`, from `crates/toolpath-claude/tests/fixtures`);
//! a `goldens/<agent>/` directory is the old tool's copy of the fixture and
//! is not read here.
//!
//! `cargo test -p toolpath-goldens` checks the snapshots; a change fails on
//! the first stage that differs, named by its snapshot, with insta's diff
//! naming the line. `cargo insta review` (or `INSTA_UPDATE=always`) accepts
//! a change you meant; `git diff crates/toolpath-goldens/snapshots/` is the
//! review. `INSTA_GLOB_FILTER='codex/*'` runs one set.
//!
//! Stages, in pipeline order: `ir` (the adapter's conversation view),
//! `document` (the toolpath graph), `projection@<set>@<agent>` (what `path p
//! export <agent> -o` writes; Copilot's in canonical form),
//! `round_trip@<set>@<agent>` (what survived reading that projection back),
//! `validate` (`path p validate` over the document and every round trip),
//! `shed` (events, bytes and fields lost at each stage, including the
//! same-format round trip) and `known_defects` (see below). Then one
//! invariant, asserted rather than pinned: the token accounting contract
//! (`toolpath_goldens::accounting`), except where a known defect pins the
//! gap.
//!
//! A diff in `known_defects@<set>.snap` means the defect it pins is FIXED or
//! WORSE; read it. A rename that disappears, a foreign type whose count
//! falls to nothing, a written cwd that becomes the source cwd, an
//! accounting mismatch that goes: fixed, accept the snapshot and retire the
//! defect's record. A new rename, a new foreign type or a higher count: a
//! regression on top of the defect, do not accept.

use std::path::Path;

use serde_json::json;
use toolpath_goldens::accounting;
use toolpath_goldens::leaks::leaks;
use toolpath_goldens::model::{Agent, Finding, Set};
use toolpath_goldens::pipeline::run;

/// The real sessions: `test-fixtures/<agent>/convo.*` at the repo root.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-fixtures");
/// Live captures and derived inputs: `goldens/<name>/input.*` at the repo
/// root.
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
        insta::glob!(FIXTURES, "*/convo.*", |input| {
            check_set(input, &real_home, home.path());
        });
        insta::glob!(GOLDENS, "*/input.*", |input| {
            let name = input
                .parent()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str());
            if name.is_some_and(|n| Agent::ALL.iter().any(|a| a.name() == n)) {
                // The old tool's copy of test-fixtures/<agent>/convo.*.
                return;
            }
            check_set(input, &real_home, home.path());
        });
    });
}

/// One set through the pipeline, its snapshots, and its invariants.
fn check_set(input: &Path, real_home: &str, home: &Path) {
    let set = Set::from_input_file(input).expect("readable input");
    let stages = run(&set, home).unwrap_or_else(|e| panic!("{}: {e}", set.name));

    // Nothing about this machine may enter a golden: not the real home
    // directory, not the scratch directory the adapters read from.
    let bytes = serde_json::to_vec(&stages).expect("stages serialise");
    let needles = [real_home, &home.display().to_string()];
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
        insta::assert_json_snapshot!("validate", stages.validate);
        insta::assert_json_snapshot!("shed", stages.shed);
        insta::assert_json_snapshot!("known_defects", stages.known_defects);
    });

    // The accounting contract: Σ steps == the adapter's total_usage == the
    // extracted total_usage, field by field. A gap a known defect pins is
    // expected (and its snapshot is where a fix shows); any other fails here.
    let gaps = accounting::check(&stages.document, &stages.ir)
        .unwrap_or_else(|e| panic!("{}: accounting: {e}", set.name));
    let pinned: Vec<_> = stages
        .known_defects
        .iter()
        .filter_map(|d| match &d.finding {
            Finding::Accounting { mismatches } => Some(mismatches),
            _ => None,
        })
        .flatten()
        .collect();
    let unpinned: Vec<String> = gaps
        .iter()
        .filter(|g| !pinned.contains(g))
        .map(|g| format!("  {g}"))
        .collect();
    assert!(
        unpinned.is_empty(),
        "{}: the accounting contract does not hold (Σ conversation.append token_usage, provider::to_view total_usage, extract_conversation total_usage):\n{}",
        set.name,
        unpinned.join("\n")
    );
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
