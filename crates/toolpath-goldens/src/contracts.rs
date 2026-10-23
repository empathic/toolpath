//! Writer and document contracts: properties a projection or a document must
//! hold for its consumer to accept it, each one learned from a bug that
//! shipped (CHANGELOG.md, the resume matrix). Snapshots pin bytes; these pin
//! meaning, so a recapture that accepts a broken output still fails here.
//!
//! The goldens test asserts every contract over every projection and every
//! document; `tests/regressions.rs` re-introduces each historical bug as a
//! fault in a stage's output and requires the matching contract to name it.

use std::collections::BTreeSet;
use std::fmt;

use serde_json::Value;

use crate::model::Agent;

/// One broken contract: where (target, line), which field, which rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// The rule's name, stable for tests: `trailing-newline`,
    /// `claude-null-signature`, `claude-empty-error-result`,
    /// `parent-chain`, `opencode-session-id`, `unique-step-ids`,
    /// `codex-timestamp`, `codex-session-meta-first`, `pi-header`,
    /// `copilot-session-start-first`.
    pub rule: &'static str,
    /// The output it was found in: an agent name, or `document`.
    pub target: String,
    /// 1-based line for JSONL outputs, 0 otherwise.
    pub line: usize,
    /// The key path of the offending field.
    pub field: String,
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}{}): {} — {}",
            self.rule,
            self.target,
            if self.line > 0 {
                format!(" line {}", self.line)
            } else {
                String::new()
            },
            self.field,
            self.detail
        )
    }
}

fn violation(
    rule: &'static str,
    target: Agent,
    line: usize,
    field: impl Into<String>,
    detail: impl Into<String>,
) -> Violation {
    Violation {
        rule,
        target: target.name().to_owned(),
        line,
        field: field.into(),
        detail: detail.into(),
    }
}

/// The contracts a projection for `target` must hold. `text` is the
/// projection as `path p export <target> -o` writes it (the raw form, before
/// any canonicalisation for pinning).
pub fn check_projection(target: Agent, text: &str) -> Vec<Violation> {
    let mut out = Vec::new();
    let jsonl = matches!(
        target,
        Agent::Claude | Agent::Codex | Agent::Pi | Agent::Copilot
    );
    // The harness appends to the file on resume; without a final newline the
    // first appended entry lands on the projected last line (2026-07-30).
    if jsonl && !text.is_empty() && !text.ends_with('\n') {
        let line = text.lines().count();
        out.push(violation(
            "trailing-newline",
            target,
            line,
            "<end of file>",
            "a JSONL projection must end with a newline, or the harness's first append corrupts the last line",
        ));
    }
    match target {
        Agent::Claude => claude(text, &mut out),
        Agent::Codex => codex(text, &mut out),
        Agent::Pi => {
            pi_header(text, &mut out);
            parent_chain(target, text, "id", "parentId", true, &mut out)
        }
        Agent::Copilot => copilot_first(text, &mut out),
        Agent::Opencode => opencode(text, &mut out),
        _ => {}
    }
    out
}

/// codex's loader skips a rollout line with no `timestamp`, and reads the
/// session from a `session_meta` first line
/// (docs/agents/formats/validation.md, S2).
fn codex(text: &str, out: &mut Vec<Violation>) {
    for (n, v) in lines(text) {
        if v.get("timestamp").and_then(Value::as_str).is_none() {
            out.push(violation(
                "codex-timestamp",
                Agent::Codex,
                n,
                "timestamp",
                "codex skips a rollout line with no timestamp",
            ));
        }
        if n == 1 && v.get("type").and_then(Value::as_str) != Some("session_meta") {
            out.push(violation(
                "codex-session-meta-first",
                Agent::Codex,
                n,
                "type",
                "a rollout starts with its session_meta line",
            ));
        }
    }
}

/// pi rewrites a session whose header is not first or not version 3
/// (docs/agents/formats/validation.md).
fn pi_header(text: &str, out: &mut Vec<Violation>) {
    let Some((n, v)) = lines(text).next() else {
        return;
    };
    let ok = v.get("type").and_then(Value::as_str) == Some("session")
        && v.get("id").is_some_and(Value::is_string)
        && v.get("version").and_then(Value::as_u64) == Some(3);
    if !ok {
        out.push(violation(
            "pi-header",
            Agent::Pi,
            n,
            "type/id/version",
            "a pi session starts with a version-3 session header with a string id",
        ));
    }
}

/// Copilot fails the whole resume unless the first event is
/// `session.start` (docs/agents/formats/validation.md, S15).
fn copilot_first(text: &str, out: &mut Vec<Violation>) {
    if let Some((n, v)) = lines(text).next()
        && v.get("type").and_then(Value::as_str) != Some("session.start")
    {
        out.push(violation(
            "copilot-session-start-first",
            Agent::Copilot,
            n,
            "type",
            "Copilot's event stream starts with session.start",
        ));
    }
}

fn lines(text: &str) -> impl Iterator<Item = (usize, Value)> + '_ {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .filter_map(|(i, l)| serde_json::from_str(l).ok().map(|v| (i + 1, v)))
}

fn claude(text: &str, out: &mut Vec<Violation>) {
    for (n, line) in lines(text) {
        let Some(blocks) = line.pointer("/message/content").and_then(Value::as_array) else {
            continue;
        };
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                // The API rejects `"signature": null` (400 …thinking.signature.str);
                // an absent signature is the writer contract (2026-07-30).
                Some("thinking") if b.get("signature").is_some_and(Value::is_null) => {
                    out.push(violation(
                        "claude-null-signature",
                        Agent::Claude,
                        n,
                        "message.content[].signature",
                        "a thinking block's signature is null; write it absent instead",
                    ));
                }
                // The API rejects an errored tool_result with empty content
                // (400 …content cannot be empty if is_error is true; copilot → claude).
                Some("tool_result")
                    if b.get("is_error").and_then(Value::as_bool) == Some(true)
                        && empty_content(b.get("content")) =>
                {
                    out.push(violation(
                        "claude-empty-error-result",
                        Agent::Claude,
                        n,
                        "message.content[].content",
                        "an errored tool_result has empty content",
                    ));
                }
                _ => {}
            }
        }
    }
    parent_chain(Agent::Claude, text, "uuid", "parentUuid", false, out);
}

fn empty_content(c: Option<&Value>) -> bool {
    match c {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Array(a)) => a.iter().all(|p| {
            p.get("text")
                .and_then(Value::as_str)
                .is_none_or(|t| t.trim().is_empty())
        }),
        _ => false,
    }
}

/// Every parent names an entry written before it: the harness walks the
/// chain from the leaf, and a dangling or missing link cuts the history off
/// there (pi → claude, pi → pi). With `one_root`, only the first chained
/// entry may have no parent (pi's main thread).
fn parent_chain(
    target: Agent,
    text: &str,
    id_key: &str,
    parent_key: &str,
    one_root: bool,
    out: &mut Vec<Violation>,
) {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut roots = 0usize;
    for (n, line) in lines(text) {
        let Some(id) = line.get(id_key).and_then(Value::as_str) else {
            continue;
        };
        match line.get(parent_key) {
            Some(Value::String(p)) if !seen.contains(p) => out.push(violation(
                "parent-chain",
                target,
                n,
                parent_key,
                format!("{parent_key} {p} names no earlier entry"),
            )),
            Some(Value::String(_)) => {}
            Some(Value::Null) | None
                if line.as_object().is_some_and(|o| o.contains_key(parent_key)) =>
            {
                roots += 1;
                if one_root && roots > 1 {
                    out.push(violation(
                        "parent-chain",
                        target,
                        n,
                        parent_key,
                        "a second entry with no parent: the chain is cut here",
                    ));
                }
            }
            _ => {}
        }
        seen.insert(id.to_owned());
    }
}

/// opencode's Zen free tier admits only sessions whose id has opencode's own
/// shape: `ses_`, 12 hex digits, 14 base62 characters (403 FreeTierError
/// otherwise).
fn opencode(text: &str, out: &mut Vec<Violation>) {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if let Some(id) = v.get("id").and_then(Value::as_str) {
        let ok = id.len() == 4 + 12 + 14
            && id.starts_with("ses_")
            && id[4..16].bytes().all(|b| b.is_ascii_hexdigit())
            && id[16..].bytes().all(|b| b.is_ascii_alphanumeric());
        if !ok {
            out.push(violation(
                "opencode-session-id",
                Agent::Opencode,
                0,
                "id",
                format!("{id} is not ses_ + 12 hex + 14 base62"),
            ));
        }
    }
}

/// The contracts a derived document must hold beyond its schema: step ids
/// unique within each path (RFC "ID Uniqueness"; derive 2026-07-01).
pub fn check_document(document: &Value) -> Vec<Violation> {
    let mut out = Vec::new();
    let paths = document
        .get("paths")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (pi, path) in paths.iter().enumerate() {
        let mut seen = BTreeSet::new();
        for step in path
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(id) = step.pointer("/step/id").and_then(Value::as_str)
                && !seen.insert(id.to_owned())
            {
                out.push(Violation {
                    rule: "unique-step-ids",
                    target: "document".into(),
                    line: 0,
                    field: format!("paths[{pi}].steps[].step.id"),
                    detail: format!("step id {id} appears twice in one path"),
                });
            }
        }
    }
    out
}
