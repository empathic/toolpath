//! Known defects, pinned as expected current behaviour. Each one is a
//! measurement over a set's outputs; the snapshot holds the measurement, so a
//! fix (or a regression on top of the defect) shows as a snapshot diff.

use std::collections::BTreeMap;

use serde_json::Value;
use toolpath_convo::ConversationView;

use crate::keypaths::{json_values, raw_paths};
use crate::model::{Agent, Finding, KnownDefect, Set};
use crate::shed::renames;

/// The outputs a defect can be measured on.
pub struct Evidence<'a> {
    pub set: &'a Set,
    /// The adapter's conversation view.
    pub ir: &'a ConversationView,
    /// The toolpath graph derived from the set.
    pub document: &'a Value,
    pub projections: &'a BTreeMap<Agent, String>,
    /// The set projected back to its own format.
    pub same_format: &'a str,
}

/// Which output a defect is measured on.
#[derive(Clone, Copy)]
#[expect(
    dead_code,
    reason = "no defect is open today; the next one pinned names its target"
)]
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

/// Every open known defect. None today: the last two
/// (`codex-to-claude-illegal-types`, `claude-roundtrip-renames`) were fixed in
/// toolpath-claude (pr/28) and the accounting three in pr/27. The measures
/// below stay for the next defect and for their own tests.
const DEFECTS: &[Defect] = &[];

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
    for v in json_values(projection) {
        let t = v["type"].as_str().unwrap_or("<no type>");
        if !CLAUDE_TYPES.contains(&t) {
            *counts.entry(t.to_owned()).or_default() += 1;
        }
    }
    Finding::IllegalClaudeTypes { counts }
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
    fn a_set_with_no_open_defect_measures_nothing() {
        let set = Set::new(
            "codex",
            b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"x\"}}\n".to_vec(),
        )
        .unwrap();
        let projections =
            BTreeMap::from([(Agent::Claude, "{\"type\":\"token_count\"}\n".to_owned())]);
        let found = known_defects(&Evidence {
            set: &set,
            ir: &ConversationView::default(),
            document: &Value::Null,
            projections: &projections,
            same_format: "",
        });
        assert!(found.is_empty());
    }
}
