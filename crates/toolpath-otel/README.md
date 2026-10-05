# toolpath-otel

Derive Toolpath provenance documents from OpenTelemetry LLM telemetry.

Coding agents increasingly emit OpenTelemetry: gateways such as OpenRouter
Broadcast mirror every model call as a trace, and the OpenTelemetry GenAI
instrumentations record prompts, completions and tool calls on spans or log
records. That telemetry knows *what* each model call saw and said, but not
how the calls form one session. This crate turns the telemetry of one agent
session into an `agent-coding-session` Toolpath document where every turn
becomes a step with a typed actor, its tool calls and results, token usage,
and the file changes its tools made.

## Overview

Input is the parsed OTLP/HTTP JSON request bodies (`resourceSpans` and/or
`resourceLogs`) of one session. Each model call is read through a
**profile** into a neutral request record; the records are stitched into a
turn DAG with chained content ids, so identical prompt prefixes collapse,
divergence forks, and ids stay stable as the session grows.

- **One session** produces a `Path`
- **Several sessions** produce a `Graph` of paths

The crate is pure: no filesystem, network, clock or logging.

## Usage

```rust,no_run
use toolpath_otel::{DeriveConfig, derive_path};

let text = std::fs::read_to_string("session.ndjson")?;
let requests: Vec<serde_json::Value> = text
    .lines()
    .map(serde_json::from_str)
    .collect::<Result<_, _>>()?;

// One session -> Path document, plus what was read but not derived
let derived = derive_path(&requests, &DeriveConfig::default())?;
println!("head {}", derived.output.path.head);
println!("skipped {}", derived.skipped.total());
# Ok::<(), Box<dyn std::error::Error>>(())
```

## API

| Item | Description |
|---|---|
| `derive(sessions, config)` | Main entry point. One session -> single-path Graph, several -> one path each |
| `derive_path(requests, config)` | Derive a Path from the request bodies of one session |
| `derive_graph(sessions, config)` | Derive a Graph, one path per session; skip counts are summed |
| `DeriveConfig` | `profile`, an optional graph `title`, and the shared `convo` derivation options (a `toolpath_convo::DeriveConfig`: toolpath-convo is a public dependency, so a breaking toolpath-convo release is a breaking release here) |
| `ProfileSelection` | `Auto` (default: `openrouter`, then `semconv`), `OpenRouter`, `Semconv`, `OpenInference` |
| `Derived<T>` | The derived document (`output`) and `skipped: SkipCounts` |
| `SkipCounts` | Telemetry read but not derived, by reason, and `total()` |
| `OtelError` | `NotOtlp`, `NoGenerations { skipped }`, `MixedSessions(ids)` |

## Sessions

The caller groups requests into sessions; request order does not matter.
Every generation that carries a client session id (`session.id`, or
`gen_ai.conversation.id` for `semconv`) must carry the same one, or the call
returns `OtelError::MixedSessions`. Generations without an id belong to the
session as given.

## Profiles

| Profile | Reads |
|---|---|
| `openrouter` | OpenRouter Broadcast traces; its connection tests are skipped and counted |
| `semconv` | OpenTelemetry GenAI semantic conventions, content on span attributes or log records |
| `openinference` | OpenInference LLM spans |

## Skip reasons

| `SkipCounts` field | Meaning |
|---|---|
| `error_status` | The generation's span status is an error |
| `connection_test` | An OpenRouter destination or settings test |
| `duplicate` | Another copy (complete or cut off) of a generation that is kept |
| `truncated` | A prompt or completion that is cut off or malformed |
| `missing_payload` | A claimed span with no generation id or nothing to build from |
| `unclaimed` | Spans and orphan log records no profile claims |

## Mapping

| OpenTelemetry concept | Toolpath concept |
|---|---|
| Session (`session.id`, else a key from its first generation) | Path; `meta.otel.derived_session_id` |
| Model call (generation) | The step(s) for the turns it adds |
| Prompt message | Conversation turn, stitched by chained content id (`step.id`) |
| User / system message | `step.actor` as `human:user` / `tool:otel` |
| Assistant completion | `step.actor` as `agent:<model>` |
| Tool call and its result | `tool_uses` on the turn's conversation change |
| Sub-agent started by a `Task`/`Agent` call | `delegations` on the delegating turn (every such call with a prompt); its turns stay steps marked `extra.otel.branch = "subagent"`, and its answer is an extra parent of the turn that receives it, so they are not dead ends |
| Side request (another system prompt) | Steps marked `extra.otel.branch = "side"` once the main line is decided: the first system prompt, or tree continuing a missing request, to produce two turns; `path.head` is the last unmarked turn |
| Generation repeating an existing turn (identical retry) | A dead-end step marked `extra.otel.branch = "unplaced"` that carries its tokens |
| Call whose content was not captured (skeleton) | Steps marked `extra.otel.branch = "skeleton"` and `extra.otel.absent` |
| Write, edit, `NotebookEdit`, `apply_patch` and opencode `delete` tool calls | `change[file]` file changes |
| Token usage | `token_usage` on the step, additive: `input_tokens` excludes cache reads and writes (`cache_read_tokens`, `cache_write_tokens`); the source's convention is `extra.otel.usage.cache_basis` |
| Cost | Per-call `extra.otel.cost`; `meta.otel.cost_usd` totals are `null` when any call is unpriced |
| Working directory from the inferred harness's own cwd marker (the system prompt; Codex's `<environment_context>` user message) | `path.base.uri` |
| Agent recognised from session ids, prompts and tool names | `meta.producer.name` (`claude-code`, `codex`, `opencode`, `pi`, else `otel`) and `meta.otel.harness`; `meta.source` is `otel` |
| Everything needed to rebuild the requests | `structural.extra.otel` |

See `docs/agents/formats/otel.md` for the full format reference.

## Part of Toolpath

This crate is part of the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) -- core types and query API
- [`toolpath-convo`](https://crates.io/crates/toolpath-convo) -- provider-agnostic conversation derivation
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) -- derive from Claude conversations
- [`toolpath-git`](https://crates.io/crates/toolpath-git) -- derive from git history
- [`path-cli`](https://crates.io/crates/path-cli) -- unified CLI (`cargo install path-cli`)
- [RFC](https://github.com/empathic/toolpath/blob/main/RFC.md) -- full format specification
