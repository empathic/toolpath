# Codex CLI session format

Reference for the on-disk format produced by OpenAI's
[Codex CLI](https://github.com/openai/codex) — the input the
[`toolpath-codex`](../../../crates/toolpath-codex) provider reads.
Compiled from:

1. Direct inspection of real session files on this machine
   (Codex CLI 0.118.0, session recorded 2026-04-20).
2. The Rust definitions in `openai/codex` at
   `codex-rs/protocol/src/protocol.rs` and
   `codex-rs/protocol/src/models.rs`.
3. The schema migrations and rollout recorder code under
   `codex-rs/state/` and `codex-rs/rollout/`.

Unlike Gemini, Codex has a **published** Rust protocol crate. Most of
what follows has authoritative type definitions upstream.

Date: **2026-04-20**. Revisit when the CLI minor version bumps or a
`state_<N+1>.sqlite` migration lands.

## Storage root

```
~/.codex/
  auth.json                       OAuth + API-key credentials (sensitive)
  config.toml                     User config + per-project trust
  version.json                    Self-update check bookkeeping
  models_cache.json               Cached model catalogue from OpenAI
  history.jsonl                   Lightweight user-prompt log
  .personality_migration          Marker file for personality migration
  .tmp/
    app-server-remote-plugin-sync-v1
    plugins/                      Plugin sync staging
    plugins.sha
  cache/
    codex_apps_tools/             Tool definitions cache
  log/
    codex-tui.log                 tracing-format TUI process log (rotating)
  memories/                       Persistent memory store (write-only from sandbox)
  shell_snapshots/
    <thread-uuid>.<ns>.sh         Snapshot of user shell env at session start
  skills/
    .system/                      Bundled skill packs
  tmp/
    arg0/                         Short-lived per-invocation state
  sessions/
    YYYY/MM/DD/
      rollout-<ts>-<uuid>.jsonl   ← the conversation log
  logs_1.sqlite                   Logs DB (ring buffer)
  state_5.sqlite                  Primary state DB (threads, jobs, etc.)
  state_<N>.sqlite-shm / -wal     SQLite WAL sidecars
```

The numeric suffixes (`state_5`, `logs_1`) are **schema generation
tags** — when the schema is incompatibly restructured, the version
increments and a new file is written alongside. See
[Database versioning](#database-versioning) below.

## `config.toml`

Minimal real example from this machine:

```toml
[projects."/Users/ben/empathic/oss/clash"]
trust_level = "trusted"

[projects."/Users/ben/empathic/oss/toolpath"]
trust_level = "trusted"
```

The schema is much richer than that — the full surface is documented in
[`codex-rs/core/config.schema.json`](https://raw.githubusercontent.com/openai/codex/main/codex-rs/core/config.schema.json).
Top-level keys observed in the schema include:

| Section | Purpose |
|---|---|
| `model` / `model_provider` / `model_providers` | Model + provider overrides |
| `model_reasoning_effort` (`none|minimal|low|medium|high|xhigh`) | Reasoning budget |
| `model_verbosity` (`low|medium|high`) | Output verbosity |
| `approval_policy` (`untrusted|on-failure|on-request|never|custom`) | When to ask the user |
| `sandbox_mode` (`read-only|workspace-write|danger-full-access`) | Filesystem isolation |
| `[projects."<abs-path>"].trust_level` | Per-project trust |
| `[permissions.<name>]` | Named permission profiles |
| `[mcp_servers.<name>]` | External MCP server definitions |
| `[tools]` | Web-search / image-view configuration |
| `tool_suggest` | Tool-discoverability policy |
| `shell_environment_policy` | What env vars propagate into sandboxed shells |
| `sqlite_home` / `CODEX_SQLITE_HOME` | SQLite DB location override |
| `history` | Whether to persist `history.jsonl` |
| `analytics` / `feedback` | Opt-in telemetry |
| `features` | Boolean feature gates |
| `project_root_markers` (default `[".git"]`) | How "project root" is detected |

## `history.jsonl`

Lightweight per-machine prompt history (not per-project). One JSON
object per line, appended:

```json
{
  "session_id": "019c95c9-00ce-7aa3-b767-0ff0551a85d5",
  "ts": 1772039511,
  "text": "ok the last agent took this task on a real tangent…"
}
```

| Field | Type | Notes |
|---|---|---|
| `session_id` | UUIDv7 string | Matches a rollout file under `sessions/` |
| `ts` | integer | Unix seconds |
| `text` | string | The user's prompt verbatim |

Redundant with the full rollout file — safe to ignore for conversation
reconstruction, useful only for fast cross-session prompt search.

## `sessions/` — rollout files

One JSONL file per session (one Codex launch, or one `codex resume`).

### Filename convention

```
sessions/YYYY/MM/DD/rollout-YYYY-MM-DDThh-mm-ss-<session-uuid>.jsonl
```

- Year/month/day directories are nested for filesystem efficiency.
- The timestamp in the filename is UTC, seconds precision, `T`-separated,
  `-` instead of `:` for filesystem safety.
- `<session-uuid>` is a UUIDv7 — it sorts lexicographically by creation
  time, and the first 8 chars match the timestamp prefix.

Codex rewrites the file continuously (streaming append from a
background writer task with a bounded 256-message channel — see
[`codex-rs/rollout/src/recorder.rs`](https://github.com/openai/codex/blob/main/codex-rs/rollout/src/recorder.rs)).

### Line shape

Every line is a JSON object with exactly three keys:

```json
{
  "timestamp": "2026-04-20T16:44:37.772Z",
  "type": "<top-level-variant>",
  "payload": { "type": "<payload-variant>", ... }
}
```

`type` is the externally-tagged `RolloutItem` enum. `payload` is the
variant's content. For item types that are themselves enums
(`ResponseItem`, `EventMsg`), the nested `payload.type` field
discriminates.

### Top-level `type` values (`RolloutItem`)

From [`codex-rs/protocol/src/protocol.rs`](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/protocol.rs):

```rust
pub enum RolloutItem {
    SessionMeta(SessionMetaLine),
    SessionState(SessionStateUpdate),
    ResponseItem(ResponseItem),
    Compacted(CompactedItem),
    TurnContext(TurnContextItem),
    EventMsg(EventMsg),
}
```

| Type | Meaning | Frequency in my fixture (138 lines) |
|---|---|---|
| `session_meta` | First line of every file; session-level metadata | 1 |
| `turn_context` | Per-turn context snapshot (new turn = new line) | 1 |
| `response_item` | Anything from the model (messages, reasoning, tool calls) | 82 |
| `event_msg` | CLI-side events (task lifecycle, exec results, patches, tokens) | 54 |
| `session_state` | Mid-session state updates (e.g. model switch) | 0 |
| `compacted` | Inserted when Codex compacts history mid-session | 0 |

### `compacted` — context compaction

When Codex compacts mid-session it appends a single `compacted` line to
the **same rollout file** — no new file, no new session id:

```json
{"type":"compacted","payload":{"message":"…summary text…","replacement_history":[…],"window_id":1}}
```

Per current Codex `main` (`codex-rs/protocol/src/protocol.rs`,
`CompactedItem`), `payload` is `{message, replacement_history?,
window_id?}`: `message` is the summary text, `replacement_history` is
the new condensed history that replaces the old, `window_id` is the
auto-compact window counter. **There is no `trigger`, `preTokens`, or
`summary` field** — manual `/compact` and automatic (overflow)
compaction write an **identical** record; the manual/auto distinction
(`CompactionTrigger`) is analytics-only and never persisted to the
rollout. (A separate field-less `event_msg` `ContextCompacted` is also
written — "either automatically or manually".)

The turns on either side keep their original ids — Codex does **not**
replay or re-id messages across the boundary, so there's no
duplicate-id hazard. `toolpath-codex` treats the payload as opaque and
currently drops it (see `tests/compaction_roundtrip.rs`); the
surrounding turns survive intact.

> Note: the repo fixture `tests/fixtures/compacted_session.jsonl` uses
> an older/synthetic `{trigger, preTokens, summary}` payload that does
> **not** match current Codex. Because the payload is parsed opaquely
> this doesn't affect derivation, but the fixture isn't representative.

## `session_meta` — first line of every file

```json
{
  "timestamp": "2026-04-20T16:44:37.772Z",
  "type": "session_meta",
  "payload": {
    "id": "019dabc6-8fef-7681-a054-b5bb75fcb97d",
    "timestamp": "2026-04-20T16:43:30.171Z",
    "cwd": "/Users/ben/empathic/oss/toolpath",
    "originator": "codex-tui",
    "cli_version": "0.118.0",
    "source": "cli",
    "model_provider": "openai",
    "base_instructions": { "text": "You are Codex…" },
    "git": {
      "commit_hash": "298fa07b0a13ed7f515a6fbc9c2fe7bc5af436a1",
      "branch": "main",
      "repository_url": "git@github.com:empathic/toolpath.git"
    }
  }
}
```

Rust type:

```rust
pub struct SessionMeta {
    pub id: ThreadId,
    pub forked_from_id: Option<ThreadId>,
    pub timestamp: String,
    pub cwd: PathBuf,
    pub originator: String,              // "codex-tui", "codex-exec", etc.
    pub cli_version: String,
    pub source: SessionSource,           // "cli" | "vscode" | ...
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
    pub agent_path: Option<String>,
    pub model_provider: Option<String>,
    pub base_instructions: Option<BaseInstructions>,
    pub dynamic_tools: Option<Vec<DynamicToolSpec>>,
    pub memory_mode: Option<String>,
}
```

`forked_from_id` → populated when a session was spawned from another
(multi-agent). `git` is denormalized alongside the struct and included
inline in the JSON.

## `turn_context` — per-turn snapshot

Emitted at the start of each turn. A single file usually has one
`turn_context` per turn:

```json
{
  "type": "turn_context",
  "payload": {
    "turn_id": "019dabc7-97da-7510-908e-81597e638179",
    "cwd": "/Users/ben/empathic/oss/toolpath",
    "current_date": "2026-04-20",
    "timezone": "America/New_York",
    "approval_policy": "on-request",
    "sandbox_policy": {
      "type": "workspace-write",
      "writable_roots": ["/Users/ben/.codex/memories"],
      "network_access": false,
      "exclude_tmpdir_env_var": false,
      "exclude_slash_tmp": false
    },
    "model": "gpt-5.4",
    "personality": "pragmatic",
    "collaboration_mode": { "mode": "default", "settings": { ... } },
    "realtime_active": false,
    "summary": "none",
    "truncation_policy": { "mode": "tokens", "limit": 10000 }
  }
}
```

The `sandbox_policy` field is the concrete resolution of
`config.toml`'s `sandbox_mode`: what roots the agent can write,
whether network is allowed, etc.

## `response_item` — model output

The externally-tagged `ResponseItem` enum captures every kind of thing
the model can produce in a turn. Inner `type` values observed:

| Inner type | Meaning |
|---|---|
| `message` | A textual message (role `developer`, `user`, or `assistant`) |
| `reasoning` | Chain-of-thought. Body is usually encrypted; `summary`/`content` are array fields |
| `function_call` | Call to a JSON-argument tool (`exec_command`, `write_stdin`, `update_plan`, etc.) |
| `function_call_output` | Paired output of a prior `function_call` |
| `custom_tool_call` | Call to a free-form-argument tool (e.g. `apply_patch`) |
| `custom_tool_call_output` | Paired output of a `custom_tool_call` |

Upstream's complete enum (with more variants Codex may emit in other
configurations):

```rust
pub enum ResponseItem {
    Message { id, role, content: Vec<ContentItem>, end_turn, phase },
    Reasoning { id, summary, content, encrypted_content },
    LocalShellCall { id, call_id, status, action },
    FunctionCall { id, name, namespace, arguments, call_id },
    ToolSearchCall { /* … */ },
    FunctionCallOutput { /* … */ },
    CustomToolCall { /* … */ },
    CustomToolCallOutput { /* … */ },
    ToolSearchOutput { /* … */ },
    WebSearchCall { /* … */ },
    ImageGenerationCall { /* … */ },
    GhostSnapshot { /* … */ },
    Compaction { /* … */ },
    Other,
}
```

### `message` variant

```json
{
  "type": "response_item",
  "payload": {
    "type": "message",
    "role": "assistant",
    "content": [
      { "type": "output_text", "text": "I'll inspect that folder…" }
    ],
    "phase": "commentary"
  }
}
```

| Role | Content item types | Notes |
|---|---|---|
| `developer` | `input_text` | System-like instructions (permissions, collab-mode, skills). Usually at position 0. |
| `user` | `input_text` | Human input |
| `assistant` | `output_text` | Model output |

`phase` is annotated on some assistant messages. Observed values:
`"commentary"` on intermediate turns and `"final"` on the closing
assistant turn (which also sets `end_turn: true`).

### `reasoning` variant

```json
{
  "type": "response_item",
  "payload": {
    "type": "reasoning",
    "summary": [],
    "content": null,
    "encrypted_content": "gAAAAABp5lf5…"
  }
}
```

`encrypted_content` is the actual reasoning trace, opaque ciphertext.
`summary` and `content` may be populated in configurations where
Codex exposes public reasoning. In my fixture they were empty/null on
every reasoning item.

### `function_call` and `function_call_output`

Arguments come through as a **JSON string** (not a parsed object),
verbatim from the model:

```json
{
  "type": "response_item",
  "payload": {
    "type": "function_call",
    "name": "exec_command",
    "arguments": "{\"cmd\":\"pwd\",\"workdir\":\"/Users/ben/...\",\"yield_time_ms\":1000,\"max_output_tokens\":200}",
    "call_id": "call_28LHwiJl0lxksoe0dJQ5iQ1y"
  }
}
```

The paired output carries the **textual tool response** verbatim:

```json
{
  "type": "response_item",
  "payload": {
    "type": "function_call_output",
    "call_id": "call_28LHwiJl0lxksoe0dJQ5iQ1y",
    "output": "Command: /opt/homebrew/bin/bash -lc pwd\nChunk ID: f1588c\nWall time: 0.0000 seconds\nProcess exited with code 0\nOriginal token count: 9\nOutput:\n/Users/ben/empathic/oss/toolpath\n"
  }
}
```

Pair function calls to outputs by `call_id`.

### `custom_tool_call` (free-form-argument tools)

`apply_patch` is the canonical example — arguments are **not JSON**
but a literal patch in Codex's V4A patch format:

```json
{
  "type": "response_item",
  "payload": {
    "type": "custom_tool_call",
    "status": "completed",
    "call_id": "call_sYb5HPObaiJRLYhllTHqbIxP",
    "name": "apply_patch",
    "input": "*** Begin Patch\n*** Add File: /Users/ben/…/Cargo.toml\n+[package]\n+name = \"codex-python-parser\"\n…\n*** End Patch"
  }
}
```

The matching `custom_tool_call_output`:

```json
{
  "type": "custom_tool_call_output",
  "call_id": "call_sYb5HPObaiJRLYhllTHqbIxP",
  "output": "{\"output\":\"Success. Updated the following files:\\nA …\",\"metadata\":{\"exit_code\":0,\"duration_seconds\":0.1}}"
}
```

`output` is a JSON string (double-encoded).

## `event_msg` — CLI-side events

Inner `type` values observed in my fixture:

| Inner type | Count | Meaning |
|---|---|---|
| `task_started` | 1 | Turn began |
| `user_message` | 1 | User prompt delivered |
| `agent_message` | 10 | Model message surfaced to the UI |
| `token_count` | 17 | Periodic token accounting |
| `exec_command_end` | 21 | Shell command finished |
| `patch_apply_end` | 3 | `apply_patch` completed (with per-file changes) |
| `task_complete` | 1 | Turn ended |

Upstream enumerates **much more** (~70 variants). The full list I
confirmed in `codex-rs/protocol/src/protocol.rs`:

**Lifecycle:** `TurnStarted`, `TurnComplete`, `TurnAborted`,
`SessionConfigured`, `ThreadNameUpdated`, `ShutdownComplete`

**Message content:** `AgentMessage`, `UserMessage`,
`AgentMessageDelta`, `AgentMessageContentDelta`, `AgentReasoning`,
`AgentReasoningDelta`, `AgentReasoningRawContent`,
`AgentReasoningRawContentDelta`, `AgentReasoningSectionBreak`

**Tool & action:** `McpToolCallBegin`, `McpToolCallEnd`,
`DynamicToolCallRequest`, `DynamicToolCallResponse`,
`WebSearchBegin`, `WebSearchEnd`, `ImageGenerationBegin`,
`ImageGenerationEnd`, `ExecCommandBegin`, `ExecCommandOutputDelta`,
`ExecCommandEnd`, `ViewImageToolCall`

**Approval & permission:** `ExecApprovalRequest`,
`ApplyPatchApprovalRequest`, `RequestPermissions`,
`RequestUserInput`, `ElicitationRequest`, `GuardianAssessment`

**Status & metadata:** `Error`, `Warning`, `TokenCount`,
`ModelReroute`, `ContextCompacted`, `ThreadRolledBack`,
`StreamError`, `DeprecationNotice`, `BackgroundEvent`

**Patch operations:** `PatchApplyBegin`, `PatchApplyUpdated`,
`PatchApplyEnd`

**Realtime conversation:** `RealtimeConversationStarted`,
`RealtimeConversationClosed`, `RealtimeConversationRealtime`,
`RealtimeConversationSdp`, `RealtimeConversationListVoicesResponse`

**History & planning:** `GetHistoryEntryResponse`, `TurnDiff`,
`PlanUpdate`, `PlanDelta`, various `Undo*` events

**Skills & MCP:** `ListSkillsResponse`, `McpStartupUpdate`,
`McpStartupComplete`, `McpListToolsResponse`,
`SkillsUpdateAvailable`

**Collaboration (multi-agent):** `CollabAgentSpawnBegin/End`,
`CollabAgentInteractionBegin/End`, `CollabWaitingBegin/End`,
`CollabCloseBegin/End`, `CollabResumeBegin/End`

**Review mode:** `EnteredReviewMode`, `ExitedReviewMode`

**Low-level:** `ItemStarted`, `ItemCompleted`, `HookStarted`,
`HookCompleted`, `RawResponseItem`, `TerminalInteraction`,
`ReasoningContentDelta`, `ReasoningRawContentDelta`

A permissive parser MUST accept unknown `event_msg.payload.type`
values — Codex adds events frequently.

### `token_count` detail

Populated once the turn has real usage data:

```json
{
  "type": "event_msg",
  "payload": {
    "type": "token_count",
    "info": {
      "total_token_usage": {
        "input_tokens": 11980,
        "cached_input_tokens": 9728,
        "output_tokens": 269,
        "reasoning_output_tokens": 41,
        "total_tokens": 12249
      },
      "last_token_usage": { /* same shape, for most-recent turn */ },
      "model_context_window": 258400
    },
    "rate_limits": {
      "limit_id": "codex",
      "primary":   { "used_percent": 1.0, "window_minutes": 300,   "resets_at": 1776721478 },
      "secondary": { "used_percent": 0.0, "window_minutes": 10080, "resets_at": 1777308278 },
      "credits": null,
      "plan_type": "team"
    }
  }
}
```

Absent/null `info` on the first `token_count` of a turn (delivered
before the model responds); populated thereafter.

**Cumulative vs. per-step — and the doubling trap:** per OpenAI's own
field definitions, `total_token_usage` is "cumulative tokens consumed
across the entire session" and `last_token_usage` is "the incremental
token delta for that specific event" (a single API call's tokens). Never
attribute the cumulative counter to a single turn (summing it per turn
grows quadratically). A step's own spend is the **increase** in
`total_token_usage` since the previous count. Crucially, derive that by
**differencing the cumulative**, not by summing `last_token_usage`: Codex
re-emits `token_count` events with a stale, repeated `last_token_usage`
(observed as duplicate events with identical values; OpenAI documents it
for rate-limit-only updates), so summing `last_token_usage` double-counts
— while a repeated cumulative total is a 0 delta. This is a known trap:
downstream tools that trust `last_token_usage` directly over-count
(openai/codex [#14489](https://github.com/openai/codex/issues/14489),
[#17539](https://github.com/openai/codex/issues/17539)). Each
`token_count` follows the step it measures (a `function_call` or a
`message`), so the delta attributes to that step.

**Round scoping + attribution:** a Codex round (one user task) can emit
several assistant messages (commentary + final) and many `token_count`
events. `toolpath-codex` groups a round's assistant turns under
`Turn.group_id` (the `turn_id` from `turn_context`/`task_started`),
records each per-step delta as that step's `attributed_token_usage`, and
sets the round's total `Turn.token_usage` (on its final turn) to the sum
of those attributions — one source of truth, so the total and the
per-step shares cannot drift, and `Σ token_usage == Σ attributed ==`
session total. Every field is per-step here (each step is a separate API
call re-sending context), so Codex attribution is full, not output-only.

**Reasoning slice of output:** `total_token_usage.reasoning_output_tokens`
is a **subset** of `output_tokens` (reasoning ⊆ output) and is itself a
cumulative session counter. `toolpath-codex` differences it per call the
*same* way as the other counters (never raw-summed — that would
double-count for the same reason `last_token_usage` does) and surfaces the
per-step reasoning delta under `attributed_token_usage.breakdowns["output"]["reasoning"]`,
with the round total carrying the summed reasoning under
`token_usage.breakdowns["output"]["reasoning"]`. Breakdowns are
**informational only**: they are never added into any total (the parent
`output_tokens` already counts those tokens), and the invariant
`Σ(reasoning) ≤ output` holds by construction. A breakdown entry is
written only when reasoning is `> 0`; zero-reasoning rounds leave the map
empty so the field is omitted.

### `exec_command_end` detail

```json
{
  "type": "event_msg",
  "payload": {
    "type": "exec_command_end",
    "call_id": "call_28LHwiJl0lxksoe0dJQ5iQ1y",
    "process_id": "52810",
    "turn_id": "019dabc7-97da-7510-908e-81597e638179",
    "command": ["/opt/homebrew/bin/bash", "-lc", "pwd"],
    "cwd": "/Users/ben/empathic/oss/toolpath",
    "parsed_cmd": [ { "type": "unknown", "cmd": "pwd" } ],
    "source": "unified_exec_startup",
    "stdout": "",
    "stderr": "",
    "aggregated_output": "/Users/ben/empathic/oss/toolpath\n",
    "exit_code": 0,
    "duration": { "secs": 0, "nanos": 4167 },
    "formatted_output": "",
    "status": "completed"
  }
}
```

`parsed_cmd` is Codex's best-effort structural parse of the command
(e.g. `git status` → `{"type":"git","args":["status"]}`). Unknown
commands use `{"type":"unknown","cmd":"…"}`.

### `patch_apply_end` detail

Carries the full per-file change manifest. Three change types:

| `type` | Fields |
|---|---|
| `add` | `content` (full new file body) |
| `update` | `unified_diff`, `move_path` (optional rename) |
| `delete` | (observed in upstream sources; not in my fixture) |

Example with both an add and an update:

```json
{
  "type": "event_msg",
  "payload": {
    "type": "patch_apply_end",
    "call_id": "call_sYb5HPObaiJRLYhllTHqbIxP",
    "turn_id": "…",
    "stdout": "Success. Updated the following files:\nA …Cargo.toml\n…",
    "stderr": "",
    "success": true,
    "changes": {
      "/abs/path/Cargo.toml": {
        "type": "add",
        "content": "[package]\nname = \"codex-python-parser\"\n…"
      },
      "/abs/path/src/runtime.rs": {
        "type": "update",
        "unified_diff": "@@ -173,3 +173,3 @@\n \n-#[derive(Debug, Default)]\n+#[derive(Debug)]\n pub struct Interpreter {\n",
        "move_path": null
      }
    }
  }
}
```

`patch_apply_end` is Codex's file-fidelity gold mine. It carries either
the full new content (adds) or a unified diff (updates) for every file
the model touched — equivalent to Gemini's `resultDisplay.fileDiff`
but richer. Any Toolpath derivation should prefer
`patch_apply_end.changes` over reconstructing diffs from raw patches
in `custom_tool_call.input`.

## Built-in tool catalogue

Verified by enumerating handler modules under
[`codex-rs/core/src/tools/handlers/`](https://github.com/openai/codex/tree/main/codex-rs/core/src/tools/handlers).

| Tool | Category | Handler | Notes |
|---|---|---|---|
| `shell` | Shell | `ShellCommandHandler` | Classic shell runtime |
| `exec_command` | Shell | `UnifiedExecHandler` | Unified exec; observed |
| `write_stdin` | Shell | `UnifiedExecHandler` | Write to running session; observed |
| `apply_patch` | FileWrite | `ApplyPatchHandler` | Custom-tool-call style; observed |
| `js_repl` | Shell | `JsReplHandler` | JavaScript REPL |
| `list_dir` | FileRead | `ListDirHandler` | Directory listing |
| `view_image` | FileRead | `ViewImageHandler` | Image content ingest |
| `plan` / `update_plan` | Planning | `PlanHandler` | Plan mode |
| `spawn_agent`, `close_agent`, `wait_agent`, `resume_agent`, `send_message`, `followup_task`, `list_agents` | Delegation | `multi_agents` / `multi_agents_v2` | Multi-agent collaboration |
| `agent_jobs` | Delegation | `BatchJobHandler` | Long-running agent jobs |
| `request_permissions` | User I/O | `RequestPermissionsHandler` | Asks user to broaden sandbox |
| `request_user_input` | User I/O | `RequestUserInputHandler` | Only in Plan mode |
| `tool_search` | FileSearch | `ToolSearchHandler` | Discover available tools |
| `tool_suggest` | FileSearch | `ToolSuggestHandler` | Recommend tools |
| `<mcp-ns>:<tool>` | variable | `McpHandler` | External MCP server tools |
| `mcp_resource` | FileRead | `McpResourceHandler` | MCP resource fetch |
| `dynamic` | variable | `DynamicToolHandler` | Per-thread dynamic tools |

Tool names can be either **plain** (`apply_patch`) or **namespaced**
(`<ns>:<name>`, used by MCP servers). See
[`codex-rs/protocol/src/tool_name.rs`](https://raw.githubusercontent.com/openai/codex/main/codex-rs/protocol/src/tool_name.rs).

## SQLite state

### `state_<N>.sqlite` — primary state

```
threads                Session/thread registry
agent_jobs             Long-running agent jobs
agent_job_items        Items within a job
jobs                   Generic job table
logs                   Per-thread activity log
stage1_outputs         Phase-1 processing outputs
thread_spawn_edges     Multi-agent parent/child graph
thread_dynamic_tools   Per-thread dynamic tool specs
backfill_state         Migration/backfill tracking
_sqlx_migrations       sqlx schema version tracking
```

The canonical schema for `threads`:

```sql
CREATE TABLE threads (
    id TEXT PRIMARY KEY,                 -- UUIDv7
    rollout_path TEXT NOT NULL,          -- Absolute path to the JSONL
    created_at INTEGER NOT NULL,         -- Unix seconds
    updated_at INTEGER NOT NULL,
    source TEXT NOT NULL,                -- "cli", "vscode", ...
    model_provider TEXT NOT NULL,
    cwd TEXT NOT NULL,
    title TEXT NOT NULL,                 -- First user message (heuristic)
    sandbox_policy TEXT NOT NULL,        -- JSON-encoded SandboxPolicy
    approval_mode TEXT NOT NULL,
    tokens_used INTEGER NOT NULL DEFAULT 0,
    has_user_event INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0,
    archived_at INTEGER,
    git_sha TEXT,
    git_branch TEXT,
    git_origin_url TEXT,
    cli_version TEXT NOT NULL DEFAULT '',
    first_user_message TEXT NOT NULL DEFAULT '',
    agent_nickname TEXT,
    agent_role TEXT,
    memory_mode TEXT NOT NULL DEFAULT 'enabled',
    model TEXT,
    reasoning_effort TEXT,
    agent_path TEXT
);
```

`threads.rollout_path` → points back to the JSONL; the SQLite is
effectively a cheap index over the rollout files. A reader should
prefer the JSONL as the source of truth and use SQLite only for
fast listing / cross-session queries.

### `logs_<N>.sqlite` — log ring

```sql
CREATE TABLE logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    ts_nanos INTEGER NOT NULL,
    level TEXT NOT NULL,
    target TEXT NOT NULL,
    feedback_log_body TEXT,
    module_path TEXT,
    file TEXT,
    line INTEGER,
    thread_id TEXT,
    process_uuid TEXT,
    estimated_bytes INTEGER NOT NULL DEFAULT 0
);
```

Structured tracing logs. Not needed for conversation reconstruction —
equivalent to `codex-tui.log` but indexed.

### Database versioning

Both databases are versioned by filename suffix:

- `state_5.sqlite` is the current primary state schema (as of CLI
  0.118.0).
- `logs_1.sqlite` is the current logs schema.

On breaking schema change, Codex writes a new `state_<N+1>.sqlite`
alongside the old one rather than migrating in place. Older Codex
binaries running in parallel can still open their previous version;
see `runtime_migrator()` in
[`codex-rs/state/src/migrations.rs`](https://raw.githubusercontent.com/openai/codex/main/codex-rs/state/src/migrations.rs).

SQL migrations are cumulative and live as `0001_*.sql` through
`0025_*.sql` files in
[`codex-rs/state/migrations/`](https://github.com/openai/codex/tree/main/codex-rs/state/migrations).

Override the SQLite location with `CODEX_SQLITE_HOME` or the
`sqlite_home` config key.

## Session resumption

`codex resume` reopens a session by either:

1. Reading the JSONL file at the `rollout_path` recorded in the
   `threads` table.
2. Rebuilding metadata from the first-line `session_meta` plus
   streaming through all subsequent items (see
   `metadata::builder_from_items` in
   [`codex-rs/rollout/src/metadata.rs`](https://raw.githubusercontent.com/openai/codex/main/codex-rs/rollout/src/metadata.rs)).

A forked session gets its own `rollout-*.jsonl` and its
`session_meta.forked_from_id` points to the original's `id`.

### Session index

`~/.codex/session_index.jsonl` — an append-only journal of
thread-name updates, used by the TUI to resolve "session by name"
queries. Latest entry wins. Not present in my fixture (empty), but
documented in upstream.

## Additional files

### `shell_snapshots/<thread-uuid>.<ns>.sh`

A dumped shell environment snapshot, sourced on sandboxed exec to
give the agent access to the user's aliases / exports / functions.
Plain bash script; version-tagged by thread UUID and nanosecond
counter.

### `memories/`

Write-only from within the sandbox (appears in every `turn_context`
as a `writable_root`). Agents persist long-term memory here via
`save_memory`-style tools.

### `skills/.system/`

Bundled skill packs shipped with the CLI. User-added skills live
alongside these.

### `cache/codex_apps_tools/`

Tool-definition cache (schema for each tool's parameter shape).

### `models_cache.json`

The catalogue of available models fetched from the provider, with
`client_version` + ETag for revalidation.

## Round-trip fidelity

These are the pitfalls for anyone parsing and re-emitting the format.

1. **Never parse `function_call.arguments` as JSON object at the
   rollout layer.** It's a string field by design — the model's raw
   output. Some arguments are valid JSON; some (for custom tools) are
   not. Store it as `String`, parse only when classifying the call.
2. **`custom_tool_call.input` is schema-free.** `apply_patch` uses V4A
   patch syntax; other custom tools use arbitrary formats. Type as
   `String`.
3. **`response_item.reasoning.encrypted_content` is opaque ciphertext.**
   Don't try to structure it. Preserve as-is for round-trip.
4. **The `patch_apply_end.changes[<file>].type` enum is open-ended.**
   `add` / `update` / `delete` are documented; future variants are
   likely. Use `Value` or `serde(other)` catch-all.
5. **Unknown `event_msg.payload.type` values will appear** in any
   non-trivial session. Codex adds events per release. Use `Other(String)`.
6. **`turn_context.sandbox_policy.type` is one of a small enum**
   (`read-only`, `workspace-write`, `danger-full-access`) but the
   inner fields vary per variant.
7. **Pair function calls to outputs by `call_id`.** Both `function_call`
   and `function_call_output` carry the same `call_id`; ditto for
   `custom_tool_call` / `custom_tool_call_output`.
8. **`token_count.info` may be null** on the first event of a turn.
9. **The file is append-only but written via a bounded background
   channel.** A crashed Codex process may leave the last few bytes of
   the JSONL mid-line. Skip-on-parse-error is the safe reader policy.
10. **`session_meta` always comes first**, `turn_context` precedes its
    turn's `response_item`s. Time ordering is otherwise the writer's
    arrival order; don't assume monotonic timestamps at sub-millisecond
    precision.
11. **`response_item.message` with `role: "user"` includes Codex-
    injected synthetic messages** ahead of the real prompt — a
    `<environment_context>` block in recent CLI versions, the project's
    `AGENTS.md` body in older ones. The TUI-facing
    `event_msg.user_message` is the authoritative source for "what the
    human actually typed." Prefer it for display; fall back to the
    `response_item` chain only when no `user_message` event is present.

## How `toolpath-codex` maps this

The mapping below is what the provider actually emits. Source:
[`crates/toolpath-codex/src/provider.rs`](../../../crates/toolpath-codex/src/provider.rs)
(rollout → `ConversationView`) and
[`derive.rs`](../../../crates/toolpath-codex/src/derive.rs)
(`ConversationView` → `toolpath::v1::Path`).

| Codex construct | Where it lands |
|---|---|
| `session_meta.id` | `ConversationView.id`, `path.id = path-codex-<first-8>` |
| `session_meta.cwd` | `Turn.environment.working_dir`, `path.base.uri` |
| `session_meta.git.commit_hash` | `path.base.ref_str` |
| `session_meta` (full) | `path.meta.extra["codex"]` (originator, cli_version, model_provider, git block, forked_from_id) |
| `turn_context.model` | `Turn.model` on subsequent assistant turns |
| `turn_context` (full) | `ConversationEvent` (round-trip preservation) |
| `message` role `user` | `Turn { role: User }` → Step with `actor: "human:user"` |
| `message` role `assistant` | `Turn { role: Assistant, model }` → Step with `actor: "agent:<model>"` |
| `message` role `developer` | `Turn { role: System }` → Step with `actor: "tool:codex"` |
| `reasoning.encrypted_content` | `Turn.extra["codex"]["reasoning_encrypted"]` (**not** `Turn.thinking` — it would render as ciphertext) |
| `reasoning.summary[].text` / `reasoning.content[].text` (plaintext) | `Turn.thinking` on the next assistant turn |
| `function_call` / `function_call_output` paired by `call_id` | `Turn.tool_uses[].{input, result}` |
| `custom_tool_call` / `_output` paired by `call_id` | same (raw `input` string preserved) |
| `event_msg.exec_command_end` | back-fills `Turn.tool_uses[].result` with exit code / stdout / stderr |
| `event_msg.patch_apply_end.changes[<file>]` | sibling `ArtifactChange` on the tool-call's turn with the unified diff as `raw` and `codex.{add,update,delete}` as `structural` |
| `event_msg.token_count.info.total_token_usage` | cumulative; differenced per step → `Turn.attributed_token_usage`, summed per round → `Turn.token_usage` (round's final turn) + `ConversationView.total_usage` |
| `event_msg.token_count.info.total_token_usage.reasoning_output_tokens` (⊆ output, cumulative) | differenced per step → `breakdowns["output"]["reasoning"]` on `attributed_token_usage`; summed per round onto `token_usage` (informational, never summed into the total) |
| `event_msg` non-turn types (`task_started`, `task_complete`, `user_message`, `agent_message`, etc.) | `ConversationView.events` as typed `ConversationEvent`s |
| unknown `response_item` / `event_msg` kinds | preserved verbatim in `events` and round-trip via `RolloutItem::Unknown` / `ResponseItem::Other` / `EventMsg::Other` |

### Fidelity guarantees

- **File changes** are lossless. Adds carry the full file content; the
  derive layer synthesizes a git-style `@@ -0,0 +N @@` diff header
  and prefixes every line with `+`. Updates carry Codex's real
  unified diff verbatim. No diff reconstruction from V4A patch input.
- **Wire-level round-trip** is asserted by
  [`tests/roundtrip.rs`](../../../crates/toolpath-codex/tests/roundtrip.rs):
  every `RolloutLine` in the fixture re-serializes to byte-equivalent
  JSON after key canonicalization. Every struct has
  `#[serde(flatten)] extra` and every externally-tagged enum has an
  `Other`/`Unknown` catch-all, so new upstream variants don't drop
  data.
- **Source → derived fidelity** is asserted by
  [`tests/fidelity.rs`](../../../crates/toolpath-codex/tests/fidelity.rs):
  timestamps, actor roles, call-id pairings, and patched file paths
  in the source rollout all survive the
  `Session → ConversationView → Path` pipeline without silent drops.

## References

- Codex CLI: <https://github.com/openai/codex>
- `RolloutItem` enum + session schema:
  <https://github.com/openai/codex/blob/main/codex-rs/protocol/src/protocol.rs>
- `ResponseItem` enum:
  <https://github.com/openai/codex/blob/main/codex-rs/protocol/src/models.rs>
- Tool handlers:
  <https://github.com/openai/codex/tree/main/codex-rs/core/src/tools/handlers>
- Rollout recorder (writes the JSONL):
  <https://github.com/openai/codex/blob/main/codex-rs/rollout/src/recorder.rs>
- Rollout metadata rebuild:
  <https://github.com/openai/codex/blob/main/codex-rs/rollout/src/metadata.rs>
- Session listing / pagination:
  <https://github.com/openai/codex/blob/main/codex-rs/rollout/src/list.rs>
- Config schema:
  <https://raw.githubusercontent.com/openai/codex/main/codex-rs/core/config.schema.json>
- SQL migrations (`0001` → `0025`):
  <https://github.com/openai/codex/tree/main/codex-rs/state/migrations>
- DB migration runtime:
  <https://raw.githubusercontent.com/openai/codex/main/codex-rs/state/src/migrations.rs>
