# toolpath-convo

Provider-agnostic conversation types and traits for AI coding tools.

This crate defines a common vocabulary for representing conversations
from any AI coding assistant (Claude, Codex, OpenCode, etc.) without
coupling consumer code to provider-specific data formats.

## Overview

**Types** define the common data model:

| Type | What it represents |
|---|---|
| `Turn` | A single conversational turn (text, thinking, tool uses, model, tokens) |
| `Role` | Who produced the turn: `User`, `Assistant`, `System`, `Other(String)` |
| `ConversationView` | A complete conversation: ordered turns + timestamps |
| `ConversationMeta` | Lightweight metadata (no turns loaded) |
| `ToolInvocation` | A tool call within a turn |
| `ToolResult` | The result of a tool call |
| `TokenUsage` | Input/output token counts |
| `WatcherEvent` | Either a `Turn` or a `Progress` event |

**Traits** define how providers expose their data:

| Trait | What it does |
|---|---|
| `ConversationProvider` | List and load conversations from any source |
| `ConversationWatcher` | Poll for new conversational events |

## Usage

```rust
use toolpath_convo::{ConversationView, ConversationProvider, Role};

// Provider crates implement ConversationProvider.
// Consumer code works against the trait:
fn show_conversation(provider: &dyn ConversationProvider) {
    let view = provider.load_conversation("/path/to/project", "session-id")
        .unwrap();

    if let Some(title) = view.title(80) {
        println!("# {}", title);
    }

    for turn in &view.turns {
        println!("[{}] {}", turn.role, turn.text);
    }
}
```

## Provider implementations

| Provider | Crate |
|---|---|
| Claude Code | [`toolpath-claude`](https://crates.io/crates/toolpath-claude) |

## Part of Toolpath

This crate is part of the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) -- core provenance types and query API
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) -- Claude conversation provider
- [`toolpath-git`](https://crates.io/crates/toolpath-git) -- derive from git history
- [`toolpath-dot`](https://crates.io/crates/toolpath-dot) -- Graphviz DOT rendering
- [`toolpath-cli`](https://crates.io/crates/toolpath-cli) -- unified CLI (`cargo install toolpath-cli`)
- [RFC](https://github.com/empathic/toolpath/blob/main/RFC.md) -- full format specification
