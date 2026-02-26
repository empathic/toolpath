# toolpath-claude

Derive Toolpath provenance documents from Claude conversation logs.

When Claude Code writes your code, the conversation — the reasoning, the
tool calls, the abandoned approaches — is the provenance. This crate
reads those conversations directly and maps them to Toolpath documents
so every AI-assisted change has a traceable origin.

## Overview

Reads Claude Code conversation data from `~/.claude/projects/` and provides:

- **Conversation reading**: Parse JSONL conversation files into typed structures
- **Query**: Filter and search conversation entries by role, tool use, text content
- **Derivation**: Map conversations to Toolpath Path documents
- **Watching**: Monitor conversation files for live updates (feature-gated)

## Derivation

Convert Claude conversations into Toolpath documents:

```rust,no_run
use toolpath_claude::{ClaudeConvo, derive::{DeriveConfig, derive_path}};

let manager = ClaudeConvo::new();
let convo = manager.read_conversation("/Users/alex/project", "session-uuid")?;

let config = DeriveConfig::default();
let path = derive_path(&convo, &config);
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Mapping

| Claude concept | Toolpath concept |
|---|---|
| Session (JSONL file) | Path |
| Project path | `path.base.uri` as `file:///...` |
| User message | Step with `actor: "human:user"` |
| Assistant message | Step with `actor: "agent:{model}"` |
| Tool use (Write/Edit) | `change` entry keyed by file path |
| Assistant text | `meta.intent` |
| Sidechain entries | Steps parented to branch point |

## Reading conversations

```rust,no_run
use toolpath_claude::ClaudeConvo;

let manager = ClaudeConvo::new();

// List projects
let projects = manager.list_projects()?;

// Read a conversation
let convo = manager.read_conversation("/Users/alex/project", "session-uuid")?;
println!("{} entries", convo.entries.len());

// Most recent conversation
let latest = manager.most_recent_conversation("/Users/alex/project")?;

// Search
let matches = manager.find_conversations_with_text("/Users/alex/project", "authentication")?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Querying

```rust,ignore
use toolpath_claude::{ConversationQuery, MessageRole};

let query = ConversationQuery::new(&convo);
let user_msgs = query.by_role(MessageRole::User);
let tool_uses = query.tool_uses_by_name("Write");
let errors = query.errors();
let matches = query.contains_text("authentication");
```

## Watching

With the `watcher` feature (enabled by default):

```rust,ignore
use toolpath_claude::{AsyncConversationWatcher, WatcherConfig};

let config = WatcherConfig::new("/Users/alex/project", "session-uuid");
let mut watcher = AsyncConversationWatcher::new(config)?;
let handle = watcher.start().await?;

// New entries arrive via the handle
while let Some(entries) = handle.recv().await {
    for entry in entries {
        println!("{}: {:?}", entry.uuid, entry.message);
    }
}
```

## Feature flags

| Feature | Default | Description |
|---|---|---|
| `watcher` | yes | Filesystem watching via `notify` + `tokio` |

## Provider-agnostic usage

This crate implements `toolpath_convo::ConversationProvider`, so consumers can
code against the provider-agnostic types instead of Claude-specific structures.

Tool results are automatically assembled across entry boundaries — Claude's JSONL
writes tool invocations and their results as separate entries, but
`load_conversation` pairs them by `tool_use_id` so `ToolInvocation.result` is
populated and tool-result-only user entries are absorbed (no phantom empty turns):

```rust,ignore
use toolpath_claude::ClaudeConvo;
use toolpath_convo::ConversationProvider;

let provider = ClaudeConvo::new();
let view = provider.load_conversation("/path/to/project", "session-id")?;

for turn in &view.turns {
    println!("[{}] {}: {}", turn.timestamp, turn.role, turn.text);
    for tool_use in &turn.tool_uses {
        if let Some(result) = &tool_use.result {
            println!("  {} -> {}", tool_use.name, if result.is_error { "error" } else { "ok" });
        }
    }
}
```

The `ConversationWatcher` trait impl emits `WatcherEvent::Turn` eagerly and
follows up with `WatcherEvent::TurnUpdated` when tool results arrive:

```rust,ignore
use toolpath_convo::{ConversationWatcher, WatcherEvent};

for event in watcher.poll()? {
    match event {
        WatcherEvent::Turn(turn) => ui.add_turn(*turn),
        WatcherEvent::TurnUpdated(turn) => ui.replace_turn(*turn),
        WatcherEvent::Progress { .. } => {}
    }
}
```

### Enrichment

Beyond basic conversation reading, the provider populates
[`toolpath-convo`](https://crates.io/crates/toolpath-convo) enrichment fields:

**Tool classification** — Claude Code tool names are mapped to `ToolCategory`:

| Claude Code tool | ToolCategory |
|---|---|
| `Read` | `FileRead` |
| `Glob`, `Grep` | `FileSearch` |
| `Write`, `Edit`, `NotebookEdit` | `FileWrite` |
| `Bash` | `Shell` |
| `WebFetch`, `WebSearch` | `Network` |
| `Task` | `Delegation` |

Unrecognized tools get `category: None` — consumers still have `name` and `input`.

**Environment context** — each turn's `EnvironmentSnapshot` is populated from the
log entry's `cwd` (working directory) and `git_branch` (VCS branch).

**Delegation tracking** — `Task` tool invocations are extracted as `DelegatedWork`
on the parent turn, with `agent_id`, `prompt`, and `result` populated. Sub-agent
turns are stored in separate session files and not yet cross-referenced (`turns`
is empty).

**Token usage** — per-turn `TokenUsage` includes `cache_read_tokens` and
`cache_write_tokens` from Claude's prompt caching. `ConversationView.total_usage`
aggregates across all turns.

**Session summary** — `ConversationView.provider_id` is `"claude-code"`.
`ConversationView.files_changed` lists all files mutated during the session
(deduplicated, first-touch order), derived from `FileWrite`-categorized tool inputs.

See [`toolpath-convo`](https://crates.io/crates/toolpath-convo) for the full trait and type definitions.

## Part of Toolpath

This crate is part of the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) -- core types and query API
- [`toolpath-convo`](https://crates.io/crates/toolpath-convo) -- provider-agnostic conversation abstraction
- [`toolpath-git`](https://crates.io/crates/toolpath-git) -- derive from git history
- [`toolpath-dot`](https://crates.io/crates/toolpath-dot) -- Graphviz DOT rendering
- [`toolpath-cli`](https://crates.io/crates/toolpath-cli) -- unified CLI (`cargo install toolpath-cli`)
- [RFC](https://github.com/empathic/toolpath/blob/main/RFC.md) -- full format specification
