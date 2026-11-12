//! The view: every set through the pipeline, one snapshot per stage in
//! `snapshots/<stage>@<set>[@<agent>].snap`.
//!
//! Inputs come from one place each. `test-fixtures/<agent>/convo.*` are the
//! real sessions every crate shares (one per agent; the set is named after
//! the agent). `goldens/<name>/input.*` are the live captures
//! (`claude-session`, from `path d baseline capture-live`) and the derived
//! inputs (`claude-compacted`, from `crates/toolpath-claude/tests/fixtures`);
//! a `goldens/<agent>/` directory is the old tool's copy of the fixture and
//! is not read here.
//!
//! `cargo test -p toolpath-goldens` checks the snapshots. Each recording is
//! its own test (`set_claude`, `set_codex`, … `set_claude_session`), so a
//! break names the recording that broke and the others stay green; inside a
//! test, a change fails on the first stage that differs, named by its
//! snapshot, with insta's diff naming the line. `cargo insta review` (or
//! `INSTA_UPDATE=always`) accepts a change you meant; `git diff
//! crates/toolpath-goldens/snapshots/` is the review. `cargo test -p
//! toolpath-goldens --test goldens set_codex` runs one recording.
//! `every_recording_has_a_test` fails when a recording is added on disk
//! without a test here.
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
use std::sync::OnceLock;

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

/// One test per recording. The path is relative to the repo root.
macro_rules! recording {
    ($test:ident, $path:literal) => {
        #[test]
        fn $test() {
            check_recording($path);
        }
    };
}

recording!(set_claude, "test-fixtures/claude/convo.jsonl");
recording!(set_codex, "test-fixtures/codex/convo.jsonl");
recording!(set_copilot, "test-fixtures/copilot/convo.jsonl");
recording!(set_cursor, "test-fixtures/cursor/convo.json");
recording!(set_gemini, "test-fixtures/gemini/convo.jsonl");
recording!(set_opencode, "test-fixtures/opencode/convo.json");
recording!(set_pi, "test-fixtures/pi/convo.jsonl");
recording!(set_claude_session, "goldens/claude-session/input.jsonl");
recording!(set_claude_compacted, "goldens/claude-compacted/input.jsonl");
// Compactions forced in the agent by Ben Barber's capture script (empathic/toolpath#108), from
// the same 10-step session. Our readers do not read compaction yet, so these snapshots show
// what compaction loses; when #108 lands their diffs are the proof of that work.
recording!(
    set_claude_forced,
    "test-fixtures/claude/convo-compacted.jsonl"
);
recording!(
    set_codex_forced,
    "test-fixtures/codex/convo-compacted.jsonl"
);
recording!(set_pi_forced, "test-fixtures/pi/convo-compacted.jsonl");
recording!(
    set_opencode_forced,
    "test-fixtures/opencode/convo-compacted.json"
);

/// Writer contracts the forced compactions break today, by set and the start of the rule's
/// message. This one is the opencode parent bug already pinned for the plain opencode recording
/// (every assistant message hangs off the first user turn, so the pi writer cuts the chain;
/// see `KNOWN` in toolpath-harness-mock/tests/claude.rs for the claude side), met again in
/// opencode's forced compaction. Not compaction-specific and not fixed ahead of Ben Barber's
/// empathic/toolpath#108: when it is fixed the entry stops breaking and the test says remove it.
const UNTIL_108: &[(&str, &str)] = &[("opencode-forced", "parent-chain (pi line")];

/// Every test above, by path, for [`every_recording_has_a_test`].
const RECORDINGS: &[&str] = &[
    "test-fixtures/claude/convo.jsonl",
    "test-fixtures/codex/convo.jsonl",
    "test-fixtures/copilot/convo.jsonl",
    "test-fixtures/cursor/convo.json",
    "test-fixtures/gemini/convo.jsonl",
    "test-fixtures/opencode/convo.json",
    "test-fixtures/pi/convo.jsonl",
    "goldens/claude-session/input.jsonl",
    "goldens/claude-compacted/input.jsonl",
    "test-fixtures/claude/convo-compacted.jsonl",
    "test-fixtures/codex/convo-compacted.jsonl",
    "test-fixtures/pi/convo-compacted.jsonl",
    "test-fixtures/opencode/convo-compacted.json",
];

/// A recording on disk without a test above would never be checked.
/// `goldens/<agent>/` is the old tool's copy of `test-fixtures/<agent>/` and
/// is not a recording.
#[test]
fn every_recording_has_a_test() {
    let mut on_disk = Vec::new();
    for (root, rel, file) in [
        (FIXTURES, "test-fixtures", "convo."),
        (FIXTURES, "test-fixtures", "convo-compacted."),
        (GOLDENS, "goldens", "input."),
    ] {
        for dir in std::fs::read_dir(root).expect("recordings directory") {
            let dir = dir.expect("entry").path();
            let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
                continue;
            };
            if rel == "goldens" && Agent::ALL.iter().any(|a| a.name() == name) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for f in entries.flatten() {
                let fname = f.file_name().to_string_lossy().into_owned();
                if fname.starts_with(file) {
                    on_disk.push(format!("{rel}/{name}/{fname}"));
                }
            }
        }
    }
    let missing: Vec<_> = on_disk
        .iter()
        .filter(|p| !RECORDINGS.contains(&p.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "recordings with no test in tests/goldens.rs: {missing:?}"
    );
}

/// The real HOME (a leak needle) and the temp HOME every adapter store points
/// into. Set once, by whichever recording test runs first; every test calls
/// this before anything reads the environment, and the others wait in
/// `get_or_init` while it runs.
fn homes() -> &'static (String, tempfile::TempDir) {
    static HOMES: OnceLock<(String, tempfile::TempDir)> = OnceLock::new();
    HOMES.get_or_init(|| {
        let real_home = std::env::var("HOME").expect("HOME is set");
        // The tests own HOME: every store an adapter might resolve points into
        // a directory that is empty and a leak needle below.
        let home = tempfile::tempdir().expect("temp HOME");
        point_every_store_at(home.path());
        (real_home, home)
    })
}

fn check_recording(rel: &str) {
    let (real_home, home) = homes();
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("../snapshots");
    settings.set_prepend_module_to_snapshot(false);
    // The IR carries hash maps; without this their key order changes per
    // process and `ir` would never be stable.
    settings.set_sort_maps(true);
    settings.set_input_file(&input);
    settings.bind(|| check_set(&input, real_home, home.path()));
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

    // The contracts (toolpath_goldens::contracts): what the harness or the
    // schema would reject, learned from bugs that shipped. Not pinned:
    // accepting a snapshot never accepts one of these.
    // Except what a forced compaction breaks before empathic/toolpath#108 lands ([`UNTIL_108`]):
    // named here, and required to still happen, so the fix removes the entry.
    let expected: Vec<&str> = UNTIL_108
        .iter()
        .filter(|(set_name, _)| *set_name == set.name)
        .map(|(_, rule)| *rule)
        .collect();
    let unexpected: Vec<&String> = stages
        .contracts
        .iter()
        .filter(|c| !expected.iter().any(|e| c.starts_with(e)))
        .collect();
    assert!(
        unexpected.is_empty(),
        "{}: broken contracts:\n  {}",
        set.name,
        unexpected
            .iter()
            .map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
    for e in &expected {
        assert!(
            stages.contracts.iter().any(|c| c.starts_with(e)),
            "{}: `{e}` no longer breaks: remove it from UNTIL_108 in tests/goldens.rs",
            set.name
        );
    }

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
                insta::assert_json_snapshot!("twice_around", stages.twice_around[agent]);
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
/// once, from [`homes`], before any adapter runs.
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
        // SAFETY: called once, inside `homes`' OnceLock, before any test reads
        // the environment (every test calls `homes()` first and the others
        // block in get_or_init); the pipeline itself never reads it.
        unsafe { std::env::set_var(k, v) };
    }
}

/// The same-format round trip is compared in the adapter's own shape. Gemini records a session as a
/// JSONL record stream and writes it back as one chat document; opencode's export nests `info` and
/// `parts` where its rows nest `data`. Compared raw, nearly every path "moved" and counted as lost
/// (82 of 87, 87 of 88); compared after the adapter has read the source, only real losses show.
#[test]
fn same_format_is_compared_in_the_adapters_own_shape() {
    let (_, home) = homes();
    for rel in [
        "test-fixtures/gemini/convo.jsonl",
        "test-fixtures/opencode/convo.json",
    ] {
        let input = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel);
        let set = Set::from_input_file(&input).expect("readable input");
        let stages = run(&set, home.path()).unwrap_or_else(|e| panic!("{}: {e}", set.name));
        // Fields every message of the format has and the round trip certainly writes back: if
        // one is "lost" the comparison is across shapes (the path moved), not a loss.
        let dropped: Vec<&str> = stages
            .shed
            .same_format
            .dropped
            .iter()
            .map(|d| d.path.as_str())
            .collect();
        for always in ["id", "timestamp", "content[].text", "info.id", "info.title"] {
            assert!(
                !dropped.contains(&always),
                "{rel}: `{always}` counted as lost: the comparison is across shapes"
            );
        }
    }
}
