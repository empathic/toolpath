#![allow(dead_code)]

use crate::tests::otel::{ProfileSelection, ReadOutcome, Session, TurnGraph, group_sessions};
use serde_json::Value;
use std::path::PathBuf;

pub mod captures;
pub mod equivalence;
pub mod retention;

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel/openrouter")
}

/// One delivery per line (`.ndjson`) or one delivery per file (`.json`).
pub fn deliveries(file: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(fixtures_dir().join(file)).unwrap();
    if file.ends_with(".ndjson") {
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    } else {
        vec![serde_json::from_str(&text).unwrap()]
    }
}

pub fn expected() -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixtures_dir().join("expected.json")).unwrap())
        .unwrap()
}

/// The four real sessions: (fixture file, key in expected.json).
pub const REAL: [(&str, &str); 4] = [
    ("claude-code.ndjson", "claude-code"),
    ("codex.ndjson", "codex"),
    ("opencode.ndjson", "opencode"),
    ("pi.ndjson", "pi"),
];

/// Every fixture file that holds a conversation.
pub const CONVERSATIONS: [&str; 5] = [
    "claude-code.ndjson",
    "codex.ndjson",
    "opencode.ndjson",
    "pi.ndjson",
    "synthetic-fork.ndjson",
];

pub fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

/// `read_deliveries` with default options (auto profile resolution).
pub fn read_deliveries<'a>(
    values: impl IntoIterator<Item = &'a Value>,
) -> crate::tests::otel::Result<ReadOutcome> {
    crate::tests::otel::read_deliveries(values, ProfileSelection::Auto)
}

pub fn session(file: &str) -> Session {
    let mut sessions = group_sessions(read_deliveries(&deliveries(file)).unwrap().generations);
    assert_eq!(sessions.len(), 1, "{file}");
    sessions.remove(0)
}

/// Node ids with more than one child.
pub fn fork_points(g: &TurnGraph) -> Vec<String> {
    let mut children: std::collections::BTreeMap<&str, usize> = Default::default();
    for n in &g.nodes {
        if let Some(p) = &n.parent {
            *children.entry(p.as_str()).or_default() += 1;
        }
    }
    children
        .into_iter()
        .filter(|(_, c)| *c > 1)
        .map(|(p, _)| p.to_string())
        .collect()
}

/// Compare `actual` with `tests/snapshots/<name>.json`. A missing file is
/// written and the test fails, so a new snapshot is always reviewed
/// before it is committed. Regenerating a committed snapshot changes
/// ids/bytes consumers rely on; do it only deliberately, with the crate's
/// one bless command: `TOOLPATH_OTEL_BLESS=1 cargo test -p toolpath-otel`.
pub fn check_snapshot(name: &str, actual: &Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.json"));
    check_snapshot_at(
        &path,
        actual,
        std::env::var_os("TOOLPATH_OTEL_BLESS").is_some(),
    );
}

/// [`check_snapshot`] against the file at `path`; `bless` rewrites it.
pub fn check_snapshot_at(path: &std::path::Path, actual: &Value, bless: bool) {
    let name = path.display();
    let text = format!("{}\n", serde_json::to_string_pretty(actual).unwrap());
    if bless {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &text).unwrap();
        return;
    }
    match std::fs::read_to_string(path) {
        Ok(want) => assert!(
            want == text,
            "snapshot {name} differs\n--- committed\n{want}\n--- now\n{text}\n\
             Regenerating this snapshot changes ids/bytes consumers rely on; \
             do it only deliberately (TOOLPATH_OTEL_BLESS=1)."
        ),
        Err(_) => {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &text).unwrap();
            panic!("wrote new snapshot {name}; review it, commit it, re-run");
        }
    }
}
