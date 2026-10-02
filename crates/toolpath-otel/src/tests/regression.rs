//! Regression gate: what the walker and stitch make of each M0 fixture.
//! Regenerating these snapshots changes ids/bytes consumers rely on; do it
//! only deliberately.

use super::common::*;
use crate::tests::otel::hash::{canonical_json, sha256_hex};
use crate::tests::otel::{group_sessions, stitch};
use serde_json::{Value, json};

const FILES: [&str; 7] = [
    "claude-code.ndjson",
    "codex.ndjson",
    "opencode.ndjson",
    "pi.ndjson",
    "synthetic-fork.ndjson",
    "codex-error-span.json",
    "connection-test.json",
];

fn walk_record(values: &[Value]) -> Value {
    let out = read_deliveries(values).unwrap();
    let skipped: Vec<Value> = out
        .skipped
        .iter()
        .map(|s| json!([s.generation_id, s.session_id, s.profile, s.reason]))
        .collect();
    let generations: Vec<Value> = out
        .generations
        .iter()
        .map(|g| {
            let canon = canonical_json(&serde_json::to_value(g).unwrap());
            json!([g.id, sha256_hex(&[canon.as_bytes()])])
        })
        .collect();
    let unclaimed = out.unclaimed;
    let sessions: Vec<Value> = group_sessions(out.generations)
        .iter()
        .map(|s| {
            let g = stitch(s);
            json!({
                "key": s.key,
                "turn_ids": g.nodes.iter().map(|n| &n.id).collect::<Vec<_>>(),
                "completions": g.links.iter().map(|l| &l.completion).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({"generations": generations, "skipped": skipped, "unclaimed": unclaimed, "sessions": sessions})
}

fn stem(f: &str) -> &str {
    f.split('.').next().unwrap()
}

#[test]
fn walk_snapshots_per_fixture() {
    for f in FILES {
        check_snapshot(&format!("walk-{}", stem(f)), &walk_record(&deliveries(f)));
    }
}

#[test]
fn walk_snapshot_of_every_fixture_together() {
    let all: Vec<Value> = FILES.iter().flat_map(|f| deliveries(f)).collect();
    check_snapshot("walk-all", &walk_record(&all));
}

#[test]
fn no_existing_input_has_an_unclaimed_span() {
    for f in FILES {
        assert_eq!(read_deliveries(&deliveries(f)).unwrap().unclaimed, 0, "{f}");
    }
}
