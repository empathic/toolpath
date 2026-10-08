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
- **Raw bytes** (files, request bodies) decode to those JSON bodies first
- **A mixed or id-less batch** splits into sessions first

The crate is pure: no filesystem, network, clock or logging.

## Usage

```rust,no_run
use toolpath_otel::{DeriveConfig, decode_input, derive_session, group_sessions};

let bytes = std::fs::read("capture.ndjson")?;
let requests = decode_input(&bytes, Some("capture.ndjson"))?;

// Tool categories come from the caller, here only Claude Code's Bash
let config = DeriveConfig::default().with_tool_category(|harness, tool| {
    (harness == "claude-code" && tool == "Bash").then_some(toolpath_convo::ToolCategory::Shell)
});

// A batch of any number of sessions -> one Path per session
let grouped = group_sessions(&requests, config.profile)?;
for session in &grouped.output {
    let derived = derive_session(session, &config)?;
    println!("{} -> head {}", session.key, derived.output.path.head);
}
println!("skipped {}", grouped.skipped.total());
# Ok::<(), Box<dyn std::error::Error>>(())
```

## API

| Item | Description |
|---|---|
| `derive(sessions, config)` | Main entry point. One session -> single-path Graph, several -> one path each |
| `derive_path(requests, config)` | Derive a Path from the request bodies of one session |
| `derive_graph(sessions, config)` | Derive a Graph, one path per session; skip counts are summed |
| `read_generations(requests, profile)` | Read request bodies (any sessions, any grouping) into a `GenerationBatch`: one `GenerationRecord` per model call and the messages their prompts name |
| `derive_path_from_records(records, messages, config)` | Derive a Path from one session's records, as `derive_path` does from the bodies they were read from |
| `derive_jsonl(records, messages, config, remote, settle, limits)` | Incremental send from records: the request bodies (`Vec<Body>`, split within `BatchLimits`) that bring a stored path up to the session's settled turns (`Settle::Settled`), or all of them (`Settle::Final`) |
| `GenerationRecord` | One model call, serde-serializable, format `GenerationRecord::FORMAT`; names its prompt by the hash of its last message. Accessors: `generation_id`, `session_id`, `start_ns`, `prompt`, `is_truncated`, `format` |
| `StoredMessage`, `MessageHash` | A prompt message linked to the one before it (`parent`), stored once under its `hash()` |
| `GenerationBatch` | `records` and `messages` (`BTreeMap<MessageHash, StoredMessage>`) |
| `Remote` | What the stored path holds: `opened`, `fed` (its `meta.otel.generation_ids`), `stored` (any subset of its step ids), `harness` (its `meta.otel.harness`), `base` (a continuation's frozen step ids) |
| `SourceHarness` | The agent recorded as `meta.otel.harness`: `as_str` and its inverse `from_name` |
| `Settle` | `Settled` (default: only turns no later call can change) or `Final` (the session is over) |
| `BatchLimits`, `Body`, `DeltaError` | Re-exported from `toolpath::v1::jsonl` |
| `decode_input(bytes, name)` | One file or request body -> its OTLP/JSON bodies (1 GiB decompression budget) |
| `decode_input_with_limit(bytes, name, limit)` | `decode_input` under the caller's decompression budget |
| `decode_input_with_limits(bytes, name, limits)` / `DecodeLimits` | `decode_input` under the caller's decompression budget and entry cap (`decompressed`, `entries`; `Default` is 1 GiB and 1,048,576) |
| `decode_protobuf(bytes)` / `encode_protobuf(value)` | One OTLP protobuf request <-> canonical OTLP/JSON (feature `protobuf`) |
| `group_sessions(requests, profile)` | Split a batch into `SessionRequests`, one per session; the batch's skip counts |
| `derive_session(session, config)` | Derive a Path from one `SessionRequests`, under its key |
| `SessionRequests` | A session's `key`, client `session_id` and cut-down `requests`; `derived_session_id()` |
| `derived_session_id(key)` | The UUID a session key derives to (`meta.otel.derived_session_id`) |
| `DeriveConfig` | `profile`, an optional graph `title`, the shared `convo` derivation options (a `toolpath_convo::DeriveConfig`: toolpath-convo is a public dependency, so a breaking toolpath-convo release is a breaking release here), and `tool_category`, the caller's tool classifier (`with_tool_category(f)` sets it) |
| `ToolClassifier` | Wraps `Fn(harness, tool_name) -> Option<ToolCategory>`; `harness` is the inferred harness id (`claude-code`, `codex`, `opencode`, `pi`, `unknown`) |
| `ProfileSelection` | `Auto` (default: `openrouter`, then `semconv`), `OpenRouter`, `Semconv`, `OpenInference` |
| `Derived<T>` | The derived document (`output`) and `skipped: SkipCounts` |
| `SkipCounts` | Telemetry read but not derived, by reason, and `total()` |
| `OtelError` | `NotOtlp`, `NoGenerations { skipped }`, `MixedSessions(ids)`, `FedGenerationMissing(id)`, `UnknownHarness(name)`, `MessageMissing(hash)`, `Delta(DeltaError)`, and the transport errors below |
| `OtelError::is_not_otlp()` | Input that is not OTLP at all (a file to skip), as opposed to malformed OTLP |
| `OtelError::is_too_large()` | Input over the decompression budget or the entry cap, as opposed to corrupt data |

## Features

| Feature | Enables |
|---|---|
| `compression` | gzip (`flate2`) and zstd (`ruzstd`) input, both pure Rust |
| `protobuf` | OTLP protobuf bodies, Collector file-exporter frames, `decode_protobuf`/`encode_protobuf` |

Both are off by default. Input that needs a missing feature is
`OtelError::FeatureDisabled`.

## Transport

`decode_input` reads a JSON body, JSON lines (BOM and CRLF accepted), an
OTLP/HTTP protobuf body (`ExportTraceServiceRequest` or
`ExportLogsServiceRequest`), the OpenTelemetry Collector file exporter's
length-prefixed frames, and gzip or zstd around any of them, up to 4
nested layers. The content decides the decoding; `name` only enables
file-extension rules (`.pb`, `.binpb`, `.gz`, `.zst`).

Decompressed output is capped by **one budget** across every layer and
frame of the input, and the decoded tree by **one entry cap** (array
and repeated-field elements, and nested objects, counted before they are
built). A service decoding untrusted bodies passes its own to
`decode_input_with_limit` or `decode_input_with_limits`; going over is
`OtelError::TooLarge { limit }` or `OtelError::TooManyEntries { limit }`
(both `is_too_large()`), which an HTTP caller maps to
`413 Payload Too Large`. Other transport errors: `Json`, `Decompress`,
`NotOtlpBody`, `Protobuf`, `Framing`.

## Session grouping

The caller groups requests, or records by `GenerationRecord::session_id`,
into sessions; order does not matter. Every generation that carries a
client session id (`session.id`, or `gen_ai.conversation.id` for
`semconv`) must carry the same one, or the call returns
`OtelError::MixedSessions`; generations without an id belong to the
session as given. For traffic that mixes sessions or carries no id,
`group_sessions` groups generations in layer order, first match wins in
start order:

| Layer | Groups by | Session key |
|---|---|---|
| 1 | Client session id (`session.id`, `gen_ai.conversation.id`) | The id |
| C | The session of the generation it continues | That session's |
| 2 | Full-history requests without an id: the session whose latest prompt this one extends, per client key | `otel-cluster:<16 hex>` |
| T | Delta requests: client key and trace | `otel-trace:<16 hex>` |

Each session's `requests` are the bodies cut down to its own spans and log
records, plus the other spans and records of its traces. `derive_path` over
them gives what `derive_session` gives, except when a derived key equal to a
client session id of the batch took a `-<n>` suffix, which only
`derive_session` keeps.

## Tool categories

The crate knows no harness's tool names. A tool call's `ToolCategory` comes
from the classifier in `DeriveConfig::tool_category`, asked with the
inferred harness id and the tool name; pass each harness's provider-crate
classifier (e.g. `toolpath_claude::provider::tool_category`) to categorize
as that harness's own deriver does. `DeriveConfig::default()` has no
classifier, so no tool call is categorized: no sub-agent is recognized
(that needs `Delegation`) and no file change is read from a tool call (that
needs `FileWrite`).

## Generation records

A store that keeps a session as it arrives keeps records, not request
bodies. `read_generations` reads each delivery once into one
`GenerationRecord` per model call and the `StoredMessage`s its prompt
names; the skip counts come from this read. A record serializes to a few
hundred bytes: ids, timing, completion, usage, cost, models, and the hash of
its last prompt message. Messages link to the one before them, so each
distinct prompt prefix is stored once and storage grows with the session's
new messages, not with the full history every call repeats (1,000
synthetic full-history calls: 2.26 GB of OTLP, 917 MB of repeated prompt
JSON, 3.1 MB of records plus 2.2 MB of messages).

Deriving from records gives byte for byte what deriving from the bodies
does, after a JSON round trip too, and every derived id depends only on
record and message content. The one exception: message hashes are over
RFC 8785 (JCS), which spells a number by its value, so a message repeated
with `1.0` for `1` (or `-0.0` for `0`) shares the first spelling's hash and
a store keeps that spelling. The read parses each distinct message once
and shares prompts that extend each other, and stitching matches prompt
prefixes by hash, so a derive costs the session's new messages rather than
its repeated history. Reading one delivery at a time is the same as
reading them all at once as long as no call's telemetry spans two
deliveries (OpenRouter Broadcast sends each call whole).

## Incremental sends

`derive_jsonl` serves a session that is still growing, as appends to a
Pathbase path (the streaming JSONL routes). It keeps no state: everything it
needs is the session's records and what it reads back from the stored path.

```rust,no_run
use std::collections::{BTreeMap, HashSet};
use toolpath_otel::{
    BatchLimits, DeriveConfig, GenerationRecord, MessageHash, ProfileSelection, Remote, Settle,
    StoredMessage, derive_jsonl, read_generations,
};

# let delivery: serde_json::Value = serde_json::json!({"resourceSpans": []});
# let mut records: Vec<GenerationRecord> = Vec::new();
# let mut messages: BTreeMap<MessageHash, StoredMessage> = BTreeMap::new();
// At ingest: read the delivery once, append its records (in arrival
// order), add the messages the store lacks (never replace one: two
// spellings of a number share a hash).
let read = read_generations(&[delivery], ProfileSelection::Auto)?;
records.extend(read.output.records);
for (hash, message) in read.output.messages {
    messages.entry(hash).or_insert(message);
}

// From the stored path: does it exist, its meta.otel.generation_ids,
// the step ids known to be stored (any subset; empty resends everything)
// and its meta.otel.harness.
let remote = Remote {
    opened: true,
    fed: vec!["gen-1".into(), "gen-2".into()],
    stored: HashSet::new(),
    harness: Some("claude-code".into()),
    base: HashSet::new(),
};
let limits = BatchLimits::new(Some(4 << 20), Some(1000));
let sent = derive_jsonl(
    &records,
    |h| messages.get(h),
    &DeriveConfig::default(),
    &remote,
    Settle::Settled,
    limits,
)?;
for body in &sent.output {
    // One append per body, in order: POST .../paths (opening) or .../steps.
    let _ndjson: &str = &body.text;
    let _sent_now: &[String] = &body.step_ids;
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

- **What the caller passes.** All the session's records so far, in the
  order they arrived, and a lookup of their messages; `opened`, whether the
  path exists; `fed`, the stored path's
  `meta.otel.generation_ids` (empty before the first send); `stored`, step
  ids the path holds. Any subset of `stored` is correct: a step left out is
  sent again, identically, and the store keeps one copy. An empty `stored`
  (resend everything settled) is correct until Pathbase can list a path's
  step ids. Claiming a step the path lacks is not. The lookup must return
  the message stored under exactly the hash asked for; it is not checked.
  `config.tool_category` categorizes tools and decides delegation calls for
  every derivation of the call; pass the same classifier on every send of
  a session, or a stored step compares as changed (`Amended`).
- **One copy of each call.** Of two records of one call (with `Auto`, an
  app-side `semconv` span and OpenRouter's Broadcast root share its
  `gen-…` id), `derive_jsonl` derives the first received and counts the
  other as `duplicate`, so a copy that arrives later never changes a sent
  step. `derive_path` and `derive_path_from_records` keep the
  better-ranked profile's copy, so the stream reads back to them only when
  that copy arrives first. With `ProfileSelection::OpenRouter` the case
  cannot arise.
- **What the caller stores.** The records and messages, nothing else. Each
  body is one append; send them in order. No bodies means no turn has
  settled yet. A call costs a derive of the records, and with `stored`
  given a second derive of the generations already sent, to check that no
  stored step changed: about 0.21 s of CPU for the 1,000th call of a
  synthetic session of one linear full-history conversation (0.74 s for
  the 2,000th). Deciding the harness stitches about `n log n`
  generations in all, whatever the session's shape: 22 ms per call for
  1,000 requests that never share history (which never settle).
- **`PathOpen` or `PathMeta`.** The first body starts with a `PathOpen` when
  `opened` is false, else with a `PathMeta` patch. Either carries the new
  feed order, so it commits with the first steps.
- **`Head` per body.** Every body ends with a `Head` naming a step stored by
  then: the latest settled main-line step so far, and the real head on the
  last body. `toolpath`'s batcher splits the steps within `limits` (bytes
  and steps per body; a step is never split).
- **Settled turns only.** A turn goes out once no later request can change
  it, its marks or its parents; `Settle::Final` sends the rest once the
  session is over. Requests that arrive out of start order only append, and a late
  sub-agent never takes the head.
- **What waits.** Besides turns whose calls or echo are still pending:
  everything outside a tree a missing continuation started until the main
  line is decided (the first tree's second produced turn), a
  side request until `Settle::Final`, and a delegating thread's turns after a
  delegation call (one the classifier names `Delegation`, such as Claude
  Code's `Task`/`Agent`) until that sub-agent's answer is found, because the
  turn that receives it gains an extra parent. A sub-agent's turns go out
  as soon as they settle, marked with their call. A turn from a later
  generation also waits for an unanswered call that may move a persistent
  shell (a Claude Code `Bash` `cd` the classifier names a `Shell` call, a
  worktree tool), since its shell-write stamps read where that call left
  the shell.
- **Harness fixed at the first settled turn.** The harness (tool
  categories, delegation calls, `producer.name`) is inferred from the
  generations that settle the session's first turn and kept for the rest
  of it: the first send stores it as `meta.otel.harness`, and a later call
  passes it back as `Remote::harness` (when absent the feed order decides
  it again, with the same answer except after a `Settle::Final` send before
  any turn settled followed by a late arrival; always pass the stored
  harness). `derive_path` applies the same rule, so
  the last send and the one-shot derivation agree.
- **Continuing a frozen path.** Pass the frozen path's step ids as `base` on
  every send of the continuation: the first with the frozen path's `fed` and
  `opened` false, the next ones with the continuation's own `fed`, `stored`
  and `opened` true. The bodies open the new path once and hold only new
  steps; a `base` step is never sent and never the `Head`, only a parent of
  the new steps that continue from it, wherever it sits (a late sub-agent
  forks off old history). The first send passes the frozen path's
  `meta.otel.harness` too: the continuation keeps it, so the session's
  paths agree on tool categories and `producer.name`, unless it is
  `unknown`, which the whole feed may refine. Pathbase anchoring a
  continuation root on any frozen step is an open point.
- **Resumed sub-agents.** A sub-agent's answer goes out once the turn that
  receives it is seen. A resumed sub-agent (Claude Code `SendMessage`,
  Codex `send_message`) echoes the answer in its next request; that echo
  is left off the answer's step when it comes later in feed order, so the
  sent step never changes.
- **`Amended`: a sent step would change.** A step sent unsettled by a
  `Settle::Final` call can later settle differently. That is a mutation, never an
  append: record it through Pathbase's mutation log (planned), never by
  sending or copying the step again. `derive_jsonl` reports
  `OtelError::Delta(DeltaError::Amended)` for a step listed in `stored`; a
  step left out cannot be checked, so its changed version goes out and
  Pathbase refuses the body with nothing written.

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
| Heredoc writes and `apply_patch` heredocs in calls the classifier names `Shell` | `change[file]` file changes with `extra.otel` `source` (`shell-heredoc`, `shell-apply-patch`), `outcome` and `executions[]`; a target that cannot be resolved, and each target of a write form not modeled (`cmd > f`, `… \| tee f`), is an attempt in the step's `extra.otel.unresolved_shell_writes` |
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
