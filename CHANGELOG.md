# Changelog

All notable changes to the Toolpath workspace are documented here.

## 0.2.0 — toolpath-convo / 0.3.0 — toolpath-claude

### toolpath-convo 0.2.0

- Added `WatcherEvent::TurnUpdated` variant for signaling when a previously-emitted turn has been updated with additional data (e.g. tool results that arrived in a later log entry)

### toolpath-claude 0.3.0

- **Breaking (behavioral):** `conversation_to_view()` and `ConversationProvider::load_conversation()` now perform cross-entry tool result assembly — tool-result-only user entries are absorbed into the preceding assistant turn's `ToolInvocation.result` fields instead of being emitted as separate phantom empty turns
- **Breaking (behavioral):** `ConversationWatcher` trait impl now emits `WatcherEvent::TurnUpdated` when tool results arrive, instead of emitting phantom empty user turns
- Added `Message::tool_results()` convenience method and `ToolResultRef` type, symmetric with `tool_uses()`/`ToolUseRef`
- Added shared `merge_tool_results()` that pairs results to invocations by `tool_use_id`
- Thanks to the crabcity maintainers for the detailed design request

## 0.2.1 — toolpath-claude

### toolpath-claude 0.2.1

- Fixed silent data loss when deserializing Claude Code JSONL conversation logs: `stop_reason`, `stop_sequence`, and all `Usage`/`CacheCreation` fields were always `None` because the structs expected camelCase keys but Claude Code writes the inner `message` object in the Anthropic API's native snake_case
- Added `#[serde(alias = "...")]` for snake_case variants on `Message`, `Usage`, and `CacheCreation` fields — both camelCase and snake_case are now accepted during deserialization
- Thanks to the crabcity maintainers for the detailed bug report

## 0.1.0 — toolpath-convo / 0.2.0 — toolpath-claude

### toolpath-convo 0.1.0

- New crate: provider-agnostic conversation types and traits for AI coding tools
- Types: `Turn`, `Role`, `ConversationView`, `ConversationMeta`, `TokenUsage`, `ToolInvocation`, `ToolResult`, `WatcherEvent`
- Traits: `ConversationProvider` (list/load conversations), `ConversationWatcher` (poll for updates)
- Enables consumer apps to code against a common conversation model instead of provider-specific types

### toolpath-claude 0.2.0

- Added convenience methods on `Message`: `text()`, `thinking()`, `tool_uses()`, `is_user()`, `is_assistant()`, `is_role()`
- Added convenience methods on `ConversationEntry`: `text()`, `role()`, `thinking()`, `tool_uses()`, `stop_reason()`, `model()`
- Added convenience methods on `Conversation`: `title(max_len)`, `first_user_text()`
- Implemented `toolpath_convo::ConversationProvider` for `ClaudeConvo`
- Implemented `toolpath_convo::ConversationWatcher` for sync `ConversationWatcher`
- Added `provider::to_view()` and `provider::to_turn()` for direct conversion
- New dependency: `toolpath-convo`

## 0.1.4 — toolpath / 0.2.0 — toolpath-cli

### toolpath 0.1.4

- Added `extra: HashMap<String, serde_json::Value>` with `#[serde(flatten)]` to `PathMeta`, `StepMeta`, and `GraphMeta`, matching the schema's `additionalProperties: true` and enabling round-trip fidelity for extension fields

### toolpath-cli 0.2.0

- **Breaking:** `path track` session files are now valid `{"Path": {...}}` Toolpath documents at all times. Tracking bookkeeping (buffer cache, sequence mappings) lives in `meta.track` and is stripped on export/close. Any Toolpath tool can read a live session file — `path validate`, `path query dead-ends`, `path render dot` all work mid-session without export.

## 0.1.3 — toolpath / 0.1.2 — all other crates

### All crates

- Improved README documentation: added motivating "why" context, cross-links between crates
- READMEs render as docs.rs landing pages with compilable examples

## 0.1.2 — toolpath / 0.1.1 — all other crates

### toolpath 0.1.2

- Added `repository`, `keywords`, and `categories` to crate metadata
- README now renders as the docs.rs landing page via `include_str!`
- All code examples in the README are compiled as doc tests

### toolpath-git 0.1.1

- Added `repository`, `keywords`, and `categories` to crate metadata
- Added `list_branches` to the API table in the README
- Added doc examples to `normalize_git_url` and `slugify_author`
- Module-level doc example for `derive`
- README now renders as the docs.rs landing page

### toolpath-claude 0.1.1

- Added `repository`, `keywords`, and `categories` to crate metadata
- README now renders as the docs.rs landing page

### toolpath-dot 0.1.1

- Added `repository`, `keywords`, and `categories` to crate metadata
- Added module-level documentation with usage example
- Added field docs to `RenderOptions`
- README now renders as the docs.rs landing page

### toolpath-cli 0.1.1

- Renamed crate from `path` to `toolpath-cli` (binary still called `path`)
- Moved crate directory from `crates/path/` to `crates/toolpath-cli/`
- Added `repository`, `keywords`, and `categories` to crate metadata
- Added `haiku` subcommand
- Installable via `cargo install toolpath-cli`

## 0.1.1 — toolpath

- Initial metadata-only release (added `repository`, `keywords`, `categories`)

## 0.1.0 — all crates

- Initial public release
- Core types: `Document`, `Graph`, `Path`, `Step` with builder API
- Query operations: `ancestors`, `dead_ends`, `filter_by_actor`, `filter_by_artifact`, `filter_by_time_range`
- Git derivation via `git2` (single branch -> Path, multiple branches -> Graph)
- Claude conversation derivation with filesystem watching
- Graphviz DOT rendering with actor color-coding and dead-end highlighting
- CLI with `derive`, `query`, `render`, `validate`, `list`, `merge`, `track` commands
- JSON Schema (`schema/toolpath.schema.json`)
- 11 example documents
- Full format specification (RFC.md)
