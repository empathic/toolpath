//! The Claude Code mock against the committed goldens and the committed cassette.
//!
//! * Every projection into Claude Code in `toolpath-goldens/snapshots/` must resume whole: every
//!   conversation entry reached, and a history the Messages API accepts.
//! * The two historical bugs the mock models, re-introduced in a copy of the fixture (never in
//!   code), must be caught: pi → claude's broken parent chain, copilot → claude's empty errored
//!   tool_result (the 400).
//! * Contract: on the codex golden's projection, the mock's history is a prefix of the request
//!   Claude Code 2.1.285 really sent when it resumed that projection (webmock cassette).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use toolpath_harness_mock::{Block, Message, Role, check_api, claude};

fn crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// The JSONL body of an insta snapshot (everything after the second `---` line).
fn snapshot_body(name: &str) -> String {
    let text = fs::read_to_string(crates().join("toolpath-goldens/snapshots").join(name))
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut seps = 0;
    text.lines()
        .filter(|l| {
            if *l == "---" {
                seps += 1;
                return false;
            }
            seps >= 2
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn projections_into_claude() -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(crates().join("toolpath-goldens/snapshots"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("projection@") && n.ends_with("@claude.snap"))
        .collect();
    v.sort();
    v
}

fn edit_lines(jsonl: &str, f: impl Fn(&mut Value)) -> String {
    jsonl
        .lines()
        .map(|l| match serde_json::from_str::<Value>(l) {
            Ok(mut v) => {
                f(&mut v);
                v.to_string()
            }
            Err(_) => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_user_text(m: &[Message]) -> Option<&str> {
    m.iter()
        .filter(|m| m.role == Role::User)
        .flat_map(|m| &m.blocks)
        .find_map(|b| {
            if let Block::Text(t) = b {
                Some(t.as_str())
            } else {
                None
            }
        })
}

/// What the mock finds today, pinned like `known_defects`: a change in either direction fails,
/// so a fix has to remove its line here. None of these resumes has been run live.
enum Known {
    /// The loader never reaches this many conversation entries.
    Unreached(usize),
    /// The Messages API would refuse the history with this reason.
    Refused(&'static str),
}

const KNOWN: &[(&str, Known)] = &[
    // opencode records each assistant message's parent as the user message it answers, and the
    // projection writes that as `parentUuid`: every assistant hangs off the first user turn.
    // Claude Code's walk from the leaf then reaches only the first and the last turn: the
    // pi -> claude bug class. opencode was never a source in the resume matrix.
    (
        "projection@opencode@claude.snap",
        Known::Unreached(OPENCODE_UNREACHED),
    ),
    // The same opencode bug on the compaction forced in opencode (empathic/toolpath#108's
    // recording): not caused by the compaction, so not waiting on #108.
    (
        "projection@opencode-forced@claude.snap",
        Known::Unreached(OPENCODE_FORCED_UNREACHED),
    ),
    // The cursor golden has a Glob call with no result anywhere in the session; the projection
    // writes the tool_use and nothing answers it. Unless Claude Code repairs it on load (not
    // verified), the API refuses an unanswered tool call.
    (
        "projection@cursor@claude.snap",
        Known::Refused(
            "tool_use tool_488515c4-acb2-45f6-929d-3c19689d35d has no tool_result in the next message",
        ),
    ),
];
const OPENCODE_UNREACHED: usize = 20; // of 22 conversation entries
const OPENCODE_FORCED_UNREACHED: usize = 22;

#[test]
fn every_projection_into_claude_resumes_whole() {
    let names = projections_into_claude();
    assert!(
        names.len() >= 6,
        "expected the six projections into claude, found {names:?}"
    );
    for name in names {
        let r = claude::resume(&snapshot_body(&name));
        assert!(!r.messages.is_empty(), "{name}: nothing sent");
        let known = KNOWN.iter().find(|(n, _)| *n == name).map(|(_, k)| k);
        match known {
            Some(Known::Unreached(n)) => assert_eq!(
                r.unreachable.len(),
                *n,
                "{name}: unreached entries changed (fixed? remove its KNOWN entry): {:?}",
                r.unreachable
            ),
            _ => assert!(
                r.unreachable.is_empty(),
                "{name}: entries the loader never reaches: {:?}",
                r.unreachable
            ),
        }
        match (check_api(&r.messages), known) {
            (Err(e), Some(Known::Refused(why))) => {
                assert!(
                    e.reason.contains(why),
                    "{name}: refused for another reason now: {}",
                    e.reason
                )
            }
            (Ok(()), Some(Known::Refused(why))) => {
                panic!("{name}: resumes now; remove its KNOWN entry ({why})")
            }
            (Err(e), _) => panic!("{name}: the Messages API would refuse it: {}", e.reason),
            (Ok(()), _) => {}
        }
    }
}

#[test]
fn catches_the_pi_parent_chain_bug() {
    let good = snapshot_body("projection@pi@claude.snap");
    let ok = claude::resume(&good);
    assert!(ok.unreachable.is_empty());
    let first = first_user_text(&ok.messages)
        .expect("a first user message")
        .to_owned();

    // 2026-10-15: assistant lines after a tool call were written with no parent.
    let broken = edit_lines(&good, |v| {
        if v.get("type").and_then(Value::as_str) == Some("assistant") {
            v["parentUuid"] = Value::Null;
        }
    });
    let r = claude::resume(&broken);
    assert!(
        !r.unreachable.is_empty(),
        "the broken chain must leave history unreached"
    );
    assert_ne!(
        first_user_text(&r.messages),
        Some(first.as_str()),
        "the first user message must be lost"
    );
}

#[test]
fn catches_the_copilot_empty_errored_result() {
    let good = snapshot_body("projection@copilot@claude.snap");
    assert!(check_api(&claude::resume(&good).messages).is_ok());

    // 2026-10-15: a failed tool call's result was written errored and empty.
    let broken = edit_lines(&good, |v| {
        if let Some(blocks) = v
            .pointer_mut("/message/content")
            .and_then(Value::as_array_mut)
        {
            for b in blocks {
                if b.get("type").and_then(Value::as_str) == Some("tool_result")
                    && b.get("is_error").and_then(Value::as_bool) == Some(true)
                {
                    b["content"] = Value::String(String::new());
                }
            }
        }
    });
    let e = check_api(&claude::resume(&broken).messages).expect_err("the API must refuse it");
    assert!(e.reason.contains("cannot be empty"), "{}", e.reason);
}

/// One block in comparable form: role, kind, and its identity (text, tool id).
fn flat(m: &[Message]) -> Vec<(Role, String)> {
    m.iter()
        .flat_map(|m| {
            m.blocks.iter().map(move |b| {
                let k = match b {
                    // Trailing whitespace is not compared: Claude Code 2.1.285 sent one projected text with a
                    // trailing newline the projection does not have.
                    Block::Text(t) => format!("text:{}", t.trim_end()),
                    Block::Thinking(_) => "thinking".to_string(),
                    Block::ToolUse { id, name, .. } => format!("tool_use:{id}:{name}"),
                    Block::ToolResult {
                        tool_use_id,
                        is_error,
                        content,
                    } => {
                        format!(
                            "tool_result:{tool_use_id}:{is_error}:{}",
                            content.trim_end()
                        )
                    }
                };
                (m.role, k)
            })
        })
        .collect()
}

fn cassette_messages(file: &Path) -> Vec<Message> {
    let c: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    let body: Value = match &c["request"]["body"] {
        Value::String(s) => serde_json::from_str(s).unwrap(),
        v => v.clone(),
    };
    // A body is the jsonl of one assistant/user line per message, so reuse the loader's block
    // parsing by wrapping each message as a session line.
    let lines: Vec<String> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let t = if m["role"] == "assistant" { "assistant" } else { "user" };
            serde_json::json!({
                "type": t, "uuid": format!("m{i}"),
                "parentUuid": if i == 0 { Value::Null } else { Value::String(format!("m{}", i - 1)) },
                "message": {"id": format!("m{i}"), "role": m["role"], "content": m["content"]},
            })
            .to_string()
        })
        .collect();
    claude::resume(&lines.join("\n")).messages
}

#[test]
fn agrees_with_what_claude_code_sent() {
    let cassette = crates()
        .join("toolpath-webmock/cassettes/claude/9d135681ef63faa4b1e550dc5e17be258c2b25ab3da95f8fa8ae109846adbb42.json");
    let sent = flat(&cassette_messages(&cassette));
    let mock = flat(&claude::resume(&snapshot_body("projection@codex@claude.snap")).messages);
    assert!(mock.len() > 3, "the mock sent too little: {mock:?}");
    assert!(
        sent.len() >= mock.len(),
        "Claude Code sent fewer blocks ({}) than the mock ({})",
        sent.len(),
        mock.len()
    );
    for (i, (m, s)) in mock.iter().zip(&sent).enumerate() {
        // Claude Code appends its own <system-reminder> text inside the tool_result it sends
        // last, so a result block only has to start with what the mock sent.
        let same =
            m == s || (m.0 == s.0 && m.1.starts_with("tool_result:") && s.1.starts_with(&m.1));
        assert!(
            same,
            "block {i}: mock {m:?}\n  vs what Claude Code sent {s:?}"
        );
    }
}
