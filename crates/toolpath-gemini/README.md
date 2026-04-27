# toolpath-gemini

Derive Toolpath provenance documents from Gemini CLI conversation logs.

When Gemini CLI writes your code, the conversation — the reasoning, the
tool calls, the sub-agent delegations — is the provenance. This crate
reads those conversations directly from `~/.gemini/tmp/...` and maps
them to Toolpath documents so every AI-assisted change has a traceable
origin.

## Overview

Reads Gemini CLI conversation data from `~/.gemini/tmp/<project>/chats/`
and provides:

- **Conversation reading**: Parse the JSON chat files into typed
  structures
- **Query**: Filter and search messages by role, tool use, text content
- **Derivation**: Map conversations to Toolpath Path documents
- **Watching**: Monitor chat files for live updates (feature-gated)

## Mapping

| Gemini concept | Toolpath concept |
|---|---|
| Session UUID dir | Conversation (main chat + sub-agent chats merged) |
| Project path | `path.base.uri` as `file:///...` |
| User message | Step with `actor: "human:user"` |
| Gemini message | Step with `actor: "agent:<model>"` |
| `toolCalls[]` with `write_file`/`replace` | `change` entry keyed by file path |
| `thoughts[]` | `Turn.thinking` (joined) |
| Sub-agent chat file (`kind: "subagent"`) | `DelegatedWork` with populated `turns` |

## Derivation

```rust,no_run
use toolpath_gemini::{GeminiConvo, derive::{DeriveConfig, derive_path}};

let manager = GeminiConvo::new();
let convo = manager.read_conversation(
    "/Users/alex/project",
    "session-uuid",
)?;

let config = DeriveConfig::default();
let path = derive_path(&convo, &config);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Reading conversations

```rust,no_run
use toolpath_gemini::GeminiConvo;

let manager = GeminiConvo::new();

// List projects
let projects = manager.list_projects()?;

// List sessions for a project
let sessions = manager.list_conversations("/Users/alex/project")?;

// Read a full session (main chat + all sub-agent chats)
let convo = manager.read_conversation(
    "/Users/alex/project",
    "session-uuid",
)?;

// Most recent conversation
let latest = manager.most_recent_conversation("/Users/alex/project")?;

// Lightweight metadata, including the first user-prompt text as a
// human-readable title for picker UIs.
for meta in manager.list_conversation_metadata("/Users/alex/project")? {
    println!(
        "{} ({}msgs): {}",
        meta.session_uuid,
        meta.message_count,
        meta.first_user_message.as_deref().unwrap_or("(no prompt)"),
    );
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Provider-agnostic usage

This crate implements `toolpath_convo::ConversationProvider`, so
consumers can code against the provider-agnostic types instead of
Gemini-specific structures.

```rust,ignore
use toolpath_gemini::GeminiConvo;
use toolpath_convo::ConversationProvider;

let provider = GeminiConvo::new();
let view = provider.load_conversation("/path/to/project", "session-uuid")?;

for turn in &view.turns {
    println!("[{}] {}: {}", turn.timestamp, turn.role, turn.text);
    for tool_use in &turn.tool_uses {
        if let Some(result) = &tool_use.result {
            println!("  {} -> {}", tool_use.name, if result.is_error { "error" } else { "ok" });
        }
    }
}
```

### Tool classification

Gemini CLI tool names are mapped to `ToolCategory`:

| Gemini CLI tool | ToolCategory |
|---|---|
| `read_file`, `read_many_files`, `list_directory`, `get_internal_docs`, `read_mcp_resource` | `FileRead` |
| `glob`, `grep_search`, `search_file_content` | `FileSearch` |
| `write_file`, `replace`, `edit` | `FileWrite` |
| `run_shell_command` | `Shell` |
| `web_fetch`, `google_web_search` | `Network` |
| `task`, `activate_skill` | `Delegation` |

Unrecognized tools get `category: None` — consumers still have `name`
and `input`.

### Sub-agent delegations

Sub-agent invocations are stored as sibling chat files (`kind: "subagent"`)
in the same session UUID directory. When you load a conversation, those
sub-agent chats are folded into `DelegatedWork` on the parent `task` tool
invocation with `turns` populated (unlike `toolpath-claude`, which leaves
sub-agent turns empty because they live in separate session files).

### Environment context

Each turn's `EnvironmentSnapshot.working_dir` is populated from the chat
file's top-level `directories[0]`.

### Token usage

Per-turn `TokenUsage` includes:
- `input_tokens` ← `tokens.input`
- `output_tokens` ← `tokens.output`
- `cache_read_tokens` ← `tokens.cached`
- `cache_write_tokens` → `None` (Gemini doesn't expose this)

`ConversationView.total_usage` aggregates across all turns.

### Provider-specific metadata

Gemini log entries often carry extra fields (`thoughts`, `tokens.tool`,
`tokens.total`, `kind`, `summary`) that don't map to the common schema.
These are forwarded into `Turn.extra["gemini"]` so trait-only consumers
can access them without importing Gemini-specific types.

## Round-trip fidelity

The crate exposes three progressively lossy views of a conversation:

| Layer | Lossless? | Use it when |
|---|---|---|
| `ChatFile` / `Conversation` (the raw on-disk schema) | **Yes** — verified by `tests/roundtrip.rs` on live fixtures | You need to re-emit the Gemini JSON byte-equivalent (archival, editing, redaction) |
| `ConversationView` (provider-agnostic projection) | No — Gemini-specific fields live under `Turn.extra["gemini"]` | You want to work across providers with one set of types |
| `toolpath::v1::Path` (provenance digest) | No — tool results/args are summarized; only file-write bodies are preserved as full diffs | You want a compact Toolpath document for blame, queries, rendering |

**For a true round-trip** — Gemini → Toolpath → Gemini — stay at the
`ChatFile` level:

```rust,ignore
use toolpath_gemini::{ChatFile, GeminiConvo};

let raw = std::fs::read_to_string("/path/to/chats/session-X.json")?;
let chat: ChatFile = serde_json::from_str(&raw)?;
// ... inspect or modify chat ...
let back = serde_json::to_string(&chat)?; // byte-equivalent to `raw` (modulo key order)
```

Guarantees baked in:

- Every unknown field — top-level or per-message — rides through via
  `#[serde(flatten)] extra: HashMap<String, Value>`. Future schema
  additions survive unchanged.
- `GeminiRole` preserves unknown role values (`"plan"`, `"system"`,
  etc.) via `Other(String)`; known values (`user`/`gemini`/`info`)
  deserialize into typed variants.
- `ToolCall.result_display` is `Option<Value>`, so Gemini's
  structured payloads (dict-with-`fileDiff`, nested ANSI-styled
  arrays) round-trip opaquely.
- Optional list fields (`directories`, `thoughts`, `toolCalls`) use
  `Option<Vec<T>>` so we distinguish *absent* from *present-but-empty*.

## Feature flags

| Feature | Default | Description |
|---|---|---|
| `watcher` | yes | Filesystem watching via `notify` + `tokio` |

## Part of Toolpath

This crate is part of the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) -- core types and query API
- [`toolpath-convo`](https://crates.io/crates/toolpath-convo) -- provider-agnostic conversation abstraction
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) -- Claude conversation provider
- [`toolpath-git`](https://crates.io/crates/toolpath-git) -- derive from git history
- [`toolpath-dot`](https://crates.io/crates/toolpath-dot) -- Graphviz DOT rendering
- [`path-cli`](https://crates.io/crates/path-cli) -- unified CLI (`cargo install path-cli`)
- [RFC](https://github.com/empathic/toolpath/blob/main/RFC.md) -- full format specification
