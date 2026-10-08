//! Property tests for the derive/extract core.
//!
//! These own the interaction surface that example-based tests sample:
//! arbitrary interleavings of turns/events/compactions, with deliberate id
//! collisions and dangling parents, driven through the three properties the
//! whole pipeline is built on:
//!
//! 1. a derived path's step ids are unique;
//! 2. derive → extract → derive is stable at generation one;
//! 3. in a stream whose turns chain, every turn step is an ancestor of the
//!    head however the events are linked — an event with no parent or a
//!    dangling one cannot orphan the session.

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
    /// (id slot, parent slot or None). Slot 0 = id-less; 1–3 = `e<slot>`
    /// (repeats collide, the Claude reused-attachment-uuid shape); 4 = `t0`
    /// (collides with the turn id pool).
    Event(u8, Option<u8>),
}

fn elem() -> impl Strategy<Value = Elem> {
    prop_oneof![
        4 => (0u8..6, proptest::option::of(0u8..8), 0u8..4, 0u8..4)
            .prop_map(|(id, p, kind, muts)| Elem::Turn(id, p, kind, muts)),
        1 => (0u8..5, proptest::option::of(0u8..8))
            .prop_map(|(id_slot, p)| Elem::Event(id_slot, p)),
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
    for e in elems {
        match e {
            Elem::Turn(id_slot, p, kind, muts) => {
                let id = format!("t{id_slot}");
                let parent = resolve(p, &ids);
                let role = if kind == 0 {
                    Role::User
                } else {
                    Role::Assistant
                };
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
            Elem::Event(id_slot, p) => {
                let id = match id_slot {
                    0 => String::new(),
                    4 => "t0".to_string(),
                    s => format!("e{s}"),
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
    ConversationView {
        id: "prop-session".into(),
        items,
        provider_id: Some("prop".into()),
        ..Default::default()
    }
}

/// One element of a stream whose turns chain, as every shipped reader's do.
#[derive(Debug, Clone)]
enum ChainedElem {
    /// A turn chained onto the id-bearing item before it (role by parity).
    Turn(u8),
    /// (id slot, linkage). Slot 0 = id-less; 1–3 = `e<slot>` (repeats
    /// collide). Linkage: 0 = no parent, 1 = a parent naming nothing in the
    /// stream, 2 = the id-bearing item before it.
    Event(u8, u8),
}

fn chained_elem() -> impl Strategy<Value = ChainedElem> {
    prop_oneof![
        3 => (0u8..4).prop_map(ChainedElem::Turn),
        2 => (0u8..4, 0u8..3).prop_map(|(id_slot, link)| ChainedElem::Event(id_slot, link)),
    ]
}

/// Materialize a chained stream. Turn ids are distinct (`t<n>` by position)
/// so a turn's step is found under its own id. Event ids may collide but
/// every event carries its position in `data`, so a colliding event is
/// renamed rather than dropped as a replay, and the turn after it resolves
/// its parent through the event's source id to the renamed step.
fn build_chained_view(elems: Vec<ChainedElem>) -> ConversationView {
    let mut items: Vec<Item> = Vec::new();
    let mut last_id: Option<String> = None;
    for (n, e) in elems.into_iter().enumerate() {
        match e {
            ChainedElem::Turn(kind) => {
                let id = format!("t{n}");
                let role = if kind % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                };
                let text = format!("text-{n}-{kind}");
                items.push(Item::Turn(turn(&id, last_id.as_deref(), role, &text)));
                last_id = Some(id);
            }
            ChainedElem::Event(id_slot, link) => {
                let id = match id_slot {
                    0 => String::new(),
                    s => format!("e{s}"),
                };
                let parent = match link {
                    0 => None,
                    1 => Some("missing".to_string()),
                    _ => last_id.clone(),
                };
                items.push(Item::Event(ConversationEvent {
                    id: id.clone(),
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    parent_id: parent,
                    event_type: "generated".into(),
                    data: HashMap::from([("n".to_string(), serde_json::json!(n))]),
                }));
                if !id.is_empty() {
                    last_id = Some(id);
                }
            }
        }
    }
    ConversationView {
        id: "prop-session".into(),
        items,
        provider_id: Some("prop".into()),
        ..Default::default()
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn chained_turns_are_all_ancestors_of_head(
        elems in proptest::collection::vec(chained_elem(), 1..12),
    ) {
        let view = build_chained_view(elems);
        let path = derive_path(&view, &DeriveConfig::default());
        let active = toolpath::v1::query::ancestors(&path.steps, &path.path.head);
        for t in view.turns() {
            prop_assert!(
                active.contains(&t.id),
                "turn {:?} is not an ancestor of head {:?}; dead ends: {:?}",
                t.id,
                path.path.head,
                toolpath::v1::query::dead_ends(&path.steps, &path.path.head)
                    .iter()
                    .map(|s| s.step.id.as_str())
                    .collect::<Vec<_>>()
            );
        }
    }

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
        // Extract recovers the session id from the artifact key of a
        // `conversation.append` step, so identity is only representable once
        // there is at least one turn.
        prop_assume!(view.turns().next().is_some());
        let gen1 = derive_path(&view, &DeriveConfig::default());
        let gen2 = derive_path(&extract_conversation(&gen1), &DeriveConfig::default());
        prop_assert_eq!(
            serde_json::to_value(&gen1).unwrap(),
            serde_json::to_value(&gen2).unwrap(),
            "derive → extract → derive changed the document"
        );
    }
}
