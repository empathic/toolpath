//! One-off helper: capture a Cursor session as a `CursorSession`
//! fixture for the cross-harness matrix.
//!
//! Two modes:
//!
//! 1. **`--composer <uuid>`** — read the named composer from
//!    Cursor.app's global `state.vscdb` and dump it. Optional
//!    `--bubbles <N>` trim (default 8) keeps fixtures comparable
//!    in size; `--no-trim` writes the whole session.
//!
//! 2. **`--from-jsonl <path>`** — synthesize a [`CursorSession`]
//!    from a `cursor-agent` CLI transcript (the CLI writes its
//!    sessions to a content-addressed protobuf blob store, *not*
//!    `state.vscdb`, but it mirrors the same JSONL transcript
//!    Cursor.app emits at
//!    `~/.cursor/projects/<slug>/agent-transcripts/<chat>/<chat>.jsonl`).
//!    Parsed into a `ConversationView` and projected back through
//!    [`CursorProjector`].
//!
//! Both modes write to `--output <path>` (default
//! `test-fixtures/cursor/convo.json`).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;
use toolpath_convo::{
    ConversationProjector, ConversationView, EnvironmentSnapshot, ProducerInfo, Role, SessionBase,
    ToolInvocation, Turn,
};
use toolpath_cursor::project::CursorProjector;
use toolpath_cursor::provider::tool_category;
use toolpath_cursor::reader::CONTENT_PREFIX;
use toolpath_cursor::{CursorConvo, CursorSession};

const DEFAULT_BUBBLE_LIMIT: usize = 8;
const DEFAULT_OUTPUT: &str = "test-fixtures/cursor/convo.json";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut composer_override: Option<String> = None;
    let mut bubble_limit: Option<usize> = Some(DEFAULT_BUBBLE_LIMIT);
    let mut output: String = DEFAULT_OUTPUT.to_string();
    let mut from_jsonl: Option<String> = None;
    let mut iter = args.iter().skip(1);
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--composer" => composer_override = iter.next().cloned(),
            "--from-jsonl" => from_jsonl = iter.next().cloned(),
            "--bubbles" => {
                bubble_limit = iter.next().and_then(|s| s.parse().ok()).or(bubble_limit);
            }
            "--no-trim" => bubble_limit = None,
            "--output" => {
                if let Some(o) = iter.next() {
                    output = o.clone();
                }
            }
            _ => {}
        }
    }

    let session = if let Some(path) = from_jsonl {
        capture_from_jsonl(&path)
    } else {
        capture_from_db(composer_override, bubble_limit)
    };

    write_session(&session, &output);
}

// ── Mode 1: from Cursor.app's global state.vscdb ──────────────────────

fn capture_from_db(
    composer_override: Option<String>,
    bubble_limit: Option<usize>,
) -> CursorSession {
    let mgr = CursorConvo::new();
    let ids = mgr.io().list_composer_ids().expect("list composer ids");
    let chosen_id = composer_override.unwrap_or_else(|| {
        let mut chosen: Option<(String, usize)> = None;
        for id in &ids {
            let Ok(s) = mgr.read_session(id) else { continue };
            let n = s.bubbles.len();
            eprintln!("  {} → {} bubbles", id, n);
            if chosen.as_ref().is_none_or(|(_, prev)| n > *prev) {
                chosen = Some((id.clone(), n));
            }
        }
        chosen.expect("at least one composer with bubbles").0
    });
    eprintln!("chose composer: {chosen_id}");

    let mut session = mgr.read_session(&chosen_id).expect("read chosen");

    // Trim bubbles + their referenced blobs unless `--no-trim`.
    if let Some(limit) = bubble_limit
        && session.bubbles.len() > limit
    {
        let keep_ids: std::collections::HashSet<String> = session
            .bubbles
            .iter()
            .take(limit)
            .map(|b| b.bubble_id.clone())
            .collect();
        session.bubbles.retain(|b| keep_ids.contains(&b.bubble_id));
        session
            .data
            .full_conversation_headers_only
            .retain(|h| keep_ids.contains(&h.bubble_id));
    }

    let needed = referenced_blob_hashes(&session);
    let trimmed: HashMap<String, String> = session
        .content_blobs
        .into_iter()
        .filter(|(k, _)| needed.contains(k))
        .collect();
    session.content_blobs = trimmed;

    session.transcript_path = None;
    session
}

fn referenced_blob_hashes(session: &CursorSession) -> std::collections::HashSet<String> {
    let mut needed = std::collections::HashSet::new();
    for b in &session.bubbles {
        let Some(tf) = &b.tool_former_data else { continue };
        let Ok(Some(result)) = tf.parse_result() else { continue };
        for field in ["beforeContentId", "afterContentId"] {
            if let Some(raw) = result.get(field).and_then(|v| v.as_str())
                && let Some(hash) = raw.strip_prefix(CONTENT_PREFIX)
            {
                needed.insert(hash.to_string());
            }
        }
    }
    needed
}

// ── Mode 2: from a cursor-agent CLI JSONL transcript ──────────────────

fn capture_from_jsonl(path: &str) -> CursorSession {
    let content = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {path}: {e}"));
    let composer_id = composer_id_from_jsonl_path(path);
    let workspace = workspace_from_jsonl_path(path);
    let view = view_from_jsonl(&content, &composer_id, &workspace);
    eprintln!(
        "parsed JSONL: {} turns ({} tool uses)",
        view.turns.len(),
        view.turns.iter().map(|t| t.tool_uses.len()).sum::<usize>(),
    );
    CursorProjector::new()
        .with_composer_id(composer_id)
        .with_workspace_path(workspace.unwrap_or_else(|| PathBuf::from("/")))
        .project(&view)
        .expect("project view to CursorSession")
}

/// `…/agent-transcripts/<chat-id>/<chat-id>.jsonl` → `<chat-id>`.
fn composer_id_from_jsonl_path(path: &str) -> String {
    PathBuf::from(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cli-elicit".to_string())
}

/// `~/.cursor/projects/<slug>/agent-transcripts/…` → reconstruct
/// the absolute workspace path from `<slug>` (path components
/// separated by `-`, with a leading `/`).
fn workspace_from_jsonl_path(path: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = path.split('/').collect();
    let projects_idx = parts.iter().position(|s| *s == "projects")?;
    let slug = parts.get(projects_idx + 1)?;
    // Cursor's CLI prefixes the slug with `private-` when the path
    // resolves through `/private/var/…` on macOS; honour that.
    Some(PathBuf::from(format!("/{}", slug.replace('-', "/"))))
}

fn view_from_jsonl(
    content: &str,
    composer_id: &str,
    workspace: &Option<PathBuf>,
) -> ConversationView {
    let mut turns: Vec<Turn> = Vec::new();
    let mut prev_id: Option<String> = None;
    let mut line_no = 0;
    let mut tool_counter = 0u32;

    let env_for = || -> Option<EnvironmentSnapshot> {
        workspace.as_ref().map(|w| EnvironmentSnapshot {
            working_dir: Some(w.to_string_lossy().into_owned()),
            vcs_branch: None,
            vcs_revision: None,
        })
    };

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        line_no += 1;
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("  line {line_no}: parse error ({e}); skipping");
                continue;
            }
        };
        let role = match v.get("role").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            Some(other) => {
                eprintln!("  line {line_no}: unknown role {other:?}; skipping");
                continue;
            }
            None => continue,
        };

        let turn_id = format!("turn-{line_no:04}");
        let content_arr = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let text = content_arr
            .iter()
            .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n");

        let tool_uses: Vec<ToolInvocation> = content_arr
            .iter()
            .filter(|c| c.get("type").and_then(Value::as_str) == Some("tool_use"))
            .map(|c| {
                tool_counter += 1;
                let name = c
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let input = c.get("input").cloned().unwrap_or(Value::Null);
                // The JSONL transcript has no tool result (that's why
                // it's the lossy view); leave `result: None` and let
                // the projector emit a `running` status downstream.
                ToolInvocation {
                    id: format!("tool_{tool_counter:04}"),
                    // Pass the agent-side name through; the projector
                    // remaps to native via category.
                    name: name.clone(),
                    input,
                    result: None,
                    // We *can* set category here from the name —
                    // cursor's `tool_category` handles agent-side
                    // names (Shell, Write, Read, Glob, …) directly.
                    category: tool_category(0, &name),
                }
            })
            .collect();

        turns.push(Turn {
            id: turn_id.clone(),
            parent_id: prev_id.clone(),
            message_id: None,
            role,
            // Synthesize plausible monotonic timestamps; the
            // transcript carries no real ones.
            timestamp: format!(
                "2026-06-01T{:02}:{:02}:00Z",
                line_no / 60,
                line_no % 60
            ),
            text,
            thinking: None,
            tool_uses,
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: env_for(),
            delegations: Vec::new(),
            file_mutations: Vec::new(),
        });
        prev_id = Some(turn_id);
    }

    ConversationView {
        id: composer_id.to_string(),
        provider_id: Some("cursor".to_string()),
        producer: Some(ProducerInfo {
            name: "cursor".into(),
            version: Some("cursor-agent".into()),
        }),
        base: Some(SessionBase {
            working_dir: workspace
                .as_ref()
                .map(|w| w.to_string_lossy().into_owned()),
            vcs_branch: None,
            vcs_revision: None,
            vcs_remote: None,
        }),
        turns,
        ..Default::default()
    }
}

// ── Output ──────────────────────────────────────────────────────────

fn write_session(session: &CursorSession, output: &str) {
    let json = serde_json::to_string_pretty(session).expect("serialize");
    if let Some(parent) = std::path::Path::new(output).parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).expect("create output dir");
    }
    fs::write(output, json).expect("write");
    eprintln!(
        "wrote {output} ({} bubbles, {} blobs)",
        session.bubbles.len(),
        session.content_blobs.len(),
    );
}
