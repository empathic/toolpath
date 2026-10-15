//! What a golden is: an input transcript from one agent, and what each stage
//! of toolpath's cross-harness pipeline produces from it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use toolpath_convo::ConversationView;

use crate::keypaths::KeyPath;

/// A harness that writes transcripts, and that a transcript can be projected
/// to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    /// Claude Code JSONL.
    Claude,
    /// A Codex rollout.
    Codex,
    /// A pi session file.
    Pi,
    /// A Copilot CLI `events.jsonl`.
    Copilot,
}

impl Agent {
    pub const ALL: [Agent; 4] = [Agent::Claude, Agent::Codex, Agent::Pi, Agent::Copilot];

    /// The name `path` uses: `path p export <name>`.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Pi => "pi",
            Agent::Copilot => "copilot",
        }
    }

    /// The toolpath crate that reads and writes this agent's transcripts.
    pub fn adapter_name(self) -> &'static str {
        match self {
            Agent::Claude => "toolpath_claude",
            Agent::Codex => "toolpath_codex",
            Agent::Pi => "toolpath_pi",
            Agent::Copilot => "toolpath_copilot",
        }
    }

    /// Every agent a transcript of `self` is projected to: all the others.
    pub fn targets(self) -> impl Iterator<Item = Agent> {
        Agent::ALL.into_iter().filter(move |a| *a != self)
    }

    /// From the first JSON line: a Codex rollout opens with `session_meta`,
    /// a Copilot session with `session.start`, a pi session with a `session`
    /// header, and every Claude Code line carries `sessionId`. `None` when
    /// the first line is none of these.
    pub fn detect(input: &[u8]) -> Option<Agent> {
        let first = std::str::from_utf8(input)
            .ok()?
            .lines()
            .find_map(|l| serde_json::from_str::<Value>(l).ok())?;
        match first["type"].as_str() {
            Some("session_meta") => Some(Agent::Codex),
            Some("session.start") => Some(Agent::Copilot),
            Some("session") if first.get("cwd").is_some() => Some(Agent::Pi),
            _ if first.get("sessionId").is_some() => Some(Agent::Claude),
            _ => None,
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A map keyed by agent, serialised with the agent's name as the key: JSON
/// (and insta's snapshot content) take only string keys.
fn by_agent_name<S, V>(map: &BTreeMap<Agent, V>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
    V: Serialize,
{
    serializer.collect_map(map.iter().map(|(agent, v)| (agent.name(), v)))
}

/// One golden set: a named input transcript.
#[derive(Clone, Debug)]
pub struct Set {
    pub name: String,
    pub agent: Agent,
    /// The transcript, byte for byte.
    pub input: Vec<u8>,
}

impl Set {
    /// A set from its bytes; the agent is detected from the first line.
    pub fn new(name: impl Into<String>, input: Vec<u8>) -> Result<Set, String> {
        let name = name.into();
        let agent = Agent::detect(&input)
            .ok_or_else(|| format!("{name}: the first line names no known agent"))?;
        Ok(Set { name, agent, input })
    }

    /// `goldens/<name>/input.jsonl`: the set is named after its directory.
    pub fn from_input_file(path: &Path) -> Result<Set, String> {
        let name = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                format!(
                    "{}: no parent directory to name the set after",
                    path.display()
                )
            })?;
        let input = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Set::new(name, input)
    }

    /// sha256 of the input, hex: the identity of the bytes that went in.
    pub fn input_sha256(&self) -> String {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(&self.input))
    }

    /// The transcript as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.input).into_owned()
    }
}

/// Everything the pipeline produces from one set, in the order it runs.
#[derive(Clone, Debug, Serialize)]
pub struct Stages {
    /// The IR the agent's adapter derives from the transcript
    /// (`provider::to_view`).
    pub ir: ConversationView,
    /// The toolpath graph `path p derive` prints: what every projection
    /// starts from.
    pub document: Value,
    /// The transcript projected to every other agent, as the JSONL `path p
    /// export <agent>` writes. Copilot's is in canonical form
    /// ([`crate::canonical::canonicalize`]): its projection mints a session
    /// id per run.
    #[serde(serialize_with = "by_agent_name")]
    pub projections: BTreeMap<Agent, String>,
    /// Each projection read back by its own adapter and derived again: what
    /// of the document survived.
    #[serde(serialize_with = "by_agent_name")]
    pub round_trips: BTreeMap<Agent, RoundTrip>,
    /// What is shed between the transcript, the document and the
    /// projections, including the same-format round trip.
    pub shed: Shed,
    /// The defects pinned as expected current behaviour for this set.
    pub known_defects: Vec<KnownDefect>,
}

/// The stages in the order they run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Bytes -> the agent's own conversation type.
    Read,
    /// -> toolpath graph.
    Derive,
    /// -> every other agent's transcript.
    Project,
    /// -> the set's own format again.
    SameFormat,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Stage::Read => "read",
            Stage::Derive => "derive",
            Stage::Project => "project",
            Stage::SameFormat => "same_format",
        })
    }
}

/// The first stage that failed, and why. Stages before it succeeded; stages
/// after it did not run.
#[derive(Clone, Debug)]
pub struct StageError {
    pub stage: Stage,
    pub error: String,
}

impl StageError {
    pub fn new(stage: Stage, error: impl fmt::Display) -> StageError {
        StageError {
            stage,
            error: error.to_string(),
        }
    }
}

impl fmt::Display for StageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stage `{}` failed: {}", self.stage, self.error)
    }
}

impl std::error::Error for StageError {}

/// Size of a toolpath document: steps, conversation text, tool uses, and
/// distinct generalised key paths.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub steps: usize,
    pub text_chars: usize,
    pub tool_uses: usize,
    pub key_paths: usize,
}

/// A projection read back by its own adapter and derived again, compared to
/// the document it was projected from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RoundTrip {
    /// The adapter read its projection; this is what survived.
    Compared {
        before: Counts,
        after: Counts,
        /// Key paths of the document absent after the round trip.
        lost: Vec<KeyPath>,
        /// Key paths present only after: vestigial or invented.
        gained: Vec<KeyPath>,
    },
    /// The adapter could not read what was projected for it.
    Unreadable { error: String },
}

/// Lines, bytes and event types of a JSONL transcript.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TranscriptStats {
    pub lines: usize,
    pub bytes: usize,
    /// Characters in every string value, summed.
    pub string_chars: usize,
    /// Count per event type (`type`, or `type/payload.type`).
    pub by_type: BTreeMap<String, usize>,
}

/// Bytes and content of a toolpath document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DocumentStats {
    pub bytes: usize,
    pub string_chars: usize,
    pub steps: usize,
    pub text_chars: usize,
    pub tool_uses: usize,
}

/// How many lines of one event type the same-format round trip kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Kept {
    pub source: usize,
    pub after: usize,
}

/// A field the same-format round trip writes under another spelling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Rename {
    pub from: String,
    pub to: String,
}

/// A field of the transcript the same-format round trip does not write
/// back, with one example value (withheld when it looks like an identifier
/// or a secret).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Dropped {
    pub path: String,
    pub example: String,
}

/// Leaf key paths of a transcript before and after the same-format round
/// trip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct KeyPathTally {
    pub source: usize,
    pub after: usize,
    pub lost: usize,
    pub gained: usize,
}

/// The transcript projected back to its own format and compared field by
/// field with the original.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SameFormat {
    pub lines: usize,
    pub bytes: usize,
    /// Event types with fewer lines after than before.
    pub event_types_shed: BTreeMap<String, Kept>,
    pub key_paths: KeyPathTally,
    pub renamed: Vec<Rename>,
    pub dropped: Vec<Dropped>,
}

/// What is shed at each stage: transcript -> document -> projections, and
/// the same-format round trip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Shed {
    pub source: TranscriptStats,
    pub ir: DocumentStats,
    #[serde(serialize_with = "by_agent_name")]
    pub projections: BTreeMap<Agent, TranscriptStats>,
    pub same_format: SameFormat,
}

/// A defect pinned as expected current behaviour. A fix changes the
/// finding, so the snapshot that holds it fails; accepting the new snapshot
/// is the acknowledgement that the defect is gone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KnownDefect {
    pub name: &'static str,
    /// The projection the finding is measured on: an agent, or
    /// `self:<agent>` for the same-format round trip.
    pub target: String,
    pub finding: Finding,
}

/// What a known defect measures.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Finding {
    /// Top-level `type` values a Claude projection writes that Claude Code
    /// does not define, with how many lines carry each.
    IllegalClaudeTypes { counts: BTreeMap<String, usize> },
    /// The `cwd` a codex projection writes, against the session's own.
    CallerCwd {
        source_cwd: Option<String>,
        written_cwd: BTreeSet<String>,
    },
    /// Fields the same-format round trip writes under another spelling.
    Renames { renames: Vec<Rename> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_is_detected_from_the_first_json_line() {
        let codex = b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"x\"}}\n";
        let copilot = b"{\"type\":\"session.start\",\"data\":{\"sessionId\":\"x\"}}\n";
        let pi = b"{\"type\":\"session\",\"id\":\"x\",\"cwd\":\"/w\"}\n";
        let claude = b"\n{\"type\":\"user\",\"sessionId\":\"x\"}\n";
        assert_eq!(Agent::detect(codex), Some(Agent::Codex));
        assert_eq!(Agent::detect(copilot), Some(Agent::Copilot));
        assert_eq!(Agent::detect(pi), Some(Agent::Pi));
        assert_eq!(Agent::detect(claude), Some(Agent::Claude));
        assert_eq!(Agent::detect(b"{\"type\":\"session\"}\n"), None);
        assert_eq!(Agent::detect(b""), None);
    }

    #[test]
    fn targets_are_every_other_agent() {
        let t: Vec<Agent> = Agent::Pi.targets().collect();
        assert_eq!(t, vec![Agent::Claude, Agent::Codex, Agent::Copilot]);
    }

    #[test]
    fn a_set_is_named_after_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("my-set").join("input.jsonl");
        std::fs::create_dir_all(input.parent().unwrap()).unwrap();
        std::fs::write(&input, b"{\"type\":\"user\",\"sessionId\":\"s\"}\n").unwrap();
        let set = Set::from_input_file(&input).unwrap();
        assert_eq!(set.name, "my-set");
        assert_eq!(set.agent, Agent::Claude);
        assert_eq!(set.input_sha256().len(), 64);
    }

    #[test]
    fn an_unrecognised_transcript_is_refused_by_name() {
        let err = Set::new("odd", b"{\"hello\":1}\n".to_vec()).unwrap_err();
        assert!(err.starts_with("odd:"), "{err}");
    }
}
