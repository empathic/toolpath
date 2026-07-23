//! Test support: executable invariants for provider round-trips.
//!
//! Every provider's compaction/round-trip suite drives its own
//! project→read cycle and hands the resulting views to [`assert_fixpoint`];
//! [`check_view_invariants`] is the structural half, callable anywhere a
//! `ConversationView` is produced. Keeping these here — instead of
//! re-asserting ad hoc per provider — is what makes the round-trip
//! contract executable rather than prose.

use crate::{Compaction, ConversationView, Item, expand_kept};

/// Structural invariants every well-formed view satisfies. Returns one
/// message per violation (empty = healthy).
///
/// - a `Compaction.kept_from` anchor resolves to a non-empty contiguous
///   run on the boundary's parent chain (see [`expand_kept`]);
/// - a compaction's `parent_id` references an item that appears earlier
///   in the stream;
/// - a turn's `parent_id` references some item in the view.
pub fn check_view_invariants(view: &ConversationView) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen_ids: Vec<&str> = Vec::new();
    let all_ids: std::collections::HashSet<&str> = view
        .items
        .iter()
        .map(|i| match i {
            Item::Turn(t) => t.id.as_str(),
            Item::Event(e) => e.id.as_str(),
            Item::Compaction(c) => c.id.as_str(),
        })
        .collect();

    for item in &view.items {
        match item {
            Item::Compaction(c) => {
                if c.kept_from.is_some() && expand_kept(&view.items, c).is_empty() {
                    problems.push(format!(
                        "compaction {}: kept_from {:?} does not resolve to a run on the \
                         boundary's parent chain",
                        c.id, c.kept_from
                    ));
                }
                if let Some(pid) = &c.parent_id
                    && !seen_ids.contains(&pid.as_str())
                {
                    problems.push(format!(
                        "compaction {}: parent_id {pid:?} is not an earlier item",
                        c.id
                    ));
                }
                seen_ids.push(c.id.as_str());
            }
            Item::Turn(t) => {
                if let Some(pid) = &t.parent_id
                    && !all_ids.contains(pid.as_str())
                {
                    problems.push(format!(
                        "turn {}: parent_id {pid:?} references no item in the view",
                        t.id
                    ));
                }
                seen_ids.push(t.id.as_str());
            }
            Item::Event(e) => seen_ids.push(e.id.as_str()),
        }
    }
    problems
}

/// Assert the projection fixpoint contract for one native round-trip
/// cycle: `source` is the original view, `once` = read(project(source)),
/// `twice` = read(project(once)).
///
/// The contract has two halves:
///
/// 1. **Idempotency** — one projection may normalize (re-mint ids, drop
///    provider passthrough the wire can't carry), but a second cycle must
///    be the identity: `once == twice`, compared as full serde values.
/// 2. **Survival** — the provenance payload must cross the first
///    projection intact: compaction count, each boundary's summary text
///    (whitespace-normalized), trigger, `kept_from`, and its position
///    among the turns.
///
/// Both output views must also satisfy [`check_view_invariants`].
pub fn assert_fixpoint(
    source: &ConversationView,
    once: &ConversationView,
    twice: &ConversationView,
) {
    let problems = check_view_invariants(once);
    assert!(
        problems.is_empty(),
        "view after one round-trip violates invariants:\n  {}",
        problems.join("\n  ")
    );
    let problems = check_view_invariants(twice);
    assert!(
        problems.is_empty(),
        "view after two round-trips violates invariants:\n  {}",
        problems.join("\n  ")
    );

    let v1 = serde_json::to_value(once).expect("view serializes");
    let v2 = serde_json::to_value(twice).expect("view serializes");
    assert_eq!(
        v1, v2,
        "project→read is not idempotent: second cycle changed the view"
    );

    let src: Vec<&Compaction> = source.compactions().collect();
    let out: Vec<&Compaction> = once.compactions().collect();
    assert_eq!(
        src.len(),
        out.len(),
        "compaction count changed across projection"
    );
    for (s, o) in src.iter().zip(&out) {
        if let Some(sum) = &s.summary {
            assert_eq!(
                Some(normalize_ws(sum)),
                o.summary.as_deref().map(normalize_ws),
                "boundary {}: summary text changed across projection",
                s.id
            );
        }
        if s.trigger.is_some() {
            assert_eq!(
                s.trigger, o.trigger,
                "boundary {}: trigger changed across projection",
                s.id
            );
        }
        assert_eq!(
            kept_from_position(source, s),
            kept_from_position(once, o),
            "boundary {}: kept anchor moved across projection (as a turn offset)",
            s.id
        );
    }
    for (nth, (s, _)) in src.iter().zip(&out).enumerate() {
        assert_eq!(
            boundary_position(source, nth),
            boundary_position(once, nth),
            "boundary {}: position among turns changed across projection",
            s.id
        );
    }
}

/// The `nth` boundary's position expressed as "number of turns before it"
/// — stable across projectors that re-mint ids.
fn boundary_position(view: &ConversationView, nth: usize) -> Option<usize> {
    let mut turns = 0usize;
    let mut seen = 0usize;
    for item in &view.items {
        match item {
            Item::Turn(_) => turns += 1,
            Item::Compaction(_) => {
                if seen == nth {
                    return Some(turns);
                }
                seen += 1;
            }
            Item::Event(_) => {}
        }
    }
    None
}

/// `kept_from` expressed as "how many turns before the boundary survive" —
/// id-agnostic, so it survives projectors that re-mint turn ids.
fn kept_from_position(view: &ConversationView, c: &Compaction) -> Option<usize> {
    c.kept_from
        .as_ref()
        .map(|_| expand_kept(&view.items, c).len())
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
