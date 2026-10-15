//! Known defects, pinned as expected current behaviour. Each one is a
//! measurement over a set's outputs; the snapshot holds the measurement, so a
//! fix (or a regression on top of the defect) shows as a snapshot diff.

use std::collections::{BTreeMap, BTreeSet};

use crate::keypaths::{json_lines, raw_paths};
use crate::model::{Agent, Finding, KnownDefect, Set};
use crate::shed::renames;

/// The outputs a defect can be measured on.
pub struct Evidence<'a> {
    pub set: &'a Set,
    pub projections: &'a BTreeMap<Agent, String>,
    /// The set projected back to its own format.
    pub same_format: &'a str,
}

/// Which output a defect is measured on.
#[derive(Clone, Copy)]
enum Target {
    Projection(Agent),
    SameFormat,
}

impl Target {
    fn name(self) -> String {
        match self {
            Target::Projection(a) => a.name().to_owned(),
            Target::SameFormat => "self".to_owned(),
        }
    }
}

struct Defect {
    name: &'static str,
    set: &'static str,
    target: Target,
    measure: fn(&Evidence) -> Finding,
}

/// Every known defect, in the order they were found.
const DEFECTS: &[Defect] = &[
    // `toolpath-claude/src/project.rs::project_event` writes a foreign event's
    // raw `event_type` as the Claude entry's `type`, so codex -> claude output
    // carries codex's wire vocabulary that real Claude Code does not define.
    // Unchanged since the projector's first commit; no other test fails
    // because toolpath-claude's reader is lenient about `type`.
    Defect {
        name: "codex-to-claude-illegal-types",
        set: "codex",
        target: Target::Projection(Agent::Claude),
        measure: |e| illegal_claude_types(&e.projections[&Agent::Claude]),
    },
    // `path p export codex -o` writes the CALLER's cwd into every codex `cwd`
    // field instead of the source session's. The pipeline projects from
    // [`crate::pipeline::CALLER_CWD`], so the wrong value is that.
    Defect {
        name: "claude-to-codex-caller-cwd",
        set: "claude",
        target: Target::Projection(Agent::Codex),
        measure: |e| caller_cwd(&e.set.text(), &e.projections[&Agent::Codex]),
    },
    // The claude projection renames `message.stop_reason` (the API field
    // Claude Code reads) to `message.stopReason`; the docs
    // (jsonl-envelope.md) say `stopReason` is a different, hook-provided
    // envelope field. Pinned as every rename the same-format round trip makes.
    Defect {
        name: "claude-roundtrip-renames",
        set: "claude",
        target: Target::SameFormat,
        measure: |e| round_trip_renames(&e.set.text(), e.same_format),
    },
];

/// The defects that apply to `evidence.set`, measured.
pub fn known_defects(evidence: &Evidence) -> Vec<KnownDefect> {
    DEFECTS
        .iter()
        .filter(|d| d.set == evidence.set.name)
        .map(|d| KnownDefect {
            name: d.name,
            target: d.target.name(),
            finding: (d.measure)(evidence),
        })
        .collect()
}

/// Top-level `type` values documented for Claude Code
/// (docs/agents/formats/claude-code/entry-types.md).
const CLAUDE_TYPES: &[&str] = &[
    "user",
    "assistant",
    "system",
    "attachment",
    "file-history-snapshot",
    "permission-mode",
    "queue-operation",
    "last-prompt",
    "summary",
    "compact_boundary",
    "progress",
    "ai-title",
    "custom-title",
    "agent-name",
    "mode",
    "atis-latch",
    "pr-link",
    "frame-link",
    "relocated",
    "worktree-state",
];

/// Lines of a Claude projection whose `type` Claude Code does not define,
/// counted per type.
pub fn illegal_claude_types(projection: &str) -> Finding {
    let mut counts = BTreeMap::new();
    for v in json_lines(projection) {
        let t = v["type"].as_str().unwrap_or("<no type>");
        if !CLAUDE_TYPES.contains(&t) {
            *counts.entry(t.to_owned()).or_default() += 1;
        }
    }
    Finding::IllegalClaudeTypes { counts }
}

/// The session's own cwd (the first `cwd` in the transcript) against every
/// `payload.cwd` a codex projection writes.
pub fn caller_cwd(source: &str, projection: &str) -> Finding {
    let source_cwd = json_lines(source).find_map(|v| v["cwd"].as_str().map(str::to_owned));
    let written_cwd: BTreeSet<String> = json_lines(projection)
        .filter_map(|v| v["payload"]["cwd"].as_str().map(str::to_owned))
        .collect();
    Finding::CallerCwd {
        source_cwd,
        written_cwd,
    }
}

/// Every field the same-format round trip writes under the other spelling.
pub fn round_trip_renames(source: &str, back: &str) -> Finding {
    let (before, after) = (raw_paths(source), raw_paths(back));
    let lost: Vec<&String> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    let gained: Vec<&String> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    Finding::Renames {
        renames: renames(&lost, &gained),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rename;

    #[test]
    fn foreign_types_are_counted_and_documented_ones_are_not() {
        let text = "{\"type\":\"user\"}\n{\"type\":\"agent_message\"}\n{\"type\":\"agent_message\"}\n{\"x\":1}\n";
        let Finding::IllegalClaudeTypes { counts } = illegal_claude_types(text) else {
            unreachable!()
        };
        assert_eq!(counts.get("agent_message"), Some(&2));
        assert_eq!(counts.get("<no type>"), Some(&1));
        assert_eq!(counts.get("user"), None);
    }

    #[test]
    fn caller_cwd_pairs_the_source_cwd_with_every_written_one() {
        let source = "{\"type\":\"user\",\"cwd\":\"/work\"}\n";
        let projection = "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/\"}}\n{\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/\"}}\n";
        assert_eq!(
            caller_cwd(source, projection),
            Finding::CallerCwd {
                source_cwd: Some("/work".into()),
                written_cwd: BTreeSet::from(["/".to_owned()]),
            }
        );
    }

    #[test]
    fn a_rename_is_a_lost_path_written_again_in_the_other_spelling() {
        let source = "{\"message\":{\"stop_reason\":\"end\"}}\n";
        let renamed = "{\"message\":{\"stopReason\":\"end\"}}\n";
        assert_eq!(
            round_trip_renames(source, renamed),
            Finding::Renames {
                renames: vec![Rename {
                    from: "message.stop_reason".into(),
                    to: "message.stopReason".into()
                }]
            }
        );
        // Written under its own name again: no rename (the defect is fixed).
        assert_eq!(
            round_trip_renames(source, source),
            Finding::Renames { renames: vec![] }
        );
        // Written under neither name: no rename either; that is a dropped
        // field, which the shed snapshot shows.
        assert_eq!(
            round_trip_renames(source, "{\"message\":{}}\n"),
            Finding::Renames { renames: vec![] }
        );
    }

    #[test]
    fn only_the_defects_of_the_named_set_apply() {
        let set = Set::new(
            "codex",
            b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"x\"}}\n".to_vec(),
        )
        .unwrap();
        let projections =
            BTreeMap::from([(Agent::Claude, "{\"type\":\"token_count\"}\n".to_owned())]);
        let found = known_defects(&Evidence {
            set: &set,
            projections: &projections,
            same_format: "",
        });
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "codex-to-claude-illegal-types");
        assert_eq!(found[0].target, "claude");
    }
}
