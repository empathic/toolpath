//! Harness knowledge: what a coding agent puts in its prompts and tools.
//! Reads only `Generation` fields, never telemetry attribute keys.

pub mod cwd;
pub mod mutations;

use crate::normalize::{content_text, is_system_like};
use crate::session::Session;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::BTreeSet;

/// The coding agent a session came from, as far as the telemetry shows.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceHarness {
    ClaudeCode,
    Codex,
    Opencode,
    Pi,
    Unknown,
}

impl SourceHarness {
    /// The string recorded as `meta.extra.otel.harness`.
    pub fn as_str(self) -> &'static str {
        match self {
            SourceHarness::ClaudeCode => "claude-code",
            SourceHarness::Codex => "codex",
            SourceHarness::Opencode => "opencode",
            SourceHarness::Pi => "pi",
            SourceHarness::Unknown => "unknown",
        }
    }

    /// Exact inverse of [`SourceHarness::as_str`].
    pub fn from_name(name: &str) -> Option<SourceHarness> {
        [
            Self::ClaudeCode,
            Self::Codex,
            Self::Opencode,
            Self::Pi,
            Self::Unknown,
        ]
        .into_iter()
        .find(|h| h.as_str() == name)
    }
}

/// What harness inference looks at.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessSignals<'a> {
    pub session_id: Option<&'a str>,
    pub request_session_id: Option<&'a str>,
    pub has_developer: bool,
    /// Text of the first generation's leading system-like message.
    pub system_text: Cow<'a, str>,
    pub tool_names: BTreeSet<&'a str>,
}

pub fn signals(session: &Session) -> HarnessSignals<'_> {
    let gens = &session.generations;
    let system_text = gens
        .first()
        .and_then(|g| g.messages.first())
        .filter(|m| is_system_like(&m.role))
        .map(|m| match &m.content {
            Value::String(s) => Cow::Borrowed(s.as_str()),
            other => Cow::Owned(content_text(other)),
        })
        .unwrap_or_default();
    HarnessSignals {
        session_id: session.session_id.as_deref(),
        request_session_id: gens.iter().find_map(|g| g.request_session_id.as_deref()),
        has_developer: gens
            .iter()
            .flat_map(|g| &g.messages)
            .any(|m| m.role == "developer"),
        system_text,
        tool_names: gens
            .iter()
            .flat_map(|g| &g.completion.tool_calls)
            .map(|c| c.function.name.as_str())
            .collect(),
    }
}

fn uuid_version(s: &str) -> Option<char> {
    let b = s.as_bytes();
    let shape = s.len() == 36
        && [8, 13, 18, 23].iter().all(|&i| b[i] == b'-')
        && s.chars()
            .enumerate()
            .all(|(i, c)| [8, 13, 18, 23].contains(&i) || c.is_ascii_hexdigit());
    shape.then(|| b[14] as char)
}

fn claude_code(s: &HarnessSignals) -> bool {
    s.session_id.and_then(uuid_version) == Some('4') && s.request_session_id.is_some()
}
fn codex(s: &HarnessSignals) -> bool {
    s.session_id.and_then(uuid_version) == Some('7') && s.has_developer
}
fn opencode(s: &HarnessSignals) -> bool {
    s.session_id.is_some_and(|id| id.starts_with("ses_"))
}
fn claude_code_signature(s: &HarnessSignals) -> bool {
    s.system_text.contains("You are Claude Code")
}
/// pi: its four core tools and no request session id. The session id is not
/// read (it varies by release); the earlier codex rule claims v7 + developer.
fn pi(s: &HarnessSignals) -> bool {
    const PI_TOOLS: [&str; 4] = ["read", "bash", "edit", "write"];
    s.request_session_id.is_none()
        && !s.tool_names.is_empty()
        && s.tool_names.iter().all(|n| PI_TOOLS.contains(n))
}

type Rule = fn(&HarnessSignals) -> bool;
const RULES: [(Rule, SourceHarness); 5] = [
    (claude_code, SourceHarness::ClaudeCode),
    (codex, SourceHarness::Codex),
    (opencode, SourceHarness::Opencode),
    (claude_code_signature, SourceHarness::ClaudeCode),
    (pi, SourceHarness::Pi),
];

/// The first matching rule.
pub fn infer_harness(s: &HarnessSignals) -> SourceHarness {
    RULES
        .iter()
        .find(|(rule, _)| rule(s))
        .map_or(SourceHarness::Unknown, |(_, h)| *h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::{Generation, Message};
    use serde_json::json;

    fn sig<'a>(
        sid: Option<&'a str>,
        req: Option<&'a str>,
        dev: bool,
        system: &'a str,
        tools: &[&'a str],
    ) -> HarnessSignals<'a> {
        HarnessSignals {
            session_id: sid,
            request_session_id: req,
            has_developer: dev,
            system_text: system.into(),
            tool_names: tools.iter().copied().collect(),
        }
    }

    #[test]
    fn harness_table() {
        let v4 = "177b923f-8cf6-42fc-9f30-9a7b86236265";
        let v7 = "01a0e870-1365-7a93-b8a1-14726a5a70da";
        for (s, want) in [
            (
                sig(Some(v4), Some(v4), false, "", &[]),
                SourceHarness::ClaudeCode,
            ),
            (sig(Some(v7), None, true, "", &[]), SourceHarness::Codex),
            (
                sig(Some("ses_abc"), None, false, "", &[]),
                SourceHarness::Opencode,
            ),
            (
                sig(
                    None,
                    None,
                    false,
                    "You are Claude Code, Anthropic's CLI",
                    &["Bash"],
                ),
                SourceHarness::ClaudeCode,
            ),
            (
                sig(None, None, false, "", &["read", "bash", "write"]),
                SourceHarness::Pi,
            ),
            (
                sig(
                    Some(v7),
                    None,
                    false,
                    "",
                    &["read", "bash", "edit", "write"],
                ),
                SourceHarness::Pi,
            ),
            (
                sig(Some(v7), None, true, "", &["read", "bash"]),
                SourceHarness::Codex,
            ),
            (
                sig(Some("ses_abc"), None, false, "", &["read", "bash"]),
                SourceHarness::Opencode,
            ),
            (
                sig(Some(v7), None, false, "", &["read", "webfetch"]),
                SourceHarness::Unknown,
            ),
            (
                sig(None, None, false, "", &["read", "webfetch"]),
                SourceHarness::Unknown,
            ),
            (sig(Some(v7), None, false, "", &[]), SourceHarness::Unknown),
            (sig(Some(v4), None, false, "", &[]), SourceHarness::Unknown),
        ] {
            assert_eq!(infer_harness(&s), want, "{s:?}");
        }
        assert_eq!(
            infer_harness(&HarnessSignals::default()),
            SourceHarness::Unknown
        );
    }

    #[test]
    fn system_signature_reads_parts_encoded_content() {
        let g = Generation {
            messages: vec![
                Message {
                    role: "system".into(),
                    content: json!([
                        {"type": "text", "text": "x-billing-header"},
                        {"type": "text", "text": "You are Claude Code, Anthropic's official CLI"}
                    ]),
                    ..Default::default()
                },
                Message {
                    role: "user".into(),
                    content: json!("hi"),
                    ..Default::default()
                },
            ]
            .into(),
            ..Default::default()
        };
        let s = Session::new("otel-cluster:0000000000000000".into(), None, vec![g]);
        let sig = signals(&s);
        assert!(sig.system_text.contains("You are Claude Code"));
        assert_eq!(infer_harness(&sig), SourceHarness::ClaudeCode);
    }
}
