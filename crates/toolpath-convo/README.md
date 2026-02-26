# toolpath-convo

Provider-agnostic conversation types and traits for AI coding tools.

This crate defines a common vocabulary for representing conversations
from any AI coding assistant (Claude, Codex, OpenCode, etc.) without
coupling consumer code to provider-specific data formats.

## Overview

**Types** define the common data model:

| Type | What it represents |
|---|---|
| `Turn` | A single conversational turn (text, thinking, tool uses, model, tokens, environment, delegations) |
| `Role` | Who produced the turn: `User`, `Assistant`, `System`, `Other(String)` |
| `ConversationView` | A complete conversation: ordered turns, timestamps, aggregate usage, files changed |
| `ConversationMeta` | Lightweight metadata (no turns loaded) |
| `ToolInvocation` | A tool call within a turn, with optional `ToolCategory` classification |
| `ToolResult` | The result of a tool call |
| `ToolCategory` | Toolpath's classification ontology: `FileRead`, `FileWrite`, `FileSearch`, `Shell`, `Network`, `Delegation` |
| `TokenUsage` | Input/output/cache token counts |
| `EnvironmentSnapshot` | Working directory and VCS branch/revision at time of a turn |
| `DelegatedWork` | A sub-agent delegation: prompt, nested turns, result |
| `WatcherEvent` | A `Turn` (new), `TurnUpdated` (enriched with tool results), or `Progress` event |

**Traits** define how providers expose their data:

| Trait | What it does |
|---|---|
| `ConversationProvider` | List and load conversations from any source |
| `ConversationWatcher` | Poll for new conversational events |

## Usage

```rust
use toolpath_convo::{ConversationView, ConversationProvider, Role, ToolCategory};

// Provider crates implement ConversationProvider.
// Consumer code works against the trait:
fn show_conversation(provider: &dyn ConversationProvider) {
    let view = provider.load_conversation("/path/to/project", "session-id")
        .unwrap();

    if let Some(title) = view.title(80) {
        println!("# {}", title);
    }

    // Session-level summary
    if let Some(usage) = &view.total_usage {
        println!("Tokens: {:?} in / {:?} out", usage.input_tokens, usage.output_tokens);
    }
    println!("Files changed: {:?}", view.files_changed);

    for turn in &view.turns {
        println!("[{}] {}", turn.role, turn.text);

        // Environment context
        if let Some(env) = &turn.environment {
            println!("  cwd: {:?}, branch: {:?}", env.working_dir, env.vcs_branch);
        }

        // Tool classification
        for tool_use in &turn.tool_uses {
            println!("  {} ({:?})", tool_use.name, tool_use.category);
        }

        // Sub-agent delegations
        for d in &turn.delegations {
            println!("  delegated: {}", d.prompt);
            if let Some(result) = &d.result {
                println!("    -> {}", result);
            }
        }
    }
}
```

## Tool classification

`ToolCategory` is toolpath's own ontology for what a tool invocation does,
independent of provider-specific naming. Provider crates map their tool
names into these categories; `None` means the tool isn't recognized.

| Category | Meaning |
|---|---|
| `FileRead` | Read a file — no side effects |
| `FileWrite` | Write, edit, create, or delete a file |
| `FileSearch` | Search or discover files by name or content pattern |
| `Shell` | Shell or terminal command execution |
| `Network` | Web fetch, search, API call |
| `Delegation` | Spawn a sub-agent or delegate work |

Consumers can filter by category without knowing provider tool vocabularies:

```rust,ignore
let writes: Vec<_> = turn.tool_uses.iter()
    .filter(|t| t.category == Some(ToolCategory::FileWrite))
    .collect();
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
