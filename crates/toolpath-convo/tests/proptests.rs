//! Property tests for the derive/extract core.
//!
//! These own the interaction surface that example-based tests sample:
//! arbitrary interleavings of turns/events/compactions, with deliberate id
//! collisions and dangling parents, driven through the three properties the
//! whole pipeline is built on:
//!
//! 1. a derived path's step ids are unique;
//! 2. derive → extract → derive is stable at generation one;
//! 3. re-emitting a turn whose step kept its source linkage (the replay
//!    shape) — after the original, before the next turn — never changes
//!    the derived path at all.

use proptest::prelude::*;
use std::collections::HashMap;
use toolpath_convo::{
    ConversationEvent, ConversationView, DeriveConfig, Item, Role, Turn, derive_path,
    extract_conversation,
};

fn turn(id: &str, parent: Option<&str>, role: Role, text: &str) -> Turn {
    Turn {
        id: id.into(),
        parent_id: parent.map(Into::into),
        group_id: None,
        role,
        timestamp: "2026-01-01T00:00:00Z".into(),
        text: text.into(),
        thinking: None,
        tool_uses: vec![],
        model: None,
        stop_reason: None,
        token_usage: None,
        attributed_token_usage: None,
        environment: None,
        delegations: vec![],
        file_mutations: vec![],
    }
}

/// One generated stream element, before ids/parents are resolved.
#[derive(Debug, Clone)]
enum Elem {
    /// (id-pool slot, parent slot among earlier elems or None, kind,
    /// file-mutation count). Kind: 0 = user, 1 = assistant with a model,
    /// 2 = harness-synthetic assistant (`model == "<synthetic>"`, e.g. an
    /// API-error message), 3 = assistant with no model. Mutations give the
    /// step sibling `file.write` changes next to its `conversation.append` —
    /// the shape that made hash-order step classification (the pi kept-run
    /// loss) reachable.
    Turn(u8, Option<u8>, u8, u8),
    /// (has native id?, parent slot or None)
    Event(bool, Option<u8>),
}

fn elem() -> impl Strategy<Value = Elem> {
    prop_oneof![
        4 => (0u8..6, proptest::option::of(0u8..8), 0u8..4, 0u8..4)
            .prop_map(|(id, p, kind, muts)| Elem::Turn(id, p, kind, muts)),
        1 => (any::<bool>(), proptest::option::of(0u8..8))
            .prop_map(|(has_id, p)| Elem::Event(has_id, p)),
    ]
}

/// Materialize a stream: slot references resolve to the id of the n-th
/// earlier item (mod count), so parents are usually real, sometimes dangling
/// (when there is no earlier item), and ids collide when the pool slot
/// repeats — some byte-identical (same role/text), some not.
fn build_view(elems: Vec<Elem>) -> ConversationView {
    let mut items: Vec<Item> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    let resolve = |slot: Option<u8>, ids: &[String]| -> Option<String> {
        slot.and_then(|s| {
            if ids.is_empty() {
                None
            } else {
                Some(ids[s as usize % ids.len()].clone())
            }
        })
    };
    let mut event_n = 0usize;
    for e in elems {
        match e {
            Elem::Turn(id_slot, p, kind, muts) => {
                let id = format!("t{id_slot}");
                let parent = resolve(p, &ids);
                let role = if kind == 0 { Role::User } else { Role::Assistant };
                let text = format!("text-{id_slot}-{kind}");
                let mut t = turn(&id, parent.as_deref(), role, &text);
                t.model = match kind {
                    1 => Some("model-x".into()),
                    2 => Some("<synthetic>".into()),
                    _ => None,
                };
                t.file_mutations = (0..muts)
                    .map(|i| toolpath_convo::FileMutation {
                        path: format!("f{i}.txt"),
                        tool_id: None,
                        operation: Some("write".into()),
                        raw_diff: None,
                        before: None,
                        after: Some(format!("content-{id_slot}-{i}")),
                        rename_to: None,
                    })
                    .collect();
                items.push(Item::Turn(t));
                ids.push(id);
            }
            Elem::Event(has_id, p) => {
                event_n += 1;
                let id = if has_id {
                    format!("e{event_n}")
                } else {
                    String::new()
                };
                let parent = resolve(p, &ids);
                items.push(Item::Event(ConversationEvent {
                    id: id.clone(),
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    parent_id: parent,
                    event_type: "generated".into(),
                    data: HashMap::new(),
                }));
                if !id.is_empty() {
                    ids.push(id);
                }
            }
        }
    }
    // Session-level files_changed rides `meta.extra` on the wire; a
    // non-empty list makes the stability property cover its recovery.
    let files_changed = if items.is_empty() {
        vec![]
    } else {
        vec!["/abs/gen.rs".into(), "rel/gen.rs".into()]
    };
    ConversationView {
        id: "prop-session".into(),
        items,
        provider_id: Some("prop".into()),
        files_changed,
        ..Default::default()
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn derived_step_ids_are_unique(elems in proptest::collection::vec(elem(), 0..12)) {
        let view = build_view(elems);
        let path = derive_path(&view, &DeriveConfig::default());
        let mut seen = std::collections::HashSet::new();
        for s in &path.steps {
            prop_assert!(seen.insert(&s.step.id), "duplicate step id {:?}", s.step.id);
        }
    }

    #[test]
    fn derive_extract_derive_is_stable(elems in proptest::collection::vec(elem(), 0..12)) {
        let view = build_view(elems);
        // An empty conversation derives to an empty path, which carries no
        // artifact keys to recover the session id from — identity is only
        // representable once there is at least one step.
        prop_assume!(!view.items.is_empty());
        let gen1 = derive_path(&view, &DeriveConfig::default());
        let gen2 = derive_path(&extract_conversation(&gen1), &DeriveConfig::default());
        prop_assert_eq!(
            serde_json::to_value(&gen1).unwrap(),
            serde_json::to_value(&gen2).unwrap(),
            "derive → extract → derive changed the document"
        );
    }

    #[test]
    fn byte_identical_replay_is_a_no_op(
        elems in proptest::collection::vec(elem(), 1..10),
        replay_of in 0u8..8,
        skip_nonturns in 0u8..4,
    ) {
        // The real replay shape (a Claude chain merge): a turn is re-emitted
        // with its original id and parent linkage, after the original —
        // possibly with events/compactions between, but before the next
        // turn. A copy at such a position resolves to a byte-identical step
        // and must be dropped without any effect on the derived path.
        // (A same-id turn with *different* linkage is not a replay; the
        // dedup renames it, which is data-preserving, not a no-op.)
        let base = build_view(elems);
        let uniquely_idd: Vec<Turn> = {
            let all: Vec<&Turn> = base.turns().collect();
            all.iter()
                .filter(|t| {
                    t.parent_id.is_some() && all.iter().filter(|o| o.id == t.id).count() == 1
                })
                .map(|t| (*t).clone())
                .collect()
        };
        // A replay is byte-identical only when the original's step kept its
        // source linkage verbatim (derive didn't splice or re-anchor it) —
        // restrict candidates to those.
        let derived = derive_path(&base, &DeriveConfig::default());
        let replayable: Vec<Turn> = uniquely_idd
            .into_iter()
            .filter(|t| {
                derived
                    .steps
                    .iter()
                    .find(|s| s.step.id == t.id)
                    .is_some_and(|s| {
                        s.step.parents.first() == t.parent_id.as_ref()
                            && s.step.parents.len() <= 1
                    })
            })
            .collect();
        prop_assume!(!replayable.is_empty());
        let src_turn = replayable[replay_of as usize % replayable.len()].clone();
        let src_pos = base
            .items
            .iter()
            .position(|i| matches!(i, Item::Turn(t) if t.id == src_turn.id))
            .unwrap();
        // Insert after the original, skipping up to `skip_nonturns` of the
        // non-turn items that follow it (events/compactions may sit between
        // an original and its replay).
        let mut at = src_pos + 1;
        let mut skips = skip_nonturns;
        while skips > 0
            && at < base.items.len()
            && !matches!(base.items[at], Item::Turn(_))
        {
            at += 1;
            skips -= 1;
        }

        let mut with_replay = base.clone();
        with_replay.items.insert(at, Item::Turn(src_turn));

        let without = derive_path(&base, &DeriveConfig::default());
        let with = derive_path(&with_replay, &DeriveConfig::default());
        prop_assert_eq!(
            serde_json::to_value(&without).unwrap(),
            serde_json::to_value(&with).unwrap(),
            "a dropped byte-identical replay changed the derived path"
        );
    }
}
