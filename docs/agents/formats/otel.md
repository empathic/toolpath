# OpenTelemetry LLM traces (OTLP)

Read by `toolpath-otel`. The OpenRouter Broadcast profile was observed in M0
(2026-09-28) with a Broadcast "OpenTelemetry Collector" destination; the
`semconv` and `openinference` profiles were captured from the official
instrumentations against local mock servers (see Pinned instrumentation
behavior; the event-mode captures, with content on log records, are
described under Event-mode captures). Keep in sync with
`crates/toolpath-otel/src/{lib,otlp,walk,session,stitch,branch,provider,derive,hash}.rs`,
`crates/toolpath-otel/src/walk/logs.rs`,
`crates/toolpath-otel/src/harness/` and
`crates/toolpath-otel/src/profile/{openrouter,semconv,openinference}.rs`.

There is no local store: a collector (or OpenRouter Broadcast) delivers
OTLP/HTTP request bodies, and the caller hands the bodies of one session to
`toolpath_otel::derive_path`.

## Input

```rust
pub fn derive_path(requests: &[serde_json::Value], config: &DeriveConfig) -> Result<Derived<Path>>
pub fn derive_graph(sessions: &[&[serde_json::Value]], config: &DeriveConfig) -> Result<Derived<Graph>>
pub fn derive(sessions: &[&[serde_json::Value]], config: &DeriveConfig) -> Result<Derived<Graph>>

pub struct Derived<T> { pub output: T, pub skipped: SkipCounts }
```

Each request is one parsed OTLP/HTTP JSON body: a JSON object holding at
least one of `resourceSpans`, `resourceLogs`, `resourceMetrics` (an array or
`null`). Anything else is `OtelError::NotOtlp`, and the whole call fails. A
logs- or metrics-only body is OTLP. When no generation can be read
(every span unclaimed, skipped, or an error) the call is
`OtelError::NoGenerations { skipped }`. Generations carrying more than one
client session id are `OtelError::MixedSessions` (the sorted ids; see
Sessions and ids). Decoding the transport (protobuf bodies,
Collector file-exporter frames, gzip and zstd) is the caller's.

`resourceSpans`: `{resource, scopeSpans:[{scope, spans:[…]}]}`;
`resourceLogs`: `{resource, scopeLogs:[{scope, logRecords:[…]}]}`, each
record read for `timeUnixNano`, `observedTimeUnixNano`, `body`,
`attributes`, `eventName`, `traceId` and `spanId`.

What was read but not derived is counted in `SkipCounts` (summed over the
sessions for a graph; `total()` adds them up): `error_status`,
`connection_test`, `duplicate`, `truncated` and `missing_payload` per
skipped generation (the reasons below), and `unclaimed` per distinct
span (once per `(traceId, spanId)`, however often it was delivered; each
span with no span id counts) and distinct orphan log record no consulted
profile claims.
Attribute values and log bodies are OTLP `AnyValue`s (`stringValue`,
`intValue` as a string or number, `doubleValue`, `boolValue`,
`arrayValue`, `kvlistValue`, `bytesValue`); an integer attribute also
reads from a `doubleValue` with no fractional part. Timestamps are
nanosecond strings (numbers accepted); `traceId`, `spanId` and
`parentSpanId` are hex, lowercased as they are read (OTLP/JSON hex is
case-insensitive), so every comparison, dedupe key, generation id and
`Generation.trace_id` sees one spelling; unknown
fields are ignored. Log records are read as described under Logs; metrics
are ignored without counting.

## Walker

Every request of a call is indexed before anything is resolved, spans and log records alike, so
dedupe, the ancestor rule and log correlation see the whole input. Per
span:

1. The consulted profiles are asked in order (`DeriveConfig::profile`;
   `ProfileSelection::Auto`: `openrouter`, then `semconv`; `openinference`
   is never consulted under auto; any other selection: only that one). A profile that **claims** the
   span makes it a candidate unit; one
   that **absorbs** it (a vendor child span whose content the unit already
   carries) consumes it silently.
2. Neither claimed nor absorbed by any consulted profile → counted as
   `unclaimed`.
3. **Ancestor rule.** A candidate with a qualifying ancestor in the same
   trace (followed through `parentSpanId`) is absorbed into the outermost
   one instead of becoming a unit. An ancestor qualifies when it is itself
   a candidate of the same profile, or of a profile ranked at or before
   the descendant's. A parent missing from the input ends the walk up; a
   `parentSpanId` cycle or a depth of 4096 abandons that walk and the
   candidate stays a unit, so malformed parent links never hang or drop
   anything.

Then per unit: first every span unit in input order, then the orphan log
units (see Logs):

1. The profile's pre-skip (OpenRouter: the destination test and the
   Broadcast test generation, counted as `connection-test`).
2. The profile identifies the generation id and session id.
3. **Status gate** (span units only; an orphan unit has no span). Not
   an error when `status` is absent or `null`, or its `code` is absent,
   `null`, `0`, `1`, `"0"`, `"1"`, `STATUS_CODE_UNSET` or
   `STATUS_CODE_OK` (case-insensitive). Anything else, an unreadable status
   included, is an error (**fail closed**): counted as `error-status`.
4. A generation id already seen → `duplicate` (the same capture present
   twice). Within one profile the
   first in input order wins. Across profiles the better-ranked profile
   (earlier in the consulted order) wins whatever the input order: OpenRouter
   returns its `gen-…` id to the caller, so an app-side semconv `chat` span
   and the Broadcast root for the same call share an id, and the
   `openrouter` generation is kept while the `semconv` one becomes the
   `duplicate` skip.
5. The profile extracts the generation: no generation id →
   `missing-payload`; a missing prompt or completion makes that side
   **absent** (a skeleton, see Skeletons); a prompt or completion that is
   present but does not parse → `truncated`. A generation id counts as seen
   only after a successful extract, so a truncated first copy never hides a
   good redelivery.

A profile's `TraceView` lists each `(traceId, spanId)` once, the first in
input order; a span with no span id is always listed. Candidates are not
affected: a redelivered unit is handled by the generation-id dedupe.

## OpenRouter Broadcast

Profile name `openrouter`. One trace per API request, resource
`service.name = "openrouter"` (plus `service.version` and
`openrouter.trace.id`, the generation id), scope `openrouter`.

| Span | Profile rule |
|---|---|
| `LLM Generation` (root, `kind = 3`) | Claimed; carries everything read. |
| `openrouter-connection-test` | Claimed, then skipped as `connection-test` (destination test, no attributes). |
| `Test Generation` (root) | The Broadcast settings' test button: a canned conversation with `gen_ai.*` attributes. Claimed only with the `service.name` or scope marker, then skipped as `connection-test`. |
| `generation`, `provider attempt N: <Provider>` | Absorbed. They repeat `gen_ai.operation.name = "chat"` and a suffixed `gen_ai.response.id` (`<id>:generation`, `<id>:attempt-0`); their content duplicates `trace.metadata.provider_responses`. |

A span counts as OpenRouter's when its resource `service.name` is absent or
`"openrouter"`, its scope is `openrouter`, or (for claimed names) it carries
any `trace.metadata.openrouter.*` attribute, so a Collector that rewrites
resources still works. A `generation` or `provider attempt` span with none
of these markers is `unclaimed`.

Root status `code` 2 is an error such as "The client has disconnected",
with no cost; the successful retry follows.

| Root attribute | Meaning |
|---|---|
| `gen_ai.prompt` | JSON string `{"messages":[…]}`: the full request history in OpenAI chat shape, whatever the upstream API. `span.input` duplicates it and is not read. |
| `gen_ai.completion` | JSON string `{completion, reasoning, toolCalls, tools, rawRequest}`. `span.output` duplicates it and is not read. |
| `session.id` | Harness session id when the client sends one (Claude Code UUIDv4, Codex UUIDv7, opencode `ses_…`; pi a UUIDv7 in recent releases, none in older ones). |
| `user.id` | Claude Code only (64-hex hash). |
| `gen_ai.request.model` / `gen_ai.response.model` | Requested vs. routed model. |
| `gen_ai.provider.name`, `gen_ai.system` | e.g. `anthropic`, `openai`. |
| `gen_ai.response.id` | Generation id (`gen-…`); the dedupe key. |
| `gen_ai.response.finish_reason` | Normalized (`tool_calls`, `stop`); the upstream value is in `trace.metadata.openrouter.finish_reason`. |
| `gen_ai.usage.*` | Tokens: `input_tokens`, `output_tokens`, `input_tokens.cached`, `input_tokens.cache_write`, `…cache_write_5m`, `…cache_write_1h`, `output_tokens.reasoning`, `total_tokens`. Cost in USD: `input_cost`, `output_cost`, `total_cost`. |
| `trace.metadata.openrouter.*` | `api_key_name`, `creator_user_id`, `entity_id`, `user_id`, `provider_name`, `provider_slug`, latencies (`first_token_ms`, `router_latency_ms`, …), unit prices. |
| `trace.metadata.provider_responses` | JSON string: per-attempt provider, status, latency, endpoint, model permaslug. |

`trace.metadata.device_id` is ignored. There is no upstream message id (no
Anthropic `msg_…`). Tool-call ids come from the upstream provider
(`toolu_bdrk_…`, `call_…`) and match the harness's own transcript exactly.

Attributes are capped at 10,000,000 characters. The marker at the cap is
unknown (M0 never hit it); a cut-off `gen_ai.prompt` or `gen_ai.completion`
fails to parse and the generation is skipped as `truncated`.

A Privacy Mode span (OpenRouter withholds `gen_ai.prompt`, `gen_ai.completion`,
or both) is a **skeleton**: the side that is missing is marked absent and the
span still yields a turn with its usage, model and ids (see Skeletons below).
A payload attribute whose value is `null` or an empty `AnyValue` (`{}`, or
only null fields) counts as withheld; one that is present but not a
`stringValue`, or does not parse, is `truncated`. Only a
span with no `gen_ai.response.id` is skipped as `missing-payload`.

### Messages

Roles `system`, `developer` (Codex), `user`, `assistant`, `tool`. `content`
is a string, a list of parts (`{"type":"text","text":…}`, possibly with
`cache_control`), or null; the same message may switch encodings between
requests. Assistant messages carry `tool_calls` (`arguments` is a JSON
string) and may echo thinking in `reasoning_details`. Tool messages carry
`tool_call_id`, `content`, and optionally `name`, `is_error`.

Harness quirks the converter handles:

- Claude Code inserts `system` messages after the first user message and
  after tool results (environment and reminder blocks); they are set aside
  from comparison and retained (see Retention).
- History echoes rewrite tool arguments: Claude Code strips a leading
  `cd <cwd> &&` from Bash commands and normalizes `Write` content, and
  argument JSON spacing differs from the completion's.

### Harness and working directory

The harness is inferred from the conversation and recorded in path meta
(`meta.extra.otel.harness`: `claude-code`, `codex`, `opencode`, `pi`, or
`unknown`). A known harness also names the producer,
`meta.extra.producer.name`, with the names Pathbase's harness dimension and
the kind's `source` list use (`claude-code`, `codex`, `opencode`, `pi`);
an `unknown` harness gives `otel`. `producer.version` is never set (the
telemetry carries no harness version). `meta.source` stays `otel`: it names
the derivation, not the harness. The harness's own deriver may name the
producer differently: `toolpath-codex` writes Codex's `originator`
(`codex-tui`, `codex_exec`, …), and `toolpath-pi` sets no producer.
`toolpath-otel`'s harness set has no Gemini CLI entry and no cwd marker for
it, so a Gemini session records whatever the rules find (usually
`unknown`). The working directory becomes `path.base`:

| Harness | cwd marker |
|---|---|
| Claude Code | `Primary working directory: …` in a system message |
| Codex | `<environment_context><cwd>…</cwd>` in a `user` message |
| opencode | `Working directory: …` in the `<env>` block of the system prompt |
| pi | `Current working directory: …`, or a `<cwd>…</cwd>` section in newer releases |

The markers come from the harnesses' public sources, never from captured
traces: opencode's `<env>` block in sst/opencode
`packages/opencode/src/session/system.ts`; pi's line form in badlogic/pi-mono
`packages/coding-agent/src/core/system-prompt.ts` (at `671798d`, before pi's
XML prompt sections); Claude Code, which is closed source, from the
environment block its prompts carry. For a known harness, only that
harness's marker is read, and only in the role the table names (system
covers developer); the first match wins. Codex's AGENTS.md and a typed
prompt are `user` messages, so a `Working directory:` line in them never
sets the cwd of a Codex session. When a known harness's own marker is
absent (another release's prompt), the session has no cwd: another
harness's marker is never read, even in a system message. For an
`unknown` harness, the first marker of any harness in a system, developer
or user message wins. Assistant and tool messages are never scanned, since
a tool's output can quote a marker.

### Tool categories

A tool call's category comes from the inferred harness's own table, the
same mapping its provider crate uses (`toolpath-claude`, `toolpath-codex`,
`toolpath-opencode`, `toolpath-pi`; pi lowercases and treats any name
containing `task` or `agent` as delegation). One addition: under codex,
`shell_command` (codex-rs `core/src/tools/spec.rs`) is `shell`;
`toolpath-codex` does not list it. For an `unknown` harness a name gets a
category only when every harness table that lists it exactly (those four
plus Gemini CLI, Copilot CLI and Cursor) agrees, so `ls`, `list`,
`list_dir` and `list_directory` stay uncategorized. These tables live in
`crates/toolpath-otel/src/harness/tools.rs` for now; a later otel-only
change replaces them with the shared `toolpath_convo::tools` tables once
those land.

### File changes

File changes come from edit/write tool calls (`Write`, `Edit`,
`MultiEdit`, `NotebookEdit`, `write`, `edit`, `write_file`; opencode and pi
key spellings are canonicalized onto Claude's) and `apply_patch`/`patch`
text (one change per file, `operation` `add`/`update`/`delete`,
`rename_to`, `after` for an added file). `NotebookEdit` names its file in
`notebook_path`, and its `new_source` is the change's `after`. opencode's
`delete` gives `operation` `delete`, as `toolpath-opencode` does. A MultiEdit-shaped call (an `edits` array) that `toolpath-convo`'s
own fallback reads the same way (Claude key names) is left to that
fallback, which also records the `edits` array in `structural`; other
spellings (pi's `oldText`/`newText`, opencode's `filePath`) are
canonicalized and carry the diff without `edits`. Files a harness writes through its
shell tool (e.g. Codex `exec_command` running `cat <<'EOF' > file`) are not
recorded here.

## Profile `semconv` (OpenTelemetry GenAI semantic conventions)

Consulted under `ProfileSelection::Auto` after `openrouter`, and alone under
`ProfileSelection::Semconv`. Every `gen_ai.*` item is `Development` status upstream
(`open-telemetry/semantic-conventions-genai`); old and new spellings are
both read.

**Claims and absorbs.** A span whose `gen_ai.operation.name` is `chat`,
`text_completion` or `generate_content` is a generation. A span with any
other `gen_ai.operation.name` (`execute_tool`, `invoke_agent`, `embeddings`,
…) is absorbed: it never becomes a generation of its own. An OpenRouter
root nested under an application's `chat` span stays an `openrouter`
generation, and the `chat` span stays a `semconv` one. Among orphan log
records (see Logs), `semconv` claims those whose event name is
`gen_ai.client.inference.operation.details` or one of the legacy per-role
events (`gen_ai.system.message`, `gen_ai.user.message`,
`gen_ai.assistant.message`, `gen_ai.tool.message`, `gen_ai.choice`), and
groups them with the default `(traceId, spanId)` grouping.

**Lookup order.** For every key: the span's attributes, then the attributes
of a span event named `gen_ai.client.inference.operation.details` (span
events have no body), then, for each of the unit's log records with that
event name in unit order, the record's attributes and then its `body`
(a kvlist, read as an object; any other body is skipped). An orphan unit
has only the record layers. The first present value wins. Content arrives
either as a JSON string or as a structured `AnyValue`; both decode to the
same JSON. A content string that is not valid JSON makes the unit
`truncated`.

**Known limitation.** That rule covers `gen_ai.system_instructions` too: an
emitter that sends the system prompt as plain text (not a JSON array of
parts) has the whole generation skipped as `truncated`. Every pinned
instrumentation sends JSON parts; relaxing the rule for plain-text system
instructions is an open point.

| Neutral field | Source |
|---|---|
| id | `gen_ai.response.id`, else `span-<spanId>`; for an orphan unit, `log-<traceId>-<spanId>` from its first record (see Logs) |
| session | `gen_ai.conversation.id`, else `session.id` (usually absent: see Sessions and ids) |
| client key | resource `service.name` |
| messages | `gen_ai.system_instructions` (text parts joined with `\n`) as a leading system message, then `gen_ai.input.messages`; the v1.36 per-role events (`gen_ai.system.message`, `gen_ai.user.message`, `gen_ai.assistant.message`, `gen_ai.tool.message`), as span events or log records (see Logs), when neither attribute is present |
| completion | the first `gen_ai.output.messages` element, or, when that attribute is absent, the `gen_ai.choice` event (span event or log record) with the lowest `index` present (an event without an index sorts last); the others are kept in `choices` |
| finish reason | `gen_ai.response.finish_reasons[0]`, else the completion message's `finish_reason`, else the legacy choice's `finish_reason` |
| models, provider | `gen_ai.request.model`, `gen_ai.response.model`; `gen_ai.provider.name`, else `gen_ai.system` |
| continuation | `gen_ai.request.previous_response.id` → the request is a delta on that generation |
| compacted | `gen_ai.conversation.compacted = true`, or a `compaction` part in the prompt |
| tool results (fallback) | an absorbed `execute_tool` span in the same trace whose `gen_ai.tool.call.id` matches a completion call and that starts at or after the generation (and before a later generation of the trace that reuses the id; a later generation's calls are read from its span and from the trace's semconv log records carrying its span id, so a turn whose content is only in log records counts): `gen_ai.tool.call.result`, error from its status or `error.type`. Span units only: an orphan unit has no span to scope the search by and gets no `execute_tool` results |

The generation's profile metadata (`structural.extra.otel.semconv`):
`scope` (`{name, version}`) always, and each other key only when present:
`operation`,
`request_params` (temperature, top_p, top_k, max_tokens, seed,
stop_sequences, penalties, reasoning level, choice count, stream, output
type), `server_address`, `time_to_first_chunk_s`, `finish_reasons`,
`choices`, `tools_digest`, `usage_raw`, `output_parts`.

**Message parts.**

| Part | Becomes |
|---|---|
| `text` | text; a message of only text parts gets string content joined with `\n` |
| `reasoning` | on the completion: `reasoning` text plus the parts kept verbatim in `reasoning_details`; in history: kept in the message's `reasoning_details`. Never part of a turn id. |
| `tool_call` | a tool call; a missing id reads as `""` |
| `tool_call_response` | one tool message per part, in order; a missing id reads as `""` |
| `blob`, `file`, `uri`, `compaction`, server tool parts, anything else | in history: kept verbatim in the message, rendered as `[<type>]` in the turn key; on the completion: `[<type>]` is written into the step's text and the parts are kept verbatim in `output_parts` |

A message that mixes text and `tool_call_response` parts is split in source
part order: each run of `tool_call_response` parts becomes one tool message
per part, and each run of other parts one message with the source role, at
its position. Results that come before the text pair with their calls. Text
that comes before them becomes a user turn between the call and its
results: id-bearing results still pair, but id-less results after it attach
to nothing, because the user turn ends the id-less pairing.

**Usage keys** (current spelling first): input `gen_ai.usage.input_tokens` |
`prompt_tokens`; output `output_tokens` | `completion_tokens`; cache read
`cache_read.input_tokens` | `cache_read_input_tokens`; cache write
`cache_write.input_tokens` | `cache_creation_input_tokens`; reasoning
`reasoning.output_tokens` (clamped to the output count). Every
`gen_ai.usage.*` key is also kept as received under `usage_raw`. There is no
cost.

## Profile `openinference` (minimal, explicit only)

Consulted only under `ProfileSelection::OpenInference`. A span with
`openinference.span.kind = LLM` is a generation; any other kind is absorbed.
Ids are `span-<spanId>` (no response id is emitted); the session is
`session.id`. Messages come from `llm.input_messages.<i>.message.*`. A
message without `message.content` takes its content from the
`message.contents.<k>.message_content.{type, text, image.image.url}` list:
a string (parts joined with `\n`) when every part is text, else an
OpenAI-style parts list (`{type: "text", text}`, `{type: "image_url",
image_url: {url}}`, other types verbatim), the shape `semconv` gives;
the completion's text is its text parts joined. Tool
calls from `….message.tool_calls.<j>.tool_call.{id, function.name,
function.arguments}`, tool results from `….message.tool_call_id`; the
completion is the lowest `llm.output_messages.<i>` index present, other
indices go to `choices`.
Indices sort numerically (`10` after `9`). Models: `llm.request.model_name`
/ `llm.response.model_name`, else `llm.model_name`; provider `llm.system`.
Usage: `llm.token_count.{prompt, completion, total,
prompt_details.cache_read, prompt_details.cache_write,
completion_details.reasoning}`; the prompt count is inclusive. A value
of `__REDACTED__`, or no `llm.input_messages.*` family at all, makes that
side absent.

## Logs

A request's `resourceLogs` are read alongside every request's
`resourceSpans`, so logs and spans correlate across requests in any order.

- **Event name:** `eventName`, else the `event.name` string attribute
  (the log data model's older spelling; which one each emitter uses is
  recorded per capture under Event-mode captures).
- **Correlation:** a record belongs to the span its `(traceId, spanId)`
  names, ids lowercased on read like every other. Every span of
  the input is indexed before any record is placed, so a `logs.json` that
  sorts before `traces.json` still correlates.
- **Where a record goes:**
  - to the unit of a claimed span (`Unit.logs`), in
    `(timeUnixNano, input order)`; a span delivered twice is two units and
    both get the record (the generation-id dedupe keeps one);
  - nowhere when its span was absorbed: never a unit's own content and
    never an orphan;
  - otherwise it is an *orphan*: its span was unclaimed, no span in the
    input has its ids, or it has no `spanId`.
  Whatever it correlates to, every record with a trace id is also listed
  by `TraceView::all_logs()`, the whole trace's distinct records in
  `(timeUnixNano, input order)`; a record with no trace id is in no
  trace. `semconv` uses it to read a later generation's tool calls when
  they exist only in log records.
- **Orphans:** offered to the consulted profiles in order; the first whose
  `claims_log` accepts a record takes it, and groups its records with its
  `group_logs` (default: by `(traceId, spanId)`, first-seen order; a record with neither id forms a
  group of its own). Each
  non-empty group is one unit with no span, taking its resource and scope
  from its first record. Orphan units are processed after every span
  unit, in the input order of each group's earliest record, through the
  same steps as a span unit except the status gate. Records no profile
  accepts count as `unclaimed`, as unclaimed spans do.
- **Record dedupe (first in input order wins):** the key is the
  `(traceId, spanId)`, `timeUnixNano` (0 when absent), the
  event name, and the sha256 of the JCS form of the body and of the
  attributes (a list of `[key, value]` pairs in delivered order). Body and
  attribute values are first converted to plain JSON
  (`otlp::any_value_to_json`), so the same record in two encodings
  (`"intValue":"5"`, `"doubleValue":1.0` and `"intValue":5`,
  `"doubleValue":1`) has one key. The conversion also merges records
  that differ only in ways it does not keep: a `bytesValue` and a
  `stringValue` with the same text (bytes stay their base64 string); a
  kvlist that repeats a key (the last value is kept, so two records that
  differ only in a shadowed value share a key); and wrong-typed values,
  which all become `null` (an unreadable `intValue`; an `AnyValue` that is
  not an object or holds no known field). A `doubleValue` given as a
  string other than `"NaN"`/`"Infinity"`/`"-Infinity"`, such as `"1.5"`,
  also becomes `null` today. That is a known limitation of the JSON
  reader, not a rule: proto3 JSON allows a double as a string. Span units run before orphan units, so a
  duplicate generation
  that straddles the two is kept by the span unit and the orphan copy is
  counted as `duplicate`.

**`semconv` over logs.** The lookup order (see Profile `semconv`) reads a
correlated or grouped `gen_ai.client.inference.operation.details` record's
attributes and then its kvlist `body` after the span and its details span
event. Legacy per-role events are read from log records as well as span
events; a legacy record's content is its kvlist `body`, or its attributes
when the body is not a kvlist. When any legacy event of a unit arrives as
a log record, the unit's legacy events are read only from its log records
and its legacy span events are ignored ("log records win" per unit, not
per event name: a span event whose name has no log-record copy is not
read either). Otherwise they come from the span's events.

**Decisions where the spec is silent.**

1. An orphan unit's trace id is its first record's `traceId`.
   Its start and end are the minimum and maximum over its records of
   `timeUnixNano`, falling back to `observedTimeUnixNano` when
   `timeUnixNano` is 0 or absent. A record with neither time is left out
   of the minimum and maximum (it does not pull the start to 0); both are
   0 only when no record of the unit has a time.
2. `semconv` identifies an orphan generation without a
   `gen_ai.response.id` as `log-<traceId>-<spanId>` (lowercased on read,
   so the same record in any encoding gets one id), taken from
   the unit's first record. It applies only when the unit has no span and
   that record carries a trace id or a span id; otherwise the unit has no
   generation id and is skipped as `missing-payload`.
3. A legacy `gen_ai.assistant.message` (or `gen_ai.choice` message) tool
   call in the flat shape `{"id","type","name","arguments"}`, as
   openai-v2 emits it, is rewritten to the v1.36 nested shape
   `{"id","type","function":{"name","arguments"}}` before it is read, for
   span events and log records alike; both shapes read the same.

An orphan unit also gets no `execute_tool` results: there is no span to
scope the search by (see the tool results row under Profile `semconv`).

## Tokens

Token classes are **additive**, as `toolpath-claude` emits them:
`input_tokens` excludes cache reads and writes, and `input_tokens +
cache_read_tokens + cache_write_tokens` is the whole prompt. Every profile
applies this, whatever its source reported (see Usage basis below). The
producing assistant step's `token_usage` has `input_tokens` (exclusive),
`output_tokens`, `cache_read_tokens` and `cache_write_tokens`; OpenRouter's
keys are `input_tokens.cached` and `input_tokens.cache_write` (the
`semconv` and `openinference` keys are in their sections). OpenRouter's
`input_tokens.cache_write_5m` / `…_1h` split of the writes stays in
`extra.otel.usage` only. Reasoning tokens (clamped to at most the output
count) go to `breakdowns["output"]["reasoning"]`, informational and never
summed; a generation that reports no output count has no breakdown, and
its reasoning count stays in `extra.otel.usage`. Each count is the generation's own (no cumulative counters); a
class the source does not report is absent, never zero-filled; a
generation whose four classes are all zero or absent has no
`token_usage`.

Every generation's `token_usage` is on exactly one step, so summing
`token_usage` over a path's steps gives the session total and agrees with
`cost_usd`. A generation whose completion is already a turn (an identical
retry: same prompt, same completion) produces no turn of its own; it gets
an **unplaced step** instead (see Retention), which carries its
`token_usage`.

The kind (`agent-coding-session/v1.1.0`) does not say whether
`input_tokens` includes cache, and producers differ (`toolpath-codex`
passes Codex's inclusive count through). The kind is immutable, so this
clarification lives here and in the RFC, not in the kind's spec.

## Usage basis (`cache_basis`)

Whether a source's input count already includes cache reads and writes
depends on the *emitter*, not the model provider (a gateway reports
`gen_ai.provider.name = anthropic` with inclusive counts). Each
generation's `extra.otel.usage.cache_basis` records what the source
reported: `inclusive` (its input count included cache reads and writes,
which were subtracted, saturating at 0) or `exclusive` (its input count
excluded them and passes through). The basis is never folded into
`input_tokens`. OpenRouter is inclusive (an M0 generation reports input
48894 with 47889 cache writes and an `input_cost` of 1005 × $1/M + 47889 ×
$1.25/M, so 1005 is the uncached input). `openinference` is inclusive.
`semconv` decides by instrumentation scope name through the table
`EXCLUSIVE_INPUT_SCOPES`. The table is **empty**: the official Anthropic
instrumentation (`opentelemetry-instrumentation-genai-anthropic` 1.2b0,
scope `opentelemetry.instrumentation.genai.anthropic`) already adds cache
reads and writes into `gen_ai.usage.input_tokens`, so every semconv count
is inclusive; the Anthropic capture test (`src/tests/captures.rs`) pins
this.

## Sessions and ids

- **Sessions** are the caller's: every generation read from one call's
  requests belongs to one path, in `(start time, generation id)` order.
  Every generation that carries a client session id (`session.id`; for
  `semconv` also `gen_ai.conversation.id`) must carry the same one, else
  the call is `OtelError::MixedSessions`; generations without one belong
  to the session as given. `derive_graph` applies this to each slice. The
  session key is that id. A session with none (older pi releases) is
  keyed by its first generation: a
  full-history request gives `otel-cluster:<16 hex>` (a hash of the client
  key, the leading system message, the first user message and the
  generation id), a delta request `otel-trace:<16 hex>` (a hash of the
  client key and trace id). Both read only the first generation, so the
  key holds as the session grows. The generation id keeps two sessions
  that open with the same system and user message (one client, a "hi" or
  a slash command) from sharing a key, and with it a path id, derived
  session id and turn ids.
- **Turn ids** are chained content hashes: the root is
  `sha256("toolpath-otel/v1\0" ‖ session_key)`, and each turn's id is
  `sha256(previous id ‖ canonical(normalized message))`, 16 lowercase hex.
  `canonical` is RFC 8785 (JCS), the one canonical form every content
  hash and digest in the crate uses (`hash::canonical_json`; the same
  `serde_json_canonicalizer` that `path-cli` uses for content-addressed
  session ids). The normalized message is
  `{role, text, calls?, tool_call_id?, is_error?}` (`calls` as
  `[[id, name], …]`; absent keys when empty, `null` or `false`). A call
  with no id is `["", name, arguments]` instead, `arguments` being the JCS
  text of its parsed arguments: nothing else tells two id-less calls
  apart, and parsing first keeps a history echo's formatting from
  forking. Ids are JCS-based from 0.1.0.
  Identical prefixes collapse into one branch; a retry or a compaction
  forks; a retry with an identical completion adds no turn, only an
  unplaced step. Ids and step payloads stay the same as a session grows.
- **Unplaced step ids** are `<completion turn id>~<generation id>`. A turn
  id is hex, so the two never collide.
- **Derived session id**: a UUID (version 8) from
  `sha256("toolpath-otel/session\0" ‖ session_key)`, never the harness's own
  id, so the otel view and a harness import of the same session coexist.

| Identifier | otel document |
|---|---|
| `meta.source` | `otel` |
| `meta.extra.producer.name` | the inferred harness (`claude-code`, `codex`, `opencode`, `pi`), else `otel` |
| Path id | `path-otel-<8>` |
| Graph id | `graph-otel-<8>` (the first path's), or `graph-otel-empty` |
| Conversation artifact | `otel://<derived id>` |
| Actors | `agent:<routed model>`, `human:user`, `tool:otel` |

The harness session id is kept as data (`meta.extra.otel.session_id`).

## Continuations and delta requests

A request that states only what is new since an earlier response (OpenAI
Responses `previous_response_id`, carried as
`gen_ai.request.previous_response.id`) chains from that response's
completion turn instead of from the root, so its turns hang off the
conversation they continue. If the continued generation is not in the
session (not captured, or skipped as `truncated` or `error-status`), the
request chains from the root and the id is listed in
`meta.otel.missing_continuations`. The tree such a request starts stays on
the main line (it is never a side request), so `path.head` can reach it.

## Skeletons

When content was not captured (content capture is opt-in in every official
instrumentation, and OpenRouter Privacy Mode withholds it) the missing side
is **absent**. The generation still yields turns: a prompt-absent request
chains from the previous generation of its session; a completion-absent one
contributes an assistant turn with empty text. Those turns carry
`extra.otel.absent = {prompt: true}`, `{completion: true}` or both.
Usage, models and actors are kept; tool calls are not, since a
metadata-only semconv span carries no calls. A skeleton inside an
otherwise-captured session chains from the previous completion, and the
next fully captured request restates a history the skeleton never saw, so
the skeleton's branch usually ends off the head's ancestry. Every step
whose content comes from a skeleton generation carries
`extra.otel.branch = "skeleton"` (unless it is a sub-agent's or a side
request's, which carry those marks), on the main line or off it, so a
query can tell an off-line skeleton from an abandoned attempt.

## Sub-agents, side requests and the head

A harness sends its sub-agents' and side requests' model calls under the
session's id, so they land in the session's turn DAG as their own
branches. A *thread* is everything below a first user message.

- **Sub-agent.** A thread whose first user message's text ends with the
  `prompt` argument of a delegation call (`Task`, `Agent`, `task`) that
  comes before it in the turn DAG is that call's sub-agent. Threads are
  matched in order, each to the earliest call before it that no earlier
  thread took, so with duplicate prompts the first thread takes the first
  call. Every delegation call with a prompt is
  listed in its turn's `delegations` (`{agent_id: <call id>, prompt,
  result?}`, `turns` empty, as `toolpath-claude` does), built from the
  call and its result alone, whether or not a sub-agent thread matches
  it. Every step of a matched thread carries
  `extra.otel.branch = "subagent"` and `extra.otel.delegation = <call
  id>`. The sub-agent's turns stay steps of the path. Its answer (a
  final assistant turn whose text the call's result carries, or a later
  user turn of the delegating thread carries, e.g. a background agent's
  task notification; an assistant turn that merely repeats the text is
  not one) becomes an **extra parent** of the delegating thread's
  turn that receives it: the first such turn after the answer, for a
  result, or the turn carrying the text. A sub-agent that answers more
  than once (resumed after its first answer) merges at the turn that
  receives an answer first (of the answers that turn receives, the
  earliest); a later answer never moves the merge. The sub-agent's steps
  are then ancestors of the head, not dead ends. A sub-agent whose answer
  never comes back in the capture keeps its steps off the head's
  ancestry, still marked `subagent`. A system turn (the shared sub-agent
  system prompt) takes its first thread's mark when that thread is a
  sub-agent's; later threads never change it. So the shared turn names
  only the first call, and a later unmatched thread under it is `side`
  below a `subagent` system turn.
- **Side request.** A thread under a different leading system message than
  the main line's (titles, classifiers) carries `extra.otel.branch =
  "side"`; it is a dead end by construction. A tree started by a request
  whose continuation target is missing is not one.
- **Main line and head.** The main line is the first leading system
  message, in start order, to produce two turns outside sub-agent threads,
  so a title or quota request that starts first does not take it. A tree
  started by a request whose continuation target is missing takes part in
  that choice like any other tree, so a capture that starts mid-session
  (the exporter enabled late, or the first requests dropped) keeps its
  conversation as the main line and the side requests around it are
  `side`. Such a tree is never `side` itself, and one that starts after
  the main line is decided leaves that choice as it was. Until
  some tree has produced two turns, the main line is undecided and no turn
  is marked `side`.
  `path.head` is the last unmarked turn in view order, whatever came later
  in time. A retry or
  a compaction on the main line still forks, and the abandoned branch is
  an unmarked dead end.
- **Unplaced step.** A generation whose completion is already a turn
  (an identical retry) gets a step of its own, `branch = "unplaced"`: an
  assistant step with empty `text`, the generation's `thinking`,
  `stop_reason`, actor and `token_usage`, and its per-generation keys (see
  Retention). Its parent is the parent of the completion turn it repeated,
  so it is a sibling of that turn: another answer to the same prompt, as a
  retry with a different completion would be. Nothing descends from it, so
  it is always a dead end; it is not a turn, so the head, the main line and
  the other marks never consider it.
- **Marks never move.** Every mark depends only on turns that come
  before it (in start order, or in feed order for incremental sends) or on
  the step's own data: a thread is matched only to a call before it and
  no earlier thread took, a merge is the first answer received, the main
  line is decided once, and `delegations` read only the delegating turn.
  Appending generations therefore never changes a mark a turn already
  has; it can only add marks and extra parents to new turns. One
  qualification: turns left unmarked while the main line is undecided
  become `side` once it is decided if they are off it.

`toolpath::v1::query::dead_ends` (and `path query`'s `.dead_end`) then
report only side requests, skeletons, unreturned sub-agents, unplaced
steps and real abandoned attempts; select on `.change[].structural.otel.branch` to tell
them apart.

## Tool call ids and results

A tool call with no id (Gemini calls carry none on the wire) is given the
positional id `<turn id>:<index>`, its position among the message's calls.
Tool results with no id pair with the most recent turn's id-less calls in
order; surplus results attach to nothing. The ids are derived from turn ids,
so they are the same on every import. Results come from the prompts first:
a call's result is the tool message in the first later request that carries
it. Only when no request in the capture carries it (the capture ends
mid-tool) is the `execute_tool` span's result used, and a later import of a
longer capture replaces it. That span is picked by time as well as call id
because instrumentations synthesize call ids that repeat across turns (e.g.
`read_file_0`).
Results are keyed by call id and each message keeps its calls in order, but
the relative source order of results for different calls is not recorded.

## Retention

The document keeps enough to rebuild each generation's messages and
completion exactly, except the deliberate drops below. The completion's
reasoning text is kept as the producing step's `thinking` and its
`reasoning_details` under the step's `structural.extra.otel`;
`reasoning_details` that the history echoes back are kept in `echo`. A
delta request's prompt is rebuilt by walking up the first-parent chain from
its completion step and stopping at the continued generation's completion
step: the prompt opens with the results for that generation's calls,
followed by the turns below it, and re-chains from that step to the
recorded `prompt_tip`. An unplaced generation's completion step is the
turn its unplaced step names in `completion`. A skeleton generation (one with an `absent` side)
has nothing to rebuild and is skipped, but a delta that continues from a
skeleton still rebuilds from the skeleton's completion. The round trip is
tested over the M0 OpenRouter fixtures (`src/tests/derive.rs`), their semconv
re-encodings (`test-fixtures/otel/equivalence/`), the span-content
captures (the Responses capture through its continuation copy, so deltas
are exercised) and inline delta and skeleton sessions (`src/tests/equivalence.rs`,
oracle in `src/tests/common/retention.rs`). Sources with id-less tool calls are
outside it: their positional ids never equal the source's `""`.

`extra` is serde-flattened, so in the JSON the keys written below as
`meta.extra.otel` and `structural.extra.otel` appear as `meta.otel` and
`structural.otel`.

- **Path meta** `meta.extra.otel` (may change as the session grows):
  `profile` (the one profile that read the session; `"mixed"` plus a
  `profiles` list when several did; absent when none), `harness`,
  `missing_continuations` (when any), `session_id`, `derived_session_id`, `session_key`,
  `request_session_id`, `user_id`, `client_key`, `generation_ids`
  (ordered), `trace_ids`, `cost_usd {total, by_model, priced_generations,
  generations}` (when any generation is priced; `total`, or a model's
  entry in `by_model`, is `null` when any generation it covers is unpriced,
  since an unknown price is never $0),
  `providers`, `truncated`; profile session data
  under `meta.extra.otel.openrouter` (`creator_user_id`, `entity_id`).
- **Per step**, under the conversation change's `structural.extra.otel`:
  every turn has `content_hash`, `first_generation_id`, `message_role`, and
  `parts` (non-text parts) when any. An assistant step with a producing
  generation adds `generation_id`, `trace_id`, `prompt_tip`,
  `request_model`, `provider`, `usage`, `cost`, `dropped` (the set-aside
  system messages as `{index, role, content_hash}`), `continues` (a delta
  request's continued generation id), `compacted: true`, `reasoning_details`
  (the completion's, when any), and the generation's profile metadata under
  `structural.extra.otel.<profile>`; for OpenRouter
  `structural.extra.otel.openrouter` (`api_key_name`,
  `creator_user_id`, `entity_id`, `openrouter_user_id`, `provider_name`,
  `provider_slug`, `upstream_finish_reason`, latencies, `unit_price`,
  `provider_responses`, `request_params` = `rawRequest` minus `messages`,
  `tools`, `input`, and `tools_digest`). An unplaced step (see Sub-agents,
  side requests and the head) carries the same per-generation keys for its
  generation, plus `completion` (the id of the turn whose completion it
  repeated) and `branch = "unplaced"`, and none of the per-turn keys. The
  step of the first generation, in session order, that carries a dropped
  text stores it once in `dropped_content` (`{content_hash: text}`),
  whether that step is a producing or an unplaced step. An assistant step keeps `echo` when
  its history echo's tool arguments differ from the completion's (the
  echoed raw arguments by call id, an id-less call's by its positional
  id; an id-less call's arguments are part of its turn id, so its echo
  is kept only when it differs in form but not in canonical bytes, such
  as `1` echoed as `1.0`, and any other difference forks) or the echo
  carries any
  `reasoning_details` (kept as echoed), even when the arguments match.
  Every turn whose content comes from a skeleton generation carries
  `absent` (`{prompt: true}`, `{completion: true}` or both). A sub-agent's
  step carries `branch = "subagent"` and `delegation`, a side request's
  `branch = "side"`, and any other skeleton turn's `branch = "skeleton"`
  (see Skeletons and Sub-agents, side requests and the head). A generation's
  `cost` is absent when it is unpriced.

## Deliberately dropped

1. Tool-definition bodies (`completion.tools`, `rawRequest.tools`,
   semconv `gen_ai.tool.definitions`); only `tools_digest` (sha256 of their
   JCS form) is kept.
2. Content encoding: string vs. parts list, `cache_control`, a tool
   message's `name`, a message's `name`.
3. The vendor child spans (`generation`, `provider attempt`); their content
   is duplicated by `provider_responses`. Absorbed semconv spans
   (`execute_tool`, `invoke_agent`, `embeddings`, …), except an
   `execute_tool` result used as the fallback above.
4. The JSON formatting of tool-call arguments (parsed and compared as
   values; `echo` keeps the history's raw form where it differs).

## Pinned instrumentation behavior (captures)

The committed captures under `test-fixtures/otel/semconv/` and
`test-fixtures/otel/openinference/` were recorded against local mock servers
by the maintainer capture harness in `scripts/otel-fixtures/` (its README
holds the versions, the capture and verify commands, and the hygiene
rules). Each capture directory
holds `traces.json` plus `manifest.json` and `expected.json`; each `event/`
capture also holds `logs.json` (see Event-mode captures). The raw protobuf
request bodies the capture harness keeps (`traces.binpb`, `logs.binpb`) are
not committed with this crate: it reads JSON only.

The harness keeps **two hash-pinned locks**.
`opentelemetry-instrumentation-openai-v2` 2.4b0 (the newest release) needs
`opentelemetry-util-genai` 1.1b0 or older, while genai-openai,
genai-anthropic and google-genai 1.2b0 need util-genai 1.2b0 or newer. So
the `openai-chat` scenario captures in its own venv (`.venv-openai-v2`,
`requirements-openai-v2.txt`, util-genai 1.1b0) and every other scenario
uses the main lock (`.venv`, `requirements.txt`); each capture's
`manifest.json` lists the packages of the lock it used. The two collapse
back into one lock once an openai-v2 release works with util-genai 1.2b0.

At the pinned versions:

- `opentelemetry-instrumentation-openai-v2` 2.4b0 does not instrument
  `responses.create`; the Responses capture uses
  `opentelemetry-instrumentation-genai-openai` 1.2b0, which does not set
  `gen_ai.request.previous_response.id`. Without it each Responses request
  stands alone; the continuation mapping is tested on a copy of the capture
  labeled SYNTHETIC (`semconv/openai-responses/span-continuation/`), which
  sets the attribute to the id the client really sent. It is
  replaced once a pinned instrumentation emits the attribute.
- `opentelemetry-instrumentation-genai-openai` 1.2b0 does not emit
  `gen_ai.usage.reasoning.output_tokens` on Responses spans, although the
  mock served a reasoning count, so the Responses capture has no reasoning
  breakdown. The reasoning-count mapping is tested on the Gemini capture and
  on inline spans.
- `opentelemetry-instrumentation-openai-v2` 2.4b0 on util-genai 1.1b0 (the
  `openai-chat` capture) reports scope `opentelemetry.util.genai.handler`
  1.1b0, not an instrumentation-specific scope; emits no cache-read usage
  key, although the mock served cached tokens; sends the system prompt as a
  `system` message inside `gen_ai.input.messages` rather than
  `gen_ai.system_instructions`; and emits no `gen_ai.tool.definitions`,
  `server.address` or `server.port`.
- `opentelemetry-instrumentation-google-genai` 1.2b0 invents ids
  `<function name>_<part index>` for id-less Gemini calls and responses, so
  the capture pairs by those ids; positional ids apply to emitters that send
  none. It also reports Gemini thought parts as ordinary `text` parts, so a
  thought summary reaches `thinking` only from an emitter that marks it as a
  `reasoning` part.
- `opentelemetry-instrumentation-genai-anthropic` 1.2b0 emits thinking as a
  `reasoning` part without its signature.
- Content capture for the util-genai packages is
  `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` ∈ {`NO_CONTENT`,
  `SPAN_ONLY`, `EVENT_ONLY`, `SPAN_AND_EVENT`}; OpenInference captures by
  default.

## Event-mode captures

`scripts/otel-fixtures/capture.py <scenario> --mode event` records a
scenario's instrumentation with message content on log records instead of
span attributes (`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=EVENT_ONLY`
with `OTEL_INSTRUMENTATION_GENAI_EMIT_EVENT=true` for all four providers),
into `semconv/<provider>/event/`. The SDK exports through the OTLP/HTTP
protobuf exporter to a local sink (`otlp_sink.py`); the committed
`traces.json` and `logs.json` are written from those bodies by Google's
protobuf library (`pb_to_json.py`), not by this crate. Each
`manifest.json` records `mode`, `event_name_source`,
`details_content_location`, `legacy_events` and `otlp_exporter`. What each instrumentation emitted at the pinned versions:

| Capture | Event name in | Details content in | Legacy events seen | OTLP exporter |
|---|---|---|---|---|
| `openai-chat/event` | `eventName` | attributes | none | `opentelemetry-exporter-otlp-proto-http` 1.45.0 (request bodies as exported) |
| `openai-responses/event` | `eventName` | attributes | none | `opentelemetry-exporter-otlp-proto-http` 1.45.0 (request bodies as exported) |
| `anthropic/event` | `eventName` | attributes | none | `opentelemetry-exporter-otlp-proto-http` 1.45.0 (request bodies as exported) |
| `gemini/event` | `eventName` | attributes | none | `opentelemetry-exporter-otlp-proto-http` 1.45.0 (request bodies as exported) |

So at the pinned versions every instrumentation names its records with
`eventName` (not the `event.name` attribute), puts the details content in
the `gen_ai.client.inference.operation.details` record's attributes (not
its body), and emits no legacy per-role events; the body layer and the
legacy-event path are tested on inline records. The event-mode log records
carry only `observedTimeUnixNano`, never `timeUnixNano`. Records within a
unit (and in `TraceView::all_logs`) are ordered by `timeUnixNano`, then
input order, and never by the observed time, so every event-mode record
sorts as time 0 and they keep input order (the exporter writes them in
emission order). The observed time is used only for an orphan unit's
start and end (the fallback under Logs, Decisions 1), where it shares the
rank space with the span times.

Each `event/` capture derives the same path as its `span/` capture on the
cross-profile comparison set (`tests/common/equivalence.rs`; the two
differ only in step timestamps, which the set leaves out;
`crates/toolpath-otel/src/tests/captures_event.rs`). The `openinference`
capture has no event mode.
