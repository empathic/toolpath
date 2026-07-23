//! Shared derivation: [`ConversationView`] → [`toolpath::v1::Path`].
//!
//! Provider-agnostic mapping used by the Pi, Claude, and future conversation
//! providers. Takes a [`ConversationView`] and emits a [`Path`] document with
//! one step per turn and a `conversation.append` structural change carrying
//! the turn's text, thinking, tool uses, and token usage. The emitted path is
//! tagged with `meta.kind = PATH_KIND_AGENT_CODING_SESSION`.

use std::collections::HashMap;

use toolpath::v1::{
    ActorDefinition, ArtifactChange, Base, PATH_KIND_AGENT_CODING_SESSION, Path, PathIdentity,
    PathMeta, Step, StepIdentity, StructuralChange,
};

use crate::{ConversationView, Item, Role, ToolCategory, ToolInvocation, Turn};

/// Configuration for [`derive_path`].
#[derive(Debug, Clone)]
pub struct DeriveConfig {
    /// Override `path.base.uri`. If `None`, fall back to the first turn's
    /// `environment.working_dir`.
    pub base_uri: Option<String>,
    /// Override `path.id`. If `None`, derive as `path-{provider}-{8chars}`.
    pub path_id: Option<String>,
    /// Override `meta.title`. If `None`, default to `"{provider} session: {8chars}"`.
    pub title: Option<String>,
    /// Include `Turn.thinking` in the structural change extras.
    pub include_thinking: bool,
    /// Include `Turn.tool_uses` in the structural change extras.
    pub include_tool_uses: bool,
}

impl Default for DeriveConfig {
    fn default() -> Self {
        Self {
            base_uri: None,
            path_id: None,
            title: None,
            include_thinking: true,
            include_tool_uses: true,
        }
    }
}

/// Derive a [`Path`] from a [`ConversationView`].
///
/// Step ids must be unique within a path (a toolpath invariant), so a
/// collision is resolved as the steps are emitted rather than surfaced as an
/// error: a byte-identical re-emission (e.g. a Claude compaction replay of an
/// unchanged message) is dropped, and a same-id-but-different step is renamed
/// to a fresh id. Either way the derivation always succeeds and the result is
/// collision-free.
pub fn derive_path(view: &ConversationView, config: &DeriveConfig) -> Path {
    let provider = view.provider_id.as_deref().unwrap_or("unknown");
    let id_prefix: String = view.id.chars().take(8).collect();

    let path_id = config
        .path_id
        .clone()
        .unwrap_or_else(|| format!("path-{}-{}", provider, id_prefix));

    // Base resolution order:
    //   1. `config.base_uri` (CLI override): provides the `uri`; ref/branch
    //      come from `view.base` if set.
    //   2. `view.base` (provider-populated): the canonical source.
    //   3. First turn's `environment.working_dir` (legacy fallback).
    let base = config
        .base_uri
        .clone()
        .map(|uri| Base {
            uri,
            ref_str: view.base.as_ref().and_then(|b| b.vcs_revision.clone()),
            branch: view.base.as_ref().and_then(|b| b.vcs_branch.clone()),
        })
        .or_else(|| {
            view.base.as_ref().and_then(|b| {
                let wd = b.working_dir.as_ref()?;
                let uri = if wd.starts_with('/') {
                    format!("file://{}", wd)
                } else {
                    wd.clone()
                };
                Some(Base {
                    uri,
                    ref_str: b.vcs_revision.clone(),
                    branch: b.vcs_branch.clone(),
                })
            })
        })
        .or_else(|| {
            view.turns()
                .find_map(|t| t.environment.as_ref()?.working_dir.clone())
                .map(|wd| {
                    let uri = if wd.starts_with('/') {
                        format!("file://{}", wd)
                    } else {
                        wd
                    };
                    Base {
                        uri,
                        ref_str: None,
                        branch: None,
                    }
                })
        });

    let conv_artifact_key = format!("{}://{}", provider, view.id);

    let mut steps: Vec<Step> = Vec::with_capacity(view.items.len());
    // Final step id → index in `steps`, for resolving id collisions as steps
    // are emitted (see `push_step`).
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut turn_to_step: HashMap<String, String> = HashMap::new();
    let mut actors: HashMap<String, ActorDefinition> = HashMap::new();

    // Single ordered pass over `view.items`, dispatching per variant so each
    // emitted step lands at its true position relative to its neighbors —
    // crucial for compaction boundaries, which must sit between the turns
    // they separate.
    //
    // Per-variant counters drive the synthetic step ids: turns synthesize
    // `step-{:04}` indexed by turn count, events `event-{:04}` by event count.
    //
    // `last_step_id` tracks the previously emitted step so that events (and
    // compactions) without an explicit parent chain off whatever came before.
    let mut turn_idx = 0usize;
    let mut event_idx = 0usize;
    let mut compact_idx = 0usize;
    let mut last_step_id: Option<String> = None;
    // The step id of the most recent *turn* (events/compactions don't update
    // it). Used to splice intervening events/compactions into the linear
    // parent chain so they land on the head's ancestry instead of dangling
    // as false dead ends — without disturbing genuine branches.
    let mut prev_turn_step: Option<String> = None;
    // The most recent turn OR compaction step — the chain anchor. Turns
    // advance both; compactions advance only this, so a post-compaction
    // turn chaining onto the boundary splices correctly while genuine
    // turn-to-turn branches are still judged against `prev_turn_step`.
    let mut prev_anchor_step: Option<String> = None;
    let mut event_steps: std::collections::HashSet<String> = std::collections::HashSet::new();

    // A byte-identical re-emission of an id-bearing item (the Claude
    // chain-merge replay shape) is the same source entry, not a new step,
    // and is recognized on source bytes, before any resolution: resolved
    // step forms are not comparable across the stream (splicing rewires
    // `parents`, and parent mappings mutate as colliding steps rename),
    // but source bytes are. Turn replays are identified in a prepass so
    // every per-turn structure below (`turn_groups`, synthesized
    // `step-NNNN` ids, group accounting) is built over surviving turns
    // only — an in-loop skip would still consume a group slot and an id
    // slot, losing a group-tail usage stamp and shifting later synthesized
    // ids. Event replays skip in-loop, before consuming an event index.
    let turn_skip: Vec<bool> = {
        let mut seen: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
        view.turns()
            .map(|turn| {
                if turn.id.is_empty() {
                    return false;
                }
                let Ok(source) = serde_json::to_value(turn) else {
                    return false;
                };
                let variants = seen.entry(turn.id.clone()).or_default();
                if variants.contains(&source) {
                    true
                } else {
                    variants.push(source);
                    false
                }
            })
            .collect()
    };
    let mut seen_event_sources: HashMap<String, Vec<serde_json::Value>> = HashMap::new();

    // Group ids of surviving turns in stream order, so a turn can tell
    // whether it's the last of its message group (message-level token
    // accounting, below).
    let turn_groups: Vec<Option<String>> = view
        .turns()
        .zip(&turn_skip)
        .filter(|(_, skip)| !**skip)
        .map(|(t, _)| t.group_id.clone())
        .collect();

    let mut turn_walk_idx = 0usize;
    for item in &view.items {
        match item {
            Item::Turn(turn) => {
                let walked = turn_walk_idx;
                turn_walk_idx += 1;
                if turn_skip[walked] {
                    continue;
                }
                let idx = turn_idx;
                turn_idx += 1;

                // Step id: use the turn's native id when set so it round-trips
                // through `extract_conversation`; otherwise synthesize sequentially.
                let step_id = if turn.id.is_empty() {
                    format!("step-{:04}", idx + 1)
                } else {
                    turn.id.clone()
                };

                let actor = actor_for_turn(turn, provider);
                record_actor(&mut actors, &actor, turn, provider, view);

                let mut step = Step {
                    step: StepIdentity {
                        id: step_id.clone(),
                        parents: Vec::new(),
                        actor,
                        timestamp: turn.timestamp.clone(),
                    },
                    change: HashMap::new(),
                    meta: None,
                };

                // Parent mapping
                if let Some(parent_id) = &turn.parent_id
                    && let Some(parent_step_id) = turn_to_step.get(parent_id)
                {
                    step.step.parents.push(parent_step_id.clone());
                }

                let pre_splice = step.step.parents.first().cloned();
                step.step.parents = splice_onto_intervening(
                    step.step.parents,
                    &prev_turn_step,
                    &prev_anchor_step,
                    &last_step_id,
                );
                let final_parent = step.step.parents.first().cloned();
                let spliced = final_parent != pre_splice;
                let onto_event = final_parent
                    .as_ref()
                    .is_some_and(|p| event_steps.contains(p));

                // Build conversation.append structural change extras
                let mut extra: HashMap<String, serde_json::Value> = HashMap::new();
                // Splicing rewires `parents` onto intervening steps and would
                // otherwise destroy the source linkage; the pre-splice parent
                // (null for a root) rides along so extract can restore it
                // exactly instead of guessing it back from the event chain.
                // Also stamped when the parent IS an event step natively —
                // without it, extract can't tell native event linkage from a
                // splice artifact and re-derivation would drift.
                if spliced || onto_event {
                    extra.insert(
                        "source_parent".to_string(),
                        pre_splice
                            .clone()
                            .map_or(serde_json::Value::Null, serde_json::Value::String),
                    );
                }
                extra.insert(
                    "role".to_string(),
                    serde_json::Value::String(turn.role.to_string()),
                );
                extra.insert(
                    "text".to_string(),
                    serde_json::Value::String(turn.text.clone()),
                );

                if config.include_thinking
                    && let Some(thinking) = &turn.thinking
                {
                    extra.insert(
                        "thinking".to_string(),
                        serde_json::Value::String(thinking.clone()),
                    );
                }

                if config.include_tool_uses && !turn.tool_uses.is_empty() {
                    let arr: Vec<serde_json::Value> = turn
                        .tool_uses
                        .iter()
                        .map(|t| {
                            let mut obj = serde_json::json!({
                                "id": t.id,
                                "name": t.name,
                                "input": t.input,
                                "category": t.category,
                            });
                            if let Some(result) = &t.result
                                && let Ok(v) = serde_json::to_value(result)
                            {
                                obj.as_object_mut().unwrap().insert("result".to_string(), v);
                            }
                            obj
                        })
                        .collect();
                    extra.insert("tool_uses".to_string(), serde_json::Value::Array(arr));
                }

                // Message-group accounting: a message's total `token_usage`
                // lands once, on the group's last turn, so summing over a
                // path's steps yields session totals. A turn with no `group_id`
                // is its own group.
                let last_of_group = match &turn.group_id {
                    None => true,
                    Some(mid) => match turn_groups.get(idx + 1) {
                        Some(Some(next)) => next != mid,
                        _ => true,
                    },
                };
                if last_of_group
                    && let Some(usage) = &turn.token_usage
                    && let Ok(v) = serde_json::to_value(usage)
                {
                    extra.insert("token_usage".to_string(), v);
                }

                // Per-step attributed spend (when the source reports it) rides
                // its own key, independent of the once-per-group `token_usage`.
                if let Some(attr) = &turn.attributed_token_usage
                    && let Ok(v) = serde_json::to_value(attr)
                {
                    extra.insert("attributed_token_usage".to_string(), v);
                }

                if let Some(mid) = &turn.group_id {
                    extra.insert(
                        "group_id".to_string(),
                        serde_json::Value::String(mid.clone()),
                    );
                }

                if !turn.delegations.is_empty()
                    && let Ok(v) = serde_json::to_value(&turn.delegations)
                {
                    extra.insert("delegations".to_string(), v);
                }

                if let Some(stop_reason) = &turn.stop_reason {
                    extra.insert(
                        "stop_reason".to_string(),
                        serde_json::Value::String(stop_reason.clone()),
                    );
                }

                if let Some(env) = &turn.environment
                    && let Ok(v) = serde_json::to_value(env)
                {
                    extra.insert("environment".to_string(), v);
                }

                step.change.insert(
                    conv_artifact_key.clone(),
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".to_string(),
                            extra,
                        }),
                    },
                );

                // File mutations → sibling `file.write` change entries.
                //
                // Preferred: each `Turn::file_mutations` entry comes from the
                // provider's `to_view` with the resolved diff already in
                // `raw_diff` (claude's git-HEAD lookup, codex's `apply_patch_end`
                // parse, opencode's git2 tree↔tree, etc.). `tool_id` links back
                // to a specific `ToolInvocation` when the provider can attribute.
                //
                // Fallback (un-migrated providers): for any `FileWrite`-category
                // tool with no matching mutation, synthesize from `tool.input`
                // via `file_write_change`.
                let attributed: std::collections::HashSet<String> = turn
                    .file_mutations
                    .iter()
                    .filter_map(|fm| fm.tool_id.clone())
                    .collect();
                for fm in &turn.file_mutations {
                    let mut t_extra: HashMap<String, serde_json::Value> = HashMap::new();
                    if let Some(tid) = &fm.tool_id {
                        t_extra.insert(
                            "tool_id".to_string(),
                            serde_json::Value::String(tid.clone()),
                        );
                        if let Some(tool) = turn.tool_uses.iter().find(|t| &t.id == tid) {
                            t_extra.insert(
                                "tool".to_string(),
                                serde_json::Value::String(tool.name.clone()),
                            );
                        }
                    }
                    if let Some(op) = &fm.operation {
                        t_extra.insert(
                            "operation".to_string(),
                            serde_json::Value::String(op.clone()),
                        );
                    }
                    if let Some(b) = &fm.before {
                        t_extra.insert("before".to_string(), serde_json::Value::String(b.clone()));
                    }
                    if let Some(a) = &fm.after {
                        t_extra.insert("after".to_string(), serde_json::Value::String(a.clone()));
                    }
                    if let Some(rt) = &fm.rename_to {
                        t_extra.insert(
                            "rename_to".to_string(),
                            serde_json::Value::String(rt.clone()),
                        );
                    }
                    step.change.insert(
                        fm.path.clone(),
                        ArtifactChange {
                            raw: fm.raw_diff.clone(),
                            structural: Some(StructuralChange {
                                change_type: "file.write".to_string(),
                                extra: t_extra,
                            }),
                        },
                    );
                }
                for tool in &turn.tool_uses {
                    if tool.category != Some(ToolCategory::FileWrite)
                        || attributed.contains(&tool.id)
                    {
                        continue;
                    }
                    let Some(path) = extract_file_path(tool) else {
                        continue;
                    };
                    let (raw, mut t_extra) = file_write_change(tool, &path, None);
                    t_extra.insert(
                        "tool".to_string(),
                        serde_json::Value::String(tool.name.clone()),
                    );
                    t_extra.insert(
                        "tool_id".to_string(),
                        serde_json::Value::String(tool.id.clone()),
                    );
                    step.change.insert(
                        path,
                        ArtifactChange {
                            raw,
                            structural: Some(StructuralChange {
                                change_type: "file.write".to_string(),
                                extra: t_extra,
                            }),
                        },
                    );
                }

                let (final_id, appended) = push_step(&mut steps, &mut by_id, step);
                // Map the turn's native id to whatever id its step ended up
                // with, so later turns chaining off it resolve correctly even
                // when this one was renamed or dropped as a duplicate.
                turn_to_step.insert(turn.id.clone(), final_id.clone());
                if appended {
                    last_step_id = Some(final_id.clone());
                    prev_turn_step = Some(final_id.clone());
                    prev_anchor_step = Some(final_id);
                }
            }

            // Emit `view.events` as `conversation.event` steps so that
            // attachments, preamble lines (ai-title, last-prompt,
            // queue-operation, permission-mode), and other non-turn entries
            // survive the IR-to-Path-to-IR roundtrip. Without this,
            // derive_path drops everything outside `turns`, so a Claude
            // session loses ~10–25% of its lines on import/export. An event
            // without an explicit `parent_id` chains off whatever step came
            // before it.
            Item::Event(event) => {
                // Skip before consuming an event index, so the `event-NNNN`
                // ids synthesized for id-less events don't shift when a
                // replay sits among them.
                if !event.id.is_empty()
                    && let Ok(source) = serde_json::to_value(event)
                {
                    let variants = seen_event_sources.entry(event.id.clone()).or_default();
                    if variants.contains(&source) {
                        continue;
                    }
                    variants.push(source);
                }

                let idx = event_idx;
                event_idx += 1;

                // Event step id: prefer the event's native id so it round-trips.
                let step_id = if event.id.is_empty() {
                    format!("event-{:04}", idx + 1)
                } else {
                    event.id.clone()
                };
                let actor = format!("tool:{}", provider);
                actors
                    .entry(actor.clone())
                    .or_insert_with(|| ActorDefinition {
                        name: Some(provider.to_string()),
                        provider: Some(provider.to_string()),
                        ..Default::default()
                    });

                // event.data is flattened into StructuralChange.extra. Strip keys
                // that collide with the typed fields on StructuralChange itself —
                // most importantly `type`, which serde renames `change_type` to.
                // A Codex `user_message` event carries `data["type"] = "user_message"`,
                // which would otherwise overwrite our `change_type = "conversation.event"`
                // and break PathOrRef untagged-enum disambiguation on parse.
                let mut extra: HashMap<String, serde_json::Value> = event
                    .data
                    .iter()
                    .filter(|(k, _)| k.as_str() != "type")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                // Stash the original `type` value under a non-colliding key so
                // round-trip can recover it for providers that need it.
                if let Some(t) = event.data.get("type") {
                    extra.insert("event_data_type".to_string(), t.clone());
                }
                extra.insert(
                    "entry_type".to_string(),
                    serde_json::Value::String(event.event_type.clone()),
                );
                // Always recorded — for an id-less event this is the
                // synthesized step id, so extract restores exactly the id a
                // re-derive will see and derive→extract→derive is stable at
                // generation one (an absent key would make extract fall back
                // to the step id and gen 2 gain this key, changing bytes).
                extra.insert(
                    "event_source_id".to_string(),
                    serde_json::Value::String(step_id.clone()),
                );
                // Source linkage, always recorded (`null` = chained by
                // position, no named parent). Without it, extract can only
                // hand back the resolved-and-spliced chain, which erases the
                // source distinction between an event that named a parent and
                // one that was positional — two such events with otherwise
                // equal data would collapse into one on the next derive. The
                // named form stamps the *resolved* parent (a parent that was
                // renamed stamps its renamed id, verbatim when unresolvable),
                // so a re-derive of the extracted view resolves identically.
                extra.insert(
                    "source_parent".to_string(),
                    event
                        .parent_id
                        .as_ref()
                        .map_or(serde_json::Value::Null, |pid| {
                            serde_json::Value::String(
                                turn_to_step
                                    .get(pid)
                                    .cloned()
                                    .unwrap_or_else(|| pid.clone()),
                            )
                        }),
                );

                let parents: Vec<String> = event
                    .parent_id
                    .as_ref()
                    .and_then(|pid| turn_to_step.get(pid).cloned())
                    .or_else(|| last_step_id.clone())
                    .into_iter()
                    .collect();
                let parents = splice_onto_intervening(
                    parents,
                    &prev_turn_step,
                    &prev_anchor_step,
                    &last_step_id,
                );

                let mut step = Step {
                    step: StepIdentity {
                        id: step_id.clone(),
                        parents,
                        actor,
                        timestamp: event.timestamp.clone(),
                    },
                    change: HashMap::new(),
                    meta: None,
                };

                step.change.insert(
                    conv_artifact_key.clone(),
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.event".to_string(),
                            extra,
                        }),
                    },
                );
                let (final_id, appended) = push_step(&mut steps, &mut by_id, step);
                // Let a later turn spliced onto this event resolve its parent
                // on a re-derive (its `parent_id` will be this event's step id).
                turn_to_step.insert(final_id.clone(), final_id.clone());
                event_steps.insert(final_id.clone());
                if appended {
                    last_step_id = Some(final_id);
                }
            }

            // A context-compaction boundary projects to one
            // `conversation.compact` step, positioned between the turns it
            // separates. Later turns whose `parent_id` references this
            // compaction resolve to its step via `turn_to_step`, so the DAG
            // is rewired through the boundary.
            Item::Compaction(c) => {
                let step_id = if c.id.is_empty() {
                    compact_idx += 1;
                    format!("compact-{:04}", compact_idx)
                } else {
                    c.id.clone()
                };

                let actor = format!("tool:{}", provider);
                actors
                    .entry(actor.clone())
                    .or_insert_with(|| ActorDefinition {
                        name: Some(provider.to_string()),
                        provider: Some(provider.to_string()),
                        ..Default::default()
                    });

                let resolved = c
                    .parent_id
                    .as_ref()
                    .and_then(|pid| turn_to_step.get(pid).cloned());
                // Fall back to whatever step came before (as events do)
                // so a compaction with a missing/unresolvable parent_id
                // still chains onto the DAG instead of becoming a
                // disconnected root that orphans the pre-compaction turns.
                let parents: Vec<String> = resolved
                    .clone()
                    .or_else(|| last_step_id.clone())
                    .into_iter()
                    .collect();
                // Splice: if the boundary chains onto the previous turn but an
                // event was emitted between them, chain through that event so
                // it isn't orphaned.
                let parents = splice_onto_intervening(
                    parents,
                    &prev_turn_step,
                    &prev_anchor_step,
                    &last_step_id,
                );
                let rewired = parents.first().cloned() != resolved;
                let onto_event = parents.first().is_some_and(|p| event_steps.contains(p));

                let mut extra: HashMap<String, serde_json::Value> = HashMap::new();
                // As in the turn arm: fallback and splice rewire `parents`,
                // so the pre-rewrite source parent rides along for extract.
                if rewired || onto_event {
                    extra.insert(
                        "source_parent".to_string(),
                        resolved
                            .clone()
                            .map_or(serde_json::Value::Null, serde_json::Value::String),
                    );
                }
                if let Some(trigger) = &c.trigger {
                    let s = match trigger {
                        crate::CompactionTrigger::Auto => "auto",
                        crate::CompactionTrigger::Manual => "manual",
                    };
                    extra.insert("trigger".to_string(), serde_json::Value::String(s.into()));
                }
                if let Some(summary) = &c.summary {
                    extra.insert(
                        "summary".to_string(),
                        serde_json::Value::String(summary.clone()),
                    );
                }
                if let Some(pre_tokens) = c.pre_tokens {
                    extra.insert(
                        "pre_tokens".to_string(),
                        serde_json::Value::Number(pre_tokens.into()),
                    );
                }
                // `kept` on the wire is the expanded contiguous run (oldest
                // first) so consumers get concrete ids; the anchor is
                // recoverable as its first element. Expanded over the emitted
                // steps — not `view.items` — because dedup renames and splices
                // change ids and parents during emission, and the wire list
                // must lie on the boundary step's own ancestry.
                let anchor = c
                    .kept_from
                    .as_ref()
                    .map(|k| turn_to_step.get(k).cloned().unwrap_or_else(|| k.clone()));
                let kept = expand_kept_steps(&steps, &by_id, &parents, anchor.as_deref());
                if !kept.is_empty()
                    && let Ok(v) = serde_json::to_value(&kept)
                {
                    extra.insert("kept".to_string(), v);
                }

                let mut step = Step {
                    step: StepIdentity {
                        id: step_id.clone(),
                        parents,
                        actor,
                        timestamp: c.timestamp.clone(),
                    },
                    change: HashMap::new(),
                    meta: None,
                };
                step.change.insert(
                    conv_artifact_key.clone(),
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.compact".to_string(),
                            extra,
                        }),
                    },
                );
                let (final_id, appended) = push_step(&mut steps, &mut by_id, step);
                // Later turns whose `parent_id` references the boundary resolve
                // through `turn_to_step`; map to the final (possibly renamed) id.
                turn_to_step.insert(c.id.clone(), final_id.clone());
                if appended {
                    last_step_id = Some(final_id.clone());
                    prev_anchor_step = Some(final_id);
                }
            }
        }
    }

    // The head is the last emitted step. Use `steps.last()` rather than
    // `last_step_id`: when the final item is a duplicate that `push_step`
    // drops, `last_step_id` regresses to the earlier step it collapsed
    // into, which would orphan any real step emitted after that earlier
    // step (e.g. an event between a turn and its replay) as a spurious
    // dead end. The last surviving step keeps the whole chain on the
    // head's ancestry.
    let head = steps.last().map(|s| s.step.id.clone()).unwrap_or_default();

    // Meta
    let title = config
        .title
        .clone()
        .unwrap_or_else(|| format!("{} session: {}", provider, id_prefix));

    let mut meta = PathMeta {
        title: Some(title),
        kind: Some(PATH_KIND_AGENT_CODING_SESSION.to_string()),
        source: view.provider_id.clone(),
        ..Default::default()
    };

    if !actors.is_empty() {
        meta.actors = Some(actors);
    }

    if !view.files_changed.is_empty()
        && let Ok(v) = serde_json::to_value(&view.files_changed)
    {
        meta.extra.insert("files_changed".to_string(), v);
    }

    // Carry `vcs_remote` (not representable on `Base`) under meta.extra.
    if let Some(remote) = view.base.as_ref().and_then(|b| b.vcs_remote.as_ref())
        && !meta.extra.contains_key("vcs_remote")
    {
        meta.extra.insert(
            "vcs_remote".to_string(),
            serde_json::Value::String(remote.clone()),
        );
    }

    // Project canonical session-level fields under well-known keys.
    if let Some(producer) = &view.producer
        && let Ok(v) = serde_json::to_value(producer)
    {
        meta.extra.insert("producer".to_string(), v);
    }

    Path {
        path: PathIdentity {
            id: path_id,
            base,
            head,
            graph_ref: None,
        },
        steps,
        meta: Some(meta),
    }
}

/// Step-space twin of [`crate::expand_kept`]: walk the emitted steps'
/// parent chain from the boundary's `parents` back to `anchor` (already
/// mapped through `turn_to_step`), collecting `conversation.append` step
/// ids oldest-first. Event steps are chain links, not kept turns — the walk
/// passes through them; hitting an earlier boundary (or running out of
/// parents) without reaching the anchor yields an empty list.
fn expand_kept_steps(
    steps: &[Step],
    by_id: &HashMap<String, usize>,
    boundary_parents: &[String],
    anchor: Option<&str>,
) -> Vec<String> {
    let Some(anchor) = anchor else {
        return Vec::new();
    };
    let mut kept: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut cur = boundary_parents.first().cloned();
    let mut found = false;
    while let Some(id) = cur {
        if !seen.insert(id.clone()) {
            break;
        }
        let Some(&idx) = by_id.get(&id) else { break };
        let step = &steps[idx];
        // A turn step carries file.write changes alongside its
        // conversation.append; `change` is a HashMap, so classify by the
        // step's conversation.* change specifically — taking whichever
        // structural iterates first made the walk abort on hash order.
        let change_type = step
            .change
            .values()
            .filter_map(|ch| ch.structural.as_ref())
            .map(|s| s.change_type.as_str())
            .find(|t| t.starts_with("conversation."));
        match change_type {
            Some("conversation.append") => {
                kept.push(id.clone());
                if id == anchor {
                    found = true;
                    break;
                }
            }
            Some("conversation.event") => {}
            _ => break,
        }
        cur = step.step.parents.first().cloned();
    }
    if !found {
        return Vec::new();
    }
    kept.reverse();
    kept
}

/// Splice a step into the linear parent chain. If `parents` chains onto the
/// immediately-preceding chain anchor — the previous turn (`prev_turn_step`)
/// or the previous turn-or-compaction (`prev_anchor_step`); the ordinary
/// linear case, including a root reached only after some leading events —
/// but other steps were emitted since (`last_step_id` differs), re-parent
/// onto that intervening chain so those steps land on the head's ancestry
/// instead of dangling as false dead ends. Compactions count as anchors for
/// the same reason turns do: `extract_conversation` resolves a spliced
/// parent back to the nearest turn/compaction, so derive must re-splice
/// from either or derive → extract → derive would drift. A genuine branch
/// (parent is an *earlier* step, not the immediately-preceding anchor) is
/// left untouched, so abandoned branches stay dead ends.
fn splice_onto_intervening(
    mut parents: Vec<String>,
    prev_turn_step: &Option<String>,
    prev_anchor_step: &Option<String>,
    last_step_id: &Option<String>,
) -> Vec<String> {
    let resolved = parents.first().cloned();
    let chains_onto_prev = resolved == *prev_turn_step
        || (prev_anchor_step.is_some() && resolved == *prev_anchor_step);
    if chains_onto_prev
        && let Some(last) = last_step_id
        && Some(last) != resolved.as_ref()
    {
        parents = vec![last.clone()];
    }
    parents
}

/// Push `step` into `steps`, resolving an id collision with an
/// already-emitted step. A byte-identical re-emission (same id, parents,
/// actor, timestamp, change) is dropped — keeping it would only duplicate a
/// step that already exists — and a same-id-but-different step is re-IDed to a
/// fresh `<id>#<n>` so the original id stays recoverable and no data is lost.
/// Returns the id the step ended up under (the surviving id when dropped, the
/// new id when re-IDed), which the caller records in `turn_to_step` /
/// `last_step_id` so the DAG keeps pointing at a real step.
/// Returns the step's final id plus whether it was actually appended —
/// `false` means a byte-identical re-emission was dropped. Callers must not
/// advance their stream position (`last_step_id`/`prev_turn_step`) on a
/// drop: the duplicate contributes nothing to the stream, and regressing
/// the position would bypass whatever was emitted between the original and
/// the replay (e.g. a compaction boundary), orphaning it as a false dead
/// end and mis-parenting later steps.
fn push_step(
    steps: &mut Vec<Step>,
    by_id: &mut HashMap<String, usize>,
    mut step: Step,
) -> (String, bool) {
    let id = step.step.id.clone();
    let Some(&existing) = by_id.get(&id) else {
        by_id.insert(id.clone(), steps.len());
        steps.push(step);
        return (id, true);
    };
    if steps_content_eq(&steps[existing], &step) {
        return (id, false);
    }
    let mut n = 2u32;
    let mut renamed = format!("{id}#{n}");
    while by_id.contains_key(&renamed) {
        n += 1;
        renamed = format!("{id}#{n}");
    }
    step.step.id = renamed.clone();
    by_id.insert(renamed.clone(), steps.len());
    steps.push(step);
    (renamed, true)
}

/// Whether two steps are the same entry — equal once serialized, so dropping
/// one is lossless. Step doesn't implement `PartialEq`, and serializing only
/// happens on an actual id collision (rare), so the cost is negligible.
/// Wire-level replays never reach this comparison: they are recognized at the
/// source level (`seen_turn_sources`/`seen_event_sources`) before resolution,
/// because resolved forms are not comparable across the stream — splicing
/// rewires `parents`, and parent mappings mutate as colliding steps rename.
fn steps_content_eq(a: &Step, b: &Step) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}

fn actor_for_turn(turn: &Turn, provider: &str) -> String {
    match &turn.role {
        Role::User => "human:user".to_string(),
        Role::Assistant if turn.model.as_deref() == Some("<synthetic>") => {
            format!("tool:{}", provider)
        }
        Role::Assistant => {
            let model = turn.model.as_deref().unwrap_or("unknown");
            format!("agent:{}", model)
        }
        Role::System => format!("tool:{}", provider),
        Role::Other(_) => format!("tool:{}", provider),
    }
}

fn record_actor(
    actors: &mut HashMap<String, ActorDefinition>,
    actor: &str,
    turn: &Turn,
    provider: &str,
    _view: &ConversationView,
) {
    if actors.contains_key(actor) {
        return;
    }
    let def = if let Some(rest) = actor.strip_prefix("agent:") {
        ActorDefinition {
            name: Some(rest.to_string()),
            provider: Some(provider.to_string()),
            model: turn.model.clone(),
            identities: vec![],
            keys: vec![],
        }
    } else if let Some(rest) = actor.strip_prefix("human:") {
        ActorDefinition {
            name: Some(rest.to_string()),
            ..Default::default()
        }
    } else {
        let name = actor.split_once(':').map(|x| x.1).unwrap_or("").to_string();
        ActorDefinition {
            name: Some(name),
            provider: Some(provider.to_string()),
            ..Default::default()
        }
    };
    actors.insert(actor.to_string(), def);
}

fn extract_file_path(tool: &ToolInvocation) -> Option<String> {
    for field in &["file_path", "path", "filename", "file"] {
        if let Some(v) = tool.input.get(*field)
            && let Some(s) = v.as_str()
        {
            return Some(s.to_string());
        }
    }
    None
}

/// Build `(raw_diff, extra)` for a single FileWrite tool invocation.
///
/// See [`file_write_diff`] for the input shapes handled; this helper
/// additionally captures the structured before/after strings in `extra`.
///
/// `before_state` is threaded through to [`file_write_diff`] for the
/// `Write { content }` shape: when `Some`, it becomes the pre-image and
/// is also recorded in `extra["before"]`. When `None`, the diff falls
/// back to an empty pre-image (addition-only hunk).
fn file_write_change(
    tool: &ToolInvocation,
    path: &str,
    before_state: Option<&str>,
) -> (Option<String>, HashMap<String, serde_json::Value>) {
    let input = &tool.input;
    let str_field = |k: &str| input.get(k).and_then(|v| v.as_str()).map(str::to_string);

    let mut extra: HashMap<String, serde_json::Value> = HashMap::new();

    if let (Some(old), Some(new)) = (str_field("old_string"), str_field("new_string")) {
        extra.insert("before".to_string(), serde_json::Value::String(old.clone()));
        extra.insert("after".to_string(), serde_json::Value::String(new.clone()));
    } else if let Some(content) = str_field("content") {
        if let Some(before) = before_state {
            extra.insert(
                "before".to_string(),
                serde_json::Value::String(before.to_string()),
            );
        }
        extra.insert("after".to_string(), serde_json::Value::String(content));
    }
    // Multi-edit shapes (`edits: [...]`) contribute only the raw diff: the
    // per-edit pairs are provider-shaped JSON that `FileMutation` cannot
    // carry, so copying them onto the wire made the first round-trip
    // silently degrade the document. The full input still rides the
    // `tool.invoke` step.

    (
        file_write_diff(&tool.name, input, path, before_state),
        extra,
    )
}

/// Compute a unified diff string for a file-write tool invocation, given the
/// raw tool input JSON. Handles Claude's Edit / Write / MultiEdit / NotebookEdit
/// shapes; returns `None` for any unrecognised shape or if nothing to diff.
///
/// Exposed so non-Conversation derivers (e.g. `toolpath-claude`'s bespoke
/// Claude-JSONL deriver, which emits its own `tool.invoke` steps) can populate
/// `ArtifactChange.raw` without reimplementing the diff logic.
///
/// Shapes handled:
///   - `Edit    { old_string, new_string, ... }`  → diff old→new
///   - `Write   { content }`                      → diff `before_state`→content
///     (uses `""` when `before_state` is `None`, producing an addition-only hunk)
///   - `MultiEdit { edits: [{old_string, new_string}, ...] }` → hunks joined,
///     each prefixed with `# edit N/total` so consumers can tell them apart.
///
/// # `before_state` for `Write`
///
/// The `Write` tool replaces a file's whole contents but the JSONL log
/// doesn't carry the prior state. Callers that can reconstruct it
/// out-of-band (e.g. by reading `git show HEAD:<path>`) should pass it
/// as `before_state`; the resulting diff shows honest `-`/`+` lines for
/// replaced content. When `None`, we fall back to diffing against the
/// empty string — correct for new files, misleading for overwrites, but
/// the best we can do from the log alone.
///
/// `before_state` is ignored for `Edit` / `MultiEdit` shapes, which
/// already carry their own `old_string`/`new_string` pre-image.
pub fn file_write_diff(
    tool_name: &str,
    input: &serde_json::Value,
    path: &str,
    before_state: Option<&str>,
) -> Option<String> {
    let str_field = |k: &str| input.get(k).and_then(|v| v.as_str());

    // Edit / NotebookEdit / anything else with old/new pair.
    if let (Some(old), Some(new)) = (str_field("old_string"), str_field("new_string")) {
        return Some(unified_diff(path, old, new));
    }

    // Write — whole-file content; diff against the caller-supplied
    // before-state when present, else empty (addition-only hunk).
    if let Some(content) = str_field("content") {
        let before = before_state.unwrap_or("");
        return Some(unified_diff(path, before, content));
    }

    // MultiEdit — multiple sequential edits on one file.
    if let Some(edits) = input.get("edits").and_then(|v| v.as_array()) {
        if edits.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        for (idx, edit) in edits.iter().enumerate() {
            let old = edit
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new = edit
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let header = format!("# edit {}/{}", idx + 1, edits.len());
            parts.push(format!("{header}\n{}", unified_diff(path, old, new)));
        }
        return Some(parts.join("\n"));
    }

    // Unused today, but keeps `tool_name` addressable for future per-tool
    // branches (e.g. NotebookEdit may one day need cell-scoped diffs).
    let _ = tool_name;
    None
}

/// Produce a minimal unified-diff string using `similar::TextDiff`.
///
/// Always emits a `--- a/{path}` / `+++ b/{path}` header even when one side is
/// empty so downstream renderers can anchor the change to the file it touched.
///
/// Any leading `/` on `path` is stripped before splicing into the header —
/// git-style `a/` and `b/` prefixes already denote the repo root, so an
/// absolute path like `/abs/file.rs` would otherwise emit `--- a//abs/file.rs`,
/// which breaks `patch(1)` and other consumers that parse the header.
pub fn unified_diff(path: &str, before: &str, after: &str) -> String {
    use similar::TextDiff;
    let diff = TextDiff::from_lines(before, after);
    let display = path.trim_start_matches('/');
    let mut out = String::new();
    out.push_str(&format!("--- a/{display}\n+++ b/{display}\n"));
    out.push_str(
        &diff
            .unified_diff()
            .context_radius(3)
            .header("", "")
            .to_string(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Compaction, CompactionTrigger, DelegatedWork, EnvironmentSnapshot, TokenUsage, ToolResult,
    };

    fn base_turn(id: &str, role: Role) -> Turn {
        Turn {
            id: id.to_string(),
            parent_id: None,
            group_id: None,
            role,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            text: String::new(),
            thinking: None,
            tool_uses: vec![],
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: Vec::new(),
        }
    }

    fn view_with(turns: Vec<Turn>) -> ConversationView {
        ConversationView {
            id: "abcdef012345".to_string(),
            items: turns.into_iter().map(Item::Turn).collect(),
            provider_id: Some("pi".to_string()),
            ..Default::default()
        }
    }

    fn conv_change(step: &Step) -> &StructuralChange {
        let key = step
            .change
            .keys()
            .find(|k| k.contains("://"))
            .expect("conversation artifact key present");
        step.change[key].structural.as_ref().unwrap()
    }

    #[test]
    fn test_empty_view() {
        let view = view_with(vec![]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(path.steps.is_empty());
        assert_eq!(path.path.head, "");
    }

    #[test]
    fn test_meta_kind_is_convo() {
        let view = view_with(vec![base_turn("t1", Role::User)]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(
            path.meta.as_ref().unwrap().kind.as_deref(),
            Some(PATH_KIND_AGENT_CODING_SESSION)
        );
        // ...and survives a JSON round-trip.
        let json = serde_json::to_string(&path).unwrap();
        assert!(json.contains(&format!(r#""kind":"{PATH_KIND_AGENT_CODING_SESSION}""#)));
    }

    #[test]
    fn test_token_usage_breakdowns_round_trip() {
        use std::collections::BTreeMap;
        // A Turn whose token_usage carries breakdowns should derive into a
        // Path and extract back out with the breakdowns intact.
        let mut breakdowns = BTreeMap::new();
        breakdowns.insert(
            "output".to_string(),
            BTreeMap::from([("reasoning".to_string(), 450u32)]),
        );
        let mut turn = base_turn("t1", Role::Assistant);
        turn.model = Some("claude-opus-4-7".into());
        turn.token_usage = Some(TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(900),
            breakdowns: breakdowns.clone(),
            ..Default::default()
        });
        let view = view_with(vec![turn]);

        let path = derive_path(&view, &DeriveConfig::default());
        let extracted = crate::extract::extract_conversation(&path);

        let usage = extracted
            .turns()
            .next()
            .unwrap()
            .token_usage
            .as_ref()
            .expect("token_usage survives round-trip");
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(900));
        assert_eq!(usage.breakdowns, breakdowns);
        assert_eq!(usage.breakdowns["output"]["reasoning"], 450);
    }

    #[test]
    fn test_token_usage_empty_breakdowns_omitted_in_json() {
        // skip_serializing_if guarantees no "breakdowns" key for the empty map,
        // keeping the wire format byte-compatible with pre-breakdowns producers.
        let usage = TokenUsage {
            input_tokens: Some(10),
            output_tokens: Some(20),
            ..Default::default()
        };
        let json = serde_json::to_string(&usage).unwrap();
        assert!(
            !json.contains("breakdowns"),
            "empty breakdowns must be omitted, got: {json}"
        );
    }

    #[test]
    fn test_token_usage_absent_breakdowns_defaults_empty() {
        // Deserializing an old-style token_usage object with no breakdowns key
        // yields an empty map (serde default).
        let usage: TokenUsage =
            serde_json::from_str(r#"{"input_tokens":10,"output_tokens":20}"#).unwrap();
        assert!(usage.breakdowns.is_empty());
    }

    #[test]
    fn test_single_user_turn() {
        let mut turn = base_turn("t1", Role::User);
        turn.text = "hello".into();
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps.len(), 1);
        assert_eq!(path.steps[0].step.actor, "human:user");
        assert_eq!(path.steps[0].step.id, "t1");
    }

    #[test]
    fn test_single_assistant_turn() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.model = Some("claude-opus-4-7".into());
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[0].step.actor, "agent:claude-opus-4-7");
    }

    #[test]
    fn test_assistant_without_model() {
        let turn = base_turn("t1", Role::Assistant);
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[0].step.actor, "agent:unknown");
    }

    #[test]
    fn test_mid_stream_replay_drop_keeps_stream_position() {
        // A byte-identical replay of `a` arrives AFTER a compaction that
        // chains onto `a`. Dropping the replay must not regress the stream
        // position back to `a`: the next turn still wires through the
        // boundary, and the boundary is not a dead end. (Claude chain
        // merges produce exactly this shape: rotated segments replay the
        // prefix verbatim with boundary entries in between.)
        let a = base_turn("a", Role::User);
        let replay = base_turn("a", Role::User);
        let c = Compaction {
            id: "c".into(),
            parent_id: Some("a".into()),
            timestamp: "2026-01-01T00:00:01Z".into(),
            trigger: None,
            summary: None,
            pre_tokens: None,
            kept_from: None,
        };
        let mut d = base_turn("d", Role::Assistant);
        d.parent_id = Some("a".into());

        let mut view = view_with(vec![a]);
        view.items.push(Item::Compaction(c));
        view.items.push(Item::Turn(replay));
        view.items.push(Item::Turn(d));

        let path = derive_path(&view, &DeriveConfig::default());
        let ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c", "d"], "replay dropped, order kept");
        assert_eq!(
            path.steps[2].step.parents,
            vec!["c".to_string()],
            "d splices through the boundary, not back onto a"
        );
        let dead = toolpath::v1::query::dead_ends(&path.steps, &path.path.head);
        assert!(
            dead.is_empty(),
            "no false dead ends: {:?}",
            dead.iter().map(|s| &s.step.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_duplicate_id_identical_content_is_dropped() {
        // The same turn id can appear twice with byte-identical content (a
        // Claude compaction replay re-emitting an unchanged message). The
        // re-emission is dropped: one step survives, and the conversation
        // head resolves to it.
        let mut first = base_turn("dup", Role::User);
        first.text = "same".into();
        let mid = base_turn("mid", Role::Assistant);
        let mut second = base_turn("dup", Role::User);
        second.text = "same".into();
        let view = view_with(vec![first, mid, second]);

        let path = derive_path(&view, &DeriveConfig::default());
        let ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(ids, vec!["dup", "mid"], "identical re-emission dropped");
        // Head is the last surviving step in document order, not the earlier
        // step the dropped duplicate collapsed into — so any real step after
        // that earlier one (here `mid`) stays on the head's ancestry.
        assert_eq!(path.path.head, "mid", "head is the last surviving step");
    }

    #[test]
    fn test_dropped_final_duplicate_keeps_compaction_on_head_ancestry() {
        // Regression for the head-regression bug: a compaction sits between a
        // turn and that turn's byte-identical replay at the end of the stream.
        // The replay is dropped, but `head` must remain the compaction (the
        // last surviving step) so the boundary is on the head's ancestry
        // rather than orphaned as a dead end.
        let mut a = base_turn("a", Role::User);
        a.text = "same".into();
        let c = Compaction {
            id: "c".into(),
            parent_id: Some("a".into()),
            timestamp: "2026-01-01T00:00:00Z".into(),
            ..Default::default()
        };
        let mut replay = base_turn("a", Role::User);
        replay.text = "same".into();

        let mut view = view_with(vec![a]);
        view.items.push(Item::Compaction(c));
        view.items.push(Item::Turn(replay));

        let path = derive_path(&view, &DeriveConfig::default());
        let ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c"], "byte-identical replay dropped");
        assert_eq!(path.path.head, "c", "head is the compaction, not turn a");
        assert_eq!(path.steps[1].step.parents, vec!["a".to_string()]);
    }

    #[test]
    fn test_duplicate_id_different_content_is_renamed() {
        // The same turn id with DIFFERENT content keeps both steps: the
        // collision is resolved by renaming the later one to a fresh id so the
        // path stays unique — never dropping data, never erroring.
        let mut first = base_turn("dup", Role::User);
        first.text = "original".into();
        let mid = base_turn("mid", Role::Assistant);
        let mut second = base_turn("dup", Role::User);
        second.text = "replayed".into();
        let view = view_with(vec![first, mid, second]);

        let path = derive_path(&view, &DeriveConfig::default());
        let ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["dup", "mid", "dup#2"],
            "differing duplicate re-IDed to `<id>#<n>`, not dropped"
        );
        assert_eq!(path.path.head, "dup#2", "head is the re-IDed final step");
        assert_eq!(
            conv_change(&path.steps[0]).extra["text"],
            serde_json::json!("original")
        );
        assert_eq!(
            conv_change(&path.steps[2]).extra["text"],
            serde_json::json!("replayed")
        );
    }

    #[test]
    fn test_renamed_duplicate_keeps_parent_references_correct() {
        // Resolving collisions inline (as steps are emitted) — not as a
        // post-pass — keeps parent references correct: a later turn whose
        // parent_id matches a renamed duplicate resolves to the RENAMED step,
        // not the first occurrence that kept the original id.
        let mut first = base_turn("dup", Role::User);
        first.text = "original".into();
        let mut second = base_turn("dup", Role::User); // re-IDed to dup#2
        second.text = "replayed".into();
        let mut child = base_turn("child", Role::Assistant);
        child.parent_id = Some("dup".into());
        let view = view_with(vec![first, second, child]);

        let path = derive_path(&view, &DeriveConfig::default());
        let ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(ids, vec!["dup", "dup#2", "child"]);
        assert_eq!(
            path.steps[2].step.parents,
            vec!["dup#2".to_string()],
            "child parents on the renamed later duplicate, not the first `dup`"
        );
    }

    #[test]
    fn test_duplicate_event_ids_are_resolved_to_unique_ids() {
        // The blocking case: Claude Code reuses `uuid` on attachment lines, so
        // two distinct events arrive with the same id. derive_path must still
        // yield unique step ids (consumers key on them, e.g. a UNIQUE index).
        let a = base_turn("t1", Role::User);
        let mut view = view_with(vec![a]);
        for v in ["v1", "v2"] {
            view.items.push(Item::Event(crate::ConversationEvent {
                id: "evt".into(), // same id, different content
                timestamp: "2026-01-01T00:00:00Z".into(),
                parent_id: None,
                event_type: "attachment".into(),
                data: std::collections::HashMap::from([("k".to_string(), serde_json::json!(v))]),
            }));
        }

        let path = derive_path(&view, &DeriveConfig::default());
        let ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
        let unique: std::collections::HashSet<&&str> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "step ids must be unique: {ids:?}");
        assert!(
            ids.contains(&"evt") && ids.contains(&"evt#2"),
            "both events survive with distinct ids: {ids:?}"
        );
    }

    #[test]
    fn test_replay_of_spliced_turn_is_still_dropped() {
        // The Claude chain-merge shape: `u1; event; a1(parent u1); replay of
        // a1`. Deriving splices a1 onto the event (parents = [event], with
        // `source_parent = u1` stamped), so the replay's bytes no longer
        // match the stored step verbatim. The comparison must see through
        // the splice artifacts and drop the replay — otherwise it gets
        // renamed-kept, becomes the head, and orphans the original turn and
        // its event as false dead ends.
        let u1 = base_turn("u1", Role::User);
        let event = crate::ConversationEvent {
            id: String::new(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            parent_id: None,
            event_type: "attachment".into(),
            data: std::collections::HashMap::new(),
        };
        let mut a1 = base_turn("a1", Role::Assistant);
        a1.parent_id = Some("u1".into());
        a1.text = "answer".into();
        let replay = a1.clone();

        let mut view = view_with(vec![u1]);
        view.items.push(Item::Event(event));
        view.items.push(Item::Turn(a1));
        view.items.push(Item::Turn(replay));

        let path = derive_path(&view, &DeriveConfig::default());
        let ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["u1", "event-0001", "a1"],
            "the byte-identical replay is dropped, not renamed-kept"
        );
        assert_eq!(path.path.head, "a1", "the original keeps the head");
        assert_eq!(
            path.steps[2].step.parents,
            vec!["event-0001".to_string()],
            "the original stays spliced onto the event"
        );
    }

    #[test]
    fn test_same_group_replay_keeps_group_usage() {
        // Replays are excluded from `turn_groups` in the prepass, so a
        // byte-identical same-group replay at the group tail must not eat
        // the once-per-group token stamp — the original tail still sees
        // itself as last of its group.
        let u = base_turn("u1", Role::User);
        let mut a = base_turn("a1", Role::Assistant);
        a.group_id = Some("g1".into());
        a.token_usage = Some(crate::TokenUsage {
            input_tokens: Some(5),
            output_tokens: Some(7),
            ..Default::default()
        });
        let replay = a.clone();
        let mut view = view_with(vec![u, a]);
        view.items.push(Item::Turn(replay));

        let path = derive_path(&view, &DeriveConfig::default());
        let ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(ids, vec!["u1", "a1"], "the replay is dropped");
        assert!(
            conv_change(&path.steps[1])
                .extra
                .contains_key("token_usage"),
            "group tail keeps its once-per-group usage stamp"
        );
    }

    #[test]
    fn test_replay_does_not_shift_idless_turn_ids() {
        // A skipped replay must not consume a synthesized-id slot: an
        // id-less turn after it derives the same `step-NNNN` id as it
        // would without the replay.
        let u = base_turn("u1", Role::User);
        let replay = u.clone();
        let mut idless = base_turn("", Role::Assistant);
        idless.text = "reply".into();
        let mut view = view_with(vec![u]);
        view.items.push(Item::Turn(replay));
        view.items.push(Item::Turn(idless));

        let path = derive_path(&view, &DeriveConfig::default());
        let ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(ids, vec!["u1", "step-0002"]);
    }

    #[test]
    fn test_system_role() {
        let turn = base_turn("t1", Role::System);
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[0].step.actor, "tool:pi");
    }

    #[test]
    fn test_other_role() {
        let turn = base_turn("t1", Role::Other("tool".into()));
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[0].step.actor, "tool:pi");
    }

    #[test]
    fn test_parent_id_preserved() {
        let t1 = base_turn("t1", Role::User);
        let mut t2 = base_turn("t2", Role::Assistant);
        t2.parent_id = Some("t1".into());
        t2.model = Some("m".into());
        let view = view_with(vec![t1, t2]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[1].step.parents, vec!["t1".to_string()]);
    }

    #[test]
    fn derived_path_validates_against_base_schema() {
        let user = base_turn("t1", Role::User);
        let mut assistant = base_turn("t2", Role::Assistant);
        assistant.parent_id = Some("t1".into());
        assistant.model = Some("gpt-5.5".into());
        let system = base_turn("t3", Role::System);
        let other = base_turn("t4", Role::Other("bash".into()));

        let mut view = view_with(vec![user, assistant, system, other]);
        view.items.push(Item::Event(crate::ConversationEvent {
            id: "e1".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            parent_id: None,
            event_type: "attachment".into(),
            data: HashMap::new(),
        }));

        let path = derive_path(&view, &DeriveConfig::default());
        let graph = serde_json::json!({
            "graph": { "id": "g1" },
            "paths": [serde_json::to_value(&path).unwrap()],
        });

        let schema: serde_json::Value = serde_json::from_str(toolpath::SCHEMA_JSON).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&graph)
            .map(|e| format!("at {}: {e}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "base-schema violations:\n{}",
            errors.join("\n")
        );
    }

    #[test]
    fn derived_path_conforms_to_agent_coding_session_kind() {
        // derive_path stamps meta.kind = agent-coding-session, so its output
        // must satisfy that kind's schema. This view exercises every shape
        // the kind constrains: each turn role, a tool call with a result, a
        // file mutation, a delegation, token usage, environment, and an event.
        let mut user = base_turn("t1", Role::User);
        user.text = "implement the feature".into();

        let mut assistant = base_turn("t2", Role::Assistant);
        assistant.parent_id = Some("t1".into());
        assistant.group_id = Some("msg_t2".into());
        assistant.model = Some("gpt-5.5".into());
        assistant.text = "on it".into();
        assistant.thinking = Some("plan the edit".into());
        assistant.stop_reason = Some("tool_use".into());
        assistant.token_usage = Some(TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(20),
            cache_read_tokens: Some(50),
            cache_write_tokens: None,
            ..Default::default()
        });
        assistant.attributed_token_usage = Some(TokenUsage {
            output_tokens: Some(20),
            ..Default::default()
        });
        assistant.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/repo".into()),
            vcs_branch: Some("main".into()),
            vcs_revision: None,
        });
        assistant.tool_uses = vec![ToolInvocation {
            id: "call-1".into(),
            name: "write_file".into(),
            input: serde_json::json!({ "file_path": "a.rs", "content": "fn main() {}" }),
            result: Some(ToolResult {
                content: "ok".into(),
                is_error: false,
            }),
            category: Some(crate::ToolCategory::FileWrite),
        }];
        assistant.file_mutations = vec![crate::FileMutation {
            path: "a.rs".into(),
            tool_id: Some("call-1".into()),
            operation: Some("add".into()),
            raw_diff: Some("@@ -0,0 +1 @@\n+fn main() {}".into()),
            before: None,
            after: Some("fn main() {}".into()),
            rename_to: None,
        }];
        assistant.delegations = vec![DelegatedWork {
            agent_id: "sub-1".into(),
            prompt: "do the subtask".into(),
            turns: vec![],
            result: Some("done".into()),
        }];

        let mut system = base_turn("t3", Role::System);
        system.parent_id = Some("t2".into());
        system.text = "system note".into();

        let mut other = base_turn("t4", Role::Other("tool".into()));
        other.parent_id = Some("t3".into());
        other.text = "tool output".into();

        let mut view = view_with(vec![user, assistant, system, other]);
        view.items.push(Item::Event(crate::ConversationEvent {
            id: "e1".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            parent_id: None,
            event_type: "attachment".into(),
            data: HashMap::new(),
        }));

        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(
            path.meta.as_ref().and_then(|m| m.kind.as_deref()),
            Some(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION),
            "derive_path must stamp the agent-coding-session kind"
        );

        // Validate against the schema for the version the constant names, so
        // this test tracks the current kind without a spelled-out version.
        let version = PATH_KIND_AGENT_CODING_SESSION
            .rsplit('/')
            .next()
            .expect("kind URI has a version segment");
        let schema_src = std::fs::read_to_string(format!(
            "{}/../path-cli/kinds/agent-coding-session/{version}/schema.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("read kind schema");
        let schema: serde_json::Value = serde_json::from_str(&schema_src).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let value = serde_json::to_value(&path).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|e| format!("at {}: {e}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "kind-schema violations:\n{}",
            errors.join("\n")
        );
    }

    fn fw_tool(name: &str, id: &str, input: serde_json::Value) -> ToolInvocation {
        ToolInvocation {
            id: id.to_string(),
            name: name.to_string(),
            input,
            result: None,
            category: Some(ToolCategory::FileWrite),
        }
    }

    #[test]
    fn test_tool_use_filewrite_with_file_path_field() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool(
            "Write",
            "tu1",
            serde_json::json!({"file_path": "src/main.rs"}),
        )];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(path.steps[0].change.contains_key("src/main.rs"));
        let sc = path.steps[0].change["src/main.rs"]
            .structural
            .as_ref()
            .unwrap();
        assert_eq!(sc.change_type, "file.write");
        assert_eq!(sc.extra["tool"], serde_json::json!("Write"));
        assert_eq!(sc.extra["tool_id"], serde_json::json!("tu1"));
    }

    #[test]
    fn test_tool_use_filewrite_with_path_field() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool("Edit", "tu1", serde_json::json!({"path": "a.rs"}))];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(path.steps[0].change.contains_key("a.rs"));
    }

    #[test]
    fn test_tool_use_filewrite_with_filename_field() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool("W", "tu1", serde_json::json!({"filename": "b.rs"}))];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(path.steps[0].change.contains_key("b.rs"));
    }

    #[test]
    fn test_tool_use_filewrite_with_file_field() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool("W", "tu1", serde_json::json!({"file": "c.rs"}))];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(path.steps[0].change.contains_key("c.rs"));
    }

    #[test]
    fn test_tool_use_filewrite_no_recognized_field() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool("W", "tu1", serde_json::json!({"other": "foo"}))];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[0].change.len(), 1);
        let sc = conv_change(&path.steps[0]);
        assert!(sc.extra.contains_key("tool_uses"));
    }

    #[test]
    fn test_tool_use_non_filewrite_ignored() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![ToolInvocation {
            id: "tu1".into(),
            name: "Read".into(),
            input: serde_json::json!({"file_path": "x.rs"}),
            result: None,
            category: Some(ToolCategory::FileRead),
        }];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(!path.steps[0].change.contains_key("x.rs"));
        assert_eq!(path.steps[0].change.len(), 1);
    }

    #[test]
    fn test_tool_use_edit_emits_unified_diff() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool(
            "Edit",
            "tu1",
            serde_json::json!({
                "file_path": "src/login.rs",
                "old_string": "validate_token()",
                "new_string": "validate_token_v2()",
            }),
        )];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let ch = &path.steps[0].change["src/login.rs"];
        let raw = ch.raw.as_deref().expect("edit should emit unified diff");
        assert!(raw.contains("--- a/src/login.rs"));
        assert!(raw.contains("+++ b/src/login.rs"));
        assert!(raw.contains("-validate_token()"));
        assert!(raw.contains("+validate_token_v2()"));
        let sc = ch.structural.as_ref().unwrap();
        assert_eq!(sc.extra["before"], serde_json::json!("validate_token()"));
        assert_eq!(sc.extra["after"], serde_json::json!("validate_token_v2()"));
    }

    #[test]
    fn test_tool_use_write_emits_full_content_diff() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool(
            "Write",
            "tu1",
            serde_json::json!({
                "file_path": "hello.txt",
                "content": "hi\nthere\n",
            }),
        )];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let ch = &path.steps[0].change["hello.txt"];
        let raw = ch.raw.as_deref().expect("write should emit diff");
        assert!(raw.contains("+hi"));
        assert!(raw.contains("+there"));
        let sc = ch.structural.as_ref().unwrap();
        assert_eq!(sc.extra["after"], serde_json::json!("hi\nthere\n"));
        assert!(!sc.extra.contains_key("before"));
    }

    #[test]
    fn test_file_write_diff_write_without_before_state_is_addition_only() {
        // Backwards-compatible fallback: `None` → diff against "".
        let input = serde_json::json!({
            "file_path": "hello.txt",
            "content": "hi\nthere\n",
        });
        let raw =
            file_write_diff("Write", &input, "hello.txt", None).expect("write should emit diff");
        assert!(raw.contains("+hi"));
        assert!(raw.contains("+there"));
        // No `-` lines — nothing was there before.
        assert!(
            !raw.lines()
                .any(|l| l.starts_with('-') && !l.starts_with("---"))
        );
    }

    #[test]
    fn test_file_write_diff_write_with_before_state_shows_replacement() {
        let input = serde_json::json!({
            "file_path": "hello.txt",
            "content": "hi\nthere\n",
        });
        let raw = file_write_diff("Write", &input, "hello.txt", Some("bye\nfriend\n"))
            .expect("write should emit diff");
        // Before content should appear as removals.
        assert!(raw.contains("-bye"));
        assert!(raw.contains("-friend"));
        // After content should appear as additions.
        assert!(raw.contains("+hi"));
        assert!(raw.contains("+there"));
    }

    #[test]
    fn test_file_write_diff_before_state_ignored_for_edit_shape() {
        // `Edit` has its own `old_string`; supplied before_state should
        // be ignored.
        let input = serde_json::json!({
            "file_path": "a.rs",
            "old_string": "foo",
            "new_string": "bar",
        });
        let raw = file_write_diff("Edit", &input, "a.rs", Some("something else entirely"))
            .expect("edit should emit diff");
        assert!(raw.contains("-foo"));
        assert!(raw.contains("+bar"));
        assert!(!raw.contains("something else entirely"));
    }

    #[test]
    fn test_unified_diff_strips_leading_slash_on_absolute_path() {
        // Regression for #36: headers for absolute paths must not contain `a//`.
        let raw = unified_diff("/abs/path.rs", "a\n", "b\n");
        assert!(
            raw.contains("--- a/abs/path.rs\n"),
            "missing stripped --- header: {raw}"
        );
        assert!(
            raw.contains("+++ b/abs/path.rs\n"),
            "missing stripped +++ header: {raw}"
        );
        assert!(
            !raw.contains("a//"),
            "header should not contain doubled slash: {raw}"
        );
        assert!(
            !raw.contains("b//"),
            "header should not contain doubled slash: {raw}"
        );
    }

    #[test]
    fn test_unified_diff_preserves_relative_path() {
        // Relative paths (no leading slash) are unchanged — only a single
        // leading `/` is stripped.
        let raw = unified_diff("src/login.rs", "a\n", "b\n");
        assert!(raw.contains("--- a/src/login.rs\n"), "{raw}");
        assert!(raw.contains("+++ b/src/login.rs\n"), "{raw}");
    }

    #[test]
    fn test_tool_use_multiedit_emits_per_hunk_diff() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![fw_tool(
            "MultiEdit",
            "tu1",
            serde_json::json!({
                "file_path": "m.rs",
                "edits": [
                    {"old_string": "foo", "new_string": "bar"},
                    {"old_string": "baz", "new_string": "qux"},
                ],
            }),
        )];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let ch = &path.steps[0].change["m.rs"];
        let raw = ch.raw.as_deref().expect("multiedit should emit diff");
        assert!(raw.contains("# edit 1/2"));
        assert!(raw.contains("# edit 2/2"));
        assert!(raw.contains("-foo"));
        assert!(raw.contains("+bar"));
        assert!(raw.contains("-baz"));
        assert!(raw.contains("+qux"));
    }

    #[test]
    fn test_thinking_included_when_enabled() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.thinking = Some("hmm".into());
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let sc = conv_change(&path.steps[0]);
        assert_eq!(sc.extra["thinking"], serde_json::json!("hmm"));
    }

    #[test]
    fn test_thinking_omitted_when_disabled() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.thinking = Some("hmm".into());
        let view = view_with(vec![turn]);
        let cfg = DeriveConfig {
            include_thinking: false,
            ..Default::default()
        };
        let path = derive_path(&view, &cfg);
        let sc = conv_change(&path.steps[0]);
        assert!(!sc.extra.contains_key("thinking"));
    }

    #[test]
    fn test_tool_uses_included_when_enabled() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![ToolInvocation {
            id: "tu1".into(),
            name: "Read".into(),
            input: serde_json::json!({}),
            result: Some(ToolResult {
                content: "x".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileRead),
        }];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let sc = conv_change(&path.steps[0]);
        assert!(sc.extra.contains_key("tool_uses"));
    }

    #[test]
    fn test_tool_uses_omitted_when_disabled() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.tool_uses = vec![ToolInvocation {
            id: "tu1".into(),
            name: "Read".into(),
            input: serde_json::json!({}),
            result: None,
            category: Some(ToolCategory::FileRead),
        }];
        let view = view_with(vec![turn]);
        let cfg = DeriveConfig {
            include_tool_uses: false,
            ..Default::default()
        };
        let path = derive_path(&view, &cfg);
        let sc = conv_change(&path.steps[0]);
        assert!(!sc.extra.contains_key("tool_uses"));
    }

    #[test]
    fn test_base_uri_from_working_dir() {
        let mut turn = base_turn("t1", Role::User);
        turn.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/Users/alex/proj".into()),
            ..Default::default()
        });
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.path.base.unwrap().uri, "file:///Users/alex/proj");
    }

    #[test]
    fn test_base_uri_from_config_override() {
        let mut turn = base_turn("t1", Role::User);
        turn.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/Users/alex/proj".into()),
            ..Default::default()
        });
        let view = view_with(vec![turn]);
        let cfg = DeriveConfig {
            base_uri: Some("github:org/repo".into()),
            ..Default::default()
        };
        let path = derive_path(&view, &cfg);
        assert_eq!(path.path.base.unwrap().uri, "github:org/repo");
    }

    #[test]
    fn test_base_uri_absent_when_no_source() {
        let turn = base_turn("t1", Role::User);
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert!(path.path.base.is_none());
    }

    #[test]
    fn test_path_id_from_config_override() {
        let view = view_with(vec![]);
        let cfg = DeriveConfig {
            path_id: Some("my-custom-id".into()),
            ..Default::default()
        };
        let path = derive_path(&view, &cfg);
        assert_eq!(path.path.id, "my-custom-id");
    }

    #[test]
    fn test_path_id_default_format() {
        let view = view_with(vec![]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.path.id, "path-pi-abcdef01");
    }

    #[test]
    fn test_files_changed_in_meta() {
        let mut view = view_with(vec![]);
        view.files_changed = vec!["a.rs".into(), "b.rs".into()];
        let path = derive_path(&view, &DeriveConfig::default());
        let meta = path.meta.unwrap();
        assert_eq!(
            meta.extra["files_changed"],
            serde_json::json!(["a.rs", "b.rs"])
        );
    }

    #[test]
    fn test_actors_in_meta() {
        let u = base_turn("t1", Role::User);
        let mut a = base_turn("t2", Role::Assistant);
        a.model = Some("claude-opus-4-7".into());
        let view = view_with(vec![u, a]);
        let path = derive_path(&view, &DeriveConfig::default());
        let actors = path.meta.unwrap().actors.unwrap();
        assert!(actors.contains_key("human:user"));
        assert!(actors.contains_key("agent:claude-opus-4-7"));
        let agent = &actors["agent:claude-opus-4-7"];
        assert_eq!(agent.provider.as_deref(), Some("pi"));
        assert_eq!(agent.model.as_deref(), Some("claude-opus-4-7"));
        let human = &actors["human:user"];
        assert_eq!(human.name.as_deref(), Some("user"));
    }

    #[test]
    fn test_head_is_last_step_id() {
        let turns = vec![
            base_turn("t1", Role::User),
            base_turn("t2", Role::User),
            base_turn("t3", Role::User),
        ];
        let view = view_with(turns);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.path.head, "t3");
    }

    #[test]
    fn test_token_usage_in_extras() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.token_usage = Some(TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            cache_read_tokens: None,
            cache_write_tokens: None,
            ..Default::default()
        });
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let sc = conv_change(&path.steps[0]);
        assert!(sc.extra.contains_key("token_usage"));
        assert_eq!(
            sc.extra["token_usage"]["input_tokens"],
            serde_json::json!(100)
        );
    }

    fn usage(output: u32) -> TokenUsage {
        TokenUsage {
            input_tokens: Some(6),
            output_tokens: Some(output),
            cache_read_tokens: Some(14_842),
            cache_write_tokens: Some(429_831),
            ..Default::default()
        }
    }

    #[test]
    fn test_message_group_carries_usage_once_on_last_step() {
        // Three turns split from one provider message (Claude Code repeats
        // message.usage on every content-block line), then one singleton
        // message. Usage must land exactly once per group_id group — on
        // the group's last step — and group_id on every grouped step.
        let mut turns: Vec<Turn> = (1..=3)
            .map(|i| {
                let mut t = base_turn(&format!("t{i}"), Role::Assistant);
                t.group_id = Some("msg_01".into());
                t.token_usage = Some(usage(997));
                t
            })
            .collect();
        let mut t4 = base_turn("t4", Role::Assistant);
        t4.group_id = Some("msg_02".into());
        t4.token_usage = Some(usage(11));
        turns.push(t4);

        let view = view_with(turns);
        let path = derive_path(&view, &DeriveConfig::default());
        let changes: Vec<&StructuralChange> = path.steps.iter().map(conv_change).collect();

        assert!(!changes[0].extra.contains_key("token_usage"));
        assert!(!changes[1].extra.contains_key("token_usage"));
        assert_eq!(
            changes[2].extra["token_usage"]["output_tokens"],
            serde_json::json!(997)
        );
        assert_eq!(
            changes[3].extra["token_usage"]["output_tokens"],
            serde_json::json!(11)
        );
        for c in &changes[..3] {
            assert_eq!(c.extra["group_id"], serde_json::json!("msg_01"));
        }
        assert_eq!(changes[3].extra["group_id"], serde_json::json!("msg_02"));
    }

    #[test]
    fn test_turn_without_group_id_is_its_own_accounting_unit() {
        // Providers that never split a message (gemini, pi, opencode)
        // leave group_id unset; every turn keeps its own usage.
        let mut turns = Vec::new();
        for i in 1..=2 {
            let mut t = base_turn(&format!("t{i}"), Role::Assistant);
            t.token_usage = Some(usage(50 + i));
            turns.push(t);
        }
        let view = view_with(turns);
        let path = derive_path(&view, &DeriveConfig::default());
        for (i, step) in path.steps.iter().enumerate() {
            let sc = conv_change(step);
            assert_eq!(
                sc.extra["token_usage"]["output_tokens"],
                serde_json::json!(51 + i as u64)
            );
            assert!(!sc.extra.contains_key("group_id"));
        }
    }

    #[test]
    fn test_message_grouping_is_consecutive_only() {
        // A group_id reappearing after an intervening message starts a
        // new group (defensive: source formats never interleave, but the
        // rule is defined over consecutive runs in document order).
        let mk = |id: &str, msg: &str, out: u32| {
            let mut t = base_turn(id, Role::Assistant);
            t.group_id = Some(msg.into());
            t.token_usage = Some(usage(out));
            t
        };
        let view = view_with(vec![
            mk("t1", "msg_01", 100),
            mk("t2", "msg_02", 200),
            mk("t3", "msg_01", 300),
        ]);
        let path = derive_path(&view, &DeriveConfig::default());
        let changes: Vec<&StructuralChange> = path.steps.iter().map(conv_change).collect();
        assert_eq!(
            changes[0].extra["token_usage"]["output_tokens"],
            serde_json::json!(100)
        );
        assert_eq!(
            changes[1].extra["token_usage"]["output_tokens"],
            serde_json::json!(200)
        );
        assert_eq!(
            changes[2].extra["token_usage"]["output_tokens"],
            serde_json::json!(300)
        );
    }

    #[test]
    fn test_delegations_in_extras() {
        let mut turn = base_turn("t1", Role::Assistant);
        turn.delegations = vec![DelegatedWork {
            agent_id: "sub-1".into(),
            prompt: "do a thing".into(),
            turns: vec![],
            result: None,
        }];
        let view = view_with(vec![turn]);
        let path = derive_path(&view, &DeriveConfig::default());
        let sc = conv_change(&path.steps[0]);
        assert!(sc.extra.contains_key("delegations"));
        assert_eq!(
            sc.extra["delegations"][0]["agent_id"],
            serde_json::json!("sub-1")
        );
    }

    #[test]
    fn test_title_from_config() {
        let view = view_with(vec![]);
        let cfg = DeriveConfig {
            title: Some("My Session".into()),
            ..Default::default()
        };
        let path = derive_path(&view, &cfg);
        assert_eq!(path.meta.unwrap().title.as_deref(), Some("My Session"));
    }

    #[test]
    fn test_title_default_when_unset() {
        let view = view_with(vec![]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(
            path.meta.unwrap().title.as_deref(),
            Some("pi session: abcdef01")
        );
    }

    #[test]
    fn test_compaction_emits_compact_step_and_rewires_dag() {
        // [Turn(a), Compaction(c, parent=a), Turn(b, parent=c)] must derive
        // to three steps in order, with the middle step carrying a
        // `conversation.compact` change whose extras survive, parented on a,
        // and turn b rewired through the boundary onto c.
        let a = base_turn("a", Role::User);
        let mut b = base_turn("b", Role::Assistant);
        b.parent_id = Some("c".into());
        b.model = Some("m".into());

        let c = Compaction {
            id: "c".into(),
            parent_id: Some("a".into()),
            timestamp: "2026-01-01T00:00:00Z".into(),
            trigger: Some(CompactionTrigger::Manual),
            summary: Some("s".into()),
            pre_tokens: Some(100),
            kept_from: Some("a".into()),
        };

        let mut view = view_with(vec![a]);
        view.items.push(Item::Compaction(c));
        view.items.push(Item::Turn(b));

        let path = derive_path(&view, &DeriveConfig::default());

        let ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c", "b"], "items emitted in order");

        let compact = &path.steps[1];
        assert_eq!(compact.step.parents, vec!["a".to_string()]);
        let sc = conv_change(compact);
        assert_eq!(sc.change_type, "conversation.compact");
        assert_eq!(sc.extra["trigger"], serde_json::json!("manual"));
        assert_eq!(sc.extra["summary"], serde_json::json!("s"));
        assert_eq!(sc.extra["pre_tokens"], serde_json::json!(100));
        // `kept` on the wire is the parent-chain expansion of `kept_from`,
        // deduplicated by construction.
        assert_eq!(sc.extra["kept"], serde_json::json!(["a"]));

        // Turn b rewired through the boundary: its parent is the compact step.
        assert_eq!(path.steps[2].step.parents, vec!["c".to_string()]);
    }

    #[test]
    fn test_kept_expansion_survives_turns_with_file_mutations() {
        // Turns in the kept run whose steps carry sibling `file.write`
        // changes next to the `conversation.append`. `step.change` is a
        // HashMap, so classifying the step by whichever structural change
        // iterates first made the kept walk abort when a file change came
        // up before the conversation change — a per-process coin flip.
        // Looped: each iteration builds fresh maps with fresh hash keys,
        // so a hash-order regression fails the loop almost surely.
        for _ in 0..64 {
            let mutations = |n: usize| {
                (0..n)
                    .map(|i| crate::FileMutation {
                        path: format!("f{i}.txt"),
                        tool_id: None,
                        operation: Some("write".into()),
                        raw_diff: None,
                        before: None,
                        after: Some(format!("body-{i}")),
                        rename_to: None,
                    })
                    .collect::<Vec<_>>()
            };
            let mut t1 = base_turn("t1", Role::User);
            t1.file_mutations = mutations(3);
            let mut t2 = base_turn("t2", Role::Assistant);
            t2.parent_id = Some("t1".into());
            t2.model = Some("m".into());
            t2.file_mutations = mutations(2);
            let c = Compaction {
                id: "c".into(),
                parent_id: Some("t2".into()),
                timestamp: "2026-01-01T00:00:00Z".into(),
                trigger: Some(CompactionTrigger::Auto),
                summary: Some("s".into()),
                pre_tokens: None,
                kept_from: Some("t1".into()),
            };
            let mut t3 = base_turn("t3", Role::User);
            t3.parent_id = Some("c".into());

            let mut view = view_with(vec![t1, t2]);
            view.items.push(Item::Compaction(c));
            view.items.push(Item::Turn(t3));

            let path = derive_path(&view, &DeriveConfig::default());
            let sc = conv_change(&path.steps[2]);
            assert_eq!(sc.change_type, "conversation.compact");
            assert_eq!(sc.extra["kept"], serde_json::json!(["t1", "t2"]));
        }
    }

    #[test]
    fn test_compaction_omits_optional_extras_when_absent() {
        let a = base_turn("a", Role::User);
        let c = Compaction {
            id: "c".into(),
            parent_id: Some("a".into()),
            timestamp: "2026-01-01T00:00:00Z".into(),
            ..Default::default()
        };
        let mut view = view_with(vec![a]);
        view.items.push(Item::Compaction(c));

        let path = derive_path(&view, &DeriveConfig::default());
        let sc = conv_change(&path.steps[1]);
        assert_eq!(sc.change_type, "conversation.compact");
        assert!(!sc.extra.contains_key("trigger"));
        assert!(!sc.extra.contains_key("summary"));
        assert!(!sc.extra.contains_key("pre_tokens"));
        assert!(!sc.extra.contains_key("kept"));
    }

    #[test]
    fn test_compaction_synthesizes_id_when_empty() {
        let a = base_turn("a", Role::User);
        let c = Compaction {
            id: String::new(),
            parent_id: Some("a".into()),
            timestamp: "2026-01-01T00:00:00Z".into(),
            ..Default::default()
        };
        let mut view = view_with(vec![a]);
        view.items.push(Item::Compaction(c));

        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[1].step.id, "compact-0001");
    }

    #[test]
    fn test_compaction_with_unresolvable_parent_chains_onto_previous_step() {
        // A compaction whose parent_id resolves to no emitted step must chain
        // onto whatever step came before it (the same fallback events use),
        // not become a disconnected root that orphans the pre-compaction turns.
        let a = base_turn("a", Role::User);
        let c = Compaction {
            id: "c".into(),
            parent_id: Some("ghost".into()), // resolves to nothing
            timestamp: "2026-01-01T00:00:00Z".into(),
            ..Default::default()
        };
        let mut view = view_with(vec![a]);
        view.items.push(Item::Compaction(c));

        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.steps[1].step.id, "c");
        assert_eq!(
            path.steps[1].step.parents,
            vec!["a".to_string()],
            "unresolvable parent falls back to the previous step, not an empty (root) parent set"
        );
    }

    fn dead_end_ids(path: &Path) -> Vec<String> {
        toolpath::v1::query::dead_ends(&path.steps, &path.path.head)
            .iter()
            .map(|s| s.step.id.clone())
            .collect()
    }

    fn snapshot_event(id: &str) -> crate::ConversationEvent {
        crate::ConversationEvent {
            id: id.into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            parent_id: None,
            event_type: "file-history-snapshot".into(),
            data: HashMap::new(),
        }
    }

    #[test]
    fn test_events_and_compaction_land_on_head_ancestry() {
        // A linear stream (Codex/opencode shape): a leading event, an
        // interleaved event, and a compaction that only back-links to the
        // prior turn. All must end up on the head's ancestry — none flagged as
        // a false dead end (`query dead-ends` / `render dot` would otherwise
        // show them as abandoned).
        let mut a = base_turn("a", Role::User);
        a.text = "go".into();
        let mut b = base_turn("b", Role::Assistant);
        b.parent_id = Some("a".into());
        let mut c = base_turn("c", Role::Assistant);
        c.parent_id = Some("b".into());
        let cmp = Compaction {
            id: "cmp".into(),
            parent_id: Some("a".into()), // back-link only, as Codex/opencode emit
            timestamp: "2026-01-01T00:00:00Z".into(),
            ..Default::default()
        };

        let mut view = view_with(vec![]);
        view.items = vec![
            Item::Event(snapshot_event("lead-evt")),
            Item::Turn(a),
            Item::Event(snapshot_event("mid-evt")),
            Item::Compaction(cmp),
            Item::Turn(b),
            Item::Turn(c),
        ];

        let path = derive_path(&view, &DeriveConfig::default());
        assert!(
            dead_end_ids(&path).is_empty(),
            "events + compaction must be on the head's ancestry, got dead ends: {:?}",
            dead_end_ids(&path)
        );
    }

    #[test]
    fn test_splice_preserves_genuine_dead_end_branches() {
        // The splice must NOT swallow real branches: an abandoned turn that
        // forks off an earlier turn (not the previous one) stays a dead end.
        let a = base_turn("a", Role::User);
        let mut x = base_turn("x", Role::Assistant); // abandoned branch off a
        x.parent_id = Some("a".into());
        let mut b = base_turn("b", Role::Assistant); // main line off a
        b.parent_id = Some("a".into());
        let mut c = base_turn("c", Role::Assistant);
        c.parent_id = Some("b".into());

        let view = view_with(vec![a, x, b, c]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(
            dead_end_ids(&path),
            vec!["x".to_string()],
            "the abandoned branch must remain a dead end"
        );
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut t1 = base_turn("t1", Role::User);
        t1.text = "hello".into();
        t1.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/proj".into()),
            ..Default::default()
        });
        let mut t2 = base_turn("t2", Role::Assistant);
        t2.parent_id = Some("t1".into());
        t2.model = Some("m".into());
        t2.tool_uses = vec![fw_tool(
            "Write",
            "tu1",
            serde_json::json!({"file_path": "x.rs"}),
        )];

        let mut view = view_with(vec![t1, t2]);
        view.files_changed = vec!["x.rs".into()];

        let path = derive_path(&view, &DeriveConfig::default());
        let json = serde_json::to_string(&path).unwrap();
        let back: Path = serde_json::from_str(&json).unwrap();
        assert_eq!(back.path.id, path.path.id);
        assert_eq!(back.path.head, path.path.head);
        assert_eq!(back.steps.len(), 2);
        assert_eq!(back.steps[1].step.parents, vec!["t1".to_string()]);
        assert!(back.steps[1].change.contains_key("x.rs"));
    }
}
