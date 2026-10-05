---
layout: base.njk
title: "Kind: agent-coding-session v1.2.0"
permalink: /kinds/agent-coding-session/v1.2.0/
---

# Kind: `agent-coding-session` v1.2.0

<dl class="kind-meta">
  <dt>URI</dt>
  <dd><code>https://toolpath.net/kinds/agent-coding-session/v1.2.0</code></dd>
  <dt>Schema</dt>
  <dd><a href="./schema.json"><code>schema.json</code></a></dd>
</dl>

A Toolpath path whose `meta.kind` is this URI records an AI coding conversation. It is an ordinary path with the extra structure described here. `head`-ancestry, dead ends, signatures, and `base` all behave as in the [base format](/format/).

Every such path comes from one place: the shared `ConversationView → Path` derivation in `toolpath-convo` (`derive_path`), which the provider crates (`toolpath-claude`, `toolpath-gemini`, `toolpath-codex`, `toolpath-copilot`, `toolpath-opencode`, `toolpath-cursor`, `toolpath-pi`) all call. The field shapes below are therefore exact. The only producer-specific parts are the contents of a tool's `input`, the diff text in a change's `raw`, and the value (not the meaning) of `group_id`.

Constraints apply by structural `type`, not by artifact key: a `change` entry is checked only when its `structural.type` is one named here, and extra properties never make a path invalid. [`schema.json`](./schema.json) encodes the rules; apply it alongside the base schema. The URI is immutable. Later revisions ship under a new version URI.

**Changed from [v1.1.0](/kinds/agent-coding-session/v1.1.0/):**

- the token classes are additive: `input_tokens` excludes cached prompt tokens, so the four classes never overlap (see [Additive classes](#additive-classes));
- a group's total sits on the group's last step wherever its steps fall, not on the last step of each run of consecutive steps (see [Group accounting](#group-accounting));
- a message's total includes every billed part of it on its own model, including parts the source's top-level total leaves out;
- counts are non-negative, `breakdowns` keys are the four classes, and `attributed_token_usage` may carry `breakdowns`.

The schema accepts the same shapes as v1.1.0's apart from negative counts and its own `meta.kind` URI. The new version exists so consumers can price the classes separately and sum steps without double-counting cached input (Codex and Gemini CLI documents under v1.1.0) or interleaved groups (Claude Code documents under v1.1.0).

## The turn payload

One entry in a turn's `change` map has `structural.type` of `"conversation.append"`. Find it by that type: the artifact key is producer-specific, formed as `<source>://<conversation-id>` from the harness in `meta.source` (e.g. `claude-code://…`, `gemini-cli://…`, `codex://…`, `opencode://…`, `cursor://…`, `pi://…`).

Its `structural` object always carries:

| Field  | Type   | Meaning                                                            |
| ------ | ------ | ------------------------------------------------------------------ |
| `type` | string | the literal `"conversation.append"`                                |
| `role` | string | `"user"`, `"assistant"`, `"system"`, or a producer-specific string |
| `text` | string | the visible prose; present even when empty (`""`)                  |

It may also carry any of the following, present only when the turn has them:

| Field                    | Type   | Meaning                                                              |
| ------------------------ | ------ | -------------------------------------------------------------------- |
| `thinking`               | string | the model's reasoning text                                           |
| `group_id`               | string | groups the steps derived from one source accounting unit (see below) |
| `tool_uses`              | array  | tools the agent invoked (shape below)                                |
| `token_usage`            | object | the group's token counts (shape and rule below)                      |
| `attributed_token_usage` | object | this step's own attributed spend, when known (see below)             |
| `stop_reason`            | string | why the model stopped (`end_turn`, `tool_use`, …)                    |
| `delegations`            | array  | sub-agent work spawned from this turn (shape below)                  |
| `environment`            | object | working environment at this turn (shape below)                       |

The model identifier is not on the change. It lives in `step.actor` (`agent:<model>`) and `meta.actors`. There is no provider-specific blob: every field the derivation captures is one of those listed above.

### `group_id`

The provider's identifier for the **source accounting unit** these steps were derived from — Claude Code's `message.id` (`msg_…`, or the entry's `requestId` for an assistant message with no id) for one split message, Codex's round `turn_id` for one round (which may itself contain several messages). It is a **grouping key, not a step identifier**: when a producer derives several steps from one accounting unit (Claude Code writes one JSONL line per content block; a Codex round emits a commentary turn plus a final turn), every sibling step carries the same `group_id`. A step without a `group_id` is its own group of one. The stored value is the provider's verbatim id; only its _meaning_ (which unit it names) is provider-specific.

### Group accounting

How `token_usage` on steps relates to the source's accounting units:

1. `token_usage` records a group's spend — a **per-group amount, never a cumulative session counter**.
2. Among the steps sharing a `group_id`, the **last in document order carries the group's total `token_usage`**, converted to the [classes below](#additive-classes); the group's other steps carry none. A group's steps need not be consecutive: when a source interleaves two groups' records, each group still has exactly one total.
3. A step without a `group_id` is its own group and carries its own `token_usage` (when the source records one).

Consequence: **summing `token_usage` over a path's steps yields the session totals.** Consumers need no dedup heuristics. (JSON Schema cannot express the once-per-group rule, so it is normative prose, enforced by producer test suites and checked by `path p validate`.)

`token_usage` has **one meaning everywhere it appears: the total for a group**. A step without a `group_id` is a one-step group, so its `token_usage` is that group's total (which is also its own spend — the two coincide for a group of one). Within a multi-step group, the total sits on the final step. Interpreting a value never requires reading the rest of its group: the key tells you it is a total, and `group_id` on the same payload tells you which group it totals. Per-step spend, when the source has it, rides a separate [`attributed_token_usage`](#per-step-attribution-attributed_token_usage) key — never `token_usage`. When a source itemizes a group's spend (Claude's `usage.iterations`, opencode's per-part `step-finish` tokens), `token_usage` carries the group total and the items do not ride `token_usage`. The total covers every billed part of the group on the group's own model, including parts the source's top-level total leaves out: for an Anthropic message it is the sum of the `usage.iterations` entries billed on the message's model, compaction iterations included, which the top-level `usage` excludes. Tokens billed on a different model (an advisor or fallback iteration) are outside the four classes, because adding them to the message's own counts would mix two models' prices. An iteration that names no model belongs to the message's model. Where a message's usage repeats across records, the producer takes its final usage (the field-wise maximum) and then sums that usage's iterations. Per-request fees, such as server tool use, are outside `token_usage` too.

### Per-step attribution: `attributed_token_usage`

Some sources expose, per step, the spend attributable to that step alone — distinct from the group total. Where a producer has it, the step carries an **`attributed_token_usage`** object (same shape as [`token_usage`](#token_usage)) holding _this step's own share_. It is **optional and orthogonal to `token_usage`**: whether a number is a group total or a step share is structural — the key it sits under — never positional. This is the rule that lets per-step accounting be added by any producer at any time without a new kind version.

How it relates to the group total:

- Within a `group_id` group, `Σ attributed_token_usage` over the group's steps is the group's attributed spend. The **unattributed remainder** — anything the source could not pin to a step — is _computed_ by a consumer as `group's token_usage − Σ group's attributed_token_usage`; it is never recorded, so stored values stay source observations (converted to the additive classes) and source inconsistencies stay visible.
- For a group where the source attributes everything (e.g. Codex, where each step is one API call and the per-call delta is reported directly), the remainder is zero and `Σ attributed_token_usage == token_usage`.
- A group with no per-step data carries no `attributed_token_usage` at all — only the group total. Producers must not fabricate a split.

A producer populates `attributed_token_usage` only when the source genuinely reports per-step spend. Among current producers, **Codex does** (its `token_count` events carry a per-call delta). **Claude does not**: its per-content-block records repeat the message's `usage` (a growing streaming snapshot in older Claude Code versions, the final value since v2.1.132), not per-block costs, so deriving a split from them would be fabrication — Claude-derived steps carry the group total only.

`Σ token_usage` over a path's steps is unaffected by `attributed_token_usage` (they are separate keys), so the session-total guarantee above always holds. A consumer wanting per-step cost reads `attributed_token_usage` where present and falls back to the group total otherwise.

### `tool_uses`

Each element is an object:

| Field      | Type           | Notes                                                                                                                              |
| ---------- | -------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `id`       | string         | provider-assigned invocation ID                                                                                                    |
| `name`     | string         | provider tool name (`Read`, `Bash`, `edit`, …)                                                                                     |
| `input`    | any            | tool arguments; shape is producer-specific                                                                                         |
| `category` | string \| null | Toolpath's classification: `file_read`, `file_write`, `file_search`, `shell`, `network`, `delegation`, or `null` when unrecognized |
| `result`   | object         | `{ "content": string, "is_error": boolean }`, when the result landed in the same turn                                              |

`id`, `name`, `input`, and `category` are always present (`category` may be `null`); `result` is optional.

### `token_usage`

| Field                | Type            | Counts                                                       | Presence                              |
| -------------------- | --------------- | ------------------------------------------------------------ | ------------------------------------- |
| `input_tokens`       | integer \| null | prompt tokens neither read from nor written to a cache       | always present                        |
| `output_tokens`      | integer \| null | generated tokens, reasoning included                         | always present                        |
| `cache_read_tokens`  | integer         | prompt tokens read from a cache                              | only when the source records it       |
| `cache_write_tokens` | integer         | prompt tokens written to a cache                             | only when the source records it       |
| `breakdowns`         | object          | how a class above divides; never a further count (see below) | only when the source itemizes a class |

Every count is a non-negative integer. `null` means the source did not report that class, not zero, so a breakdown of an unreported class may hold only zeros. Values follow the [group accounting](#group-accounting) rule above.

### Additive classes

The four counts are **disjoint**: each token the model processed is counted in exactly one of them. In particular, **`input_tokens` excludes cached prompt tokens**: those are in `cache_read_tokens` or `cache_write_tokens` and nowhere else. So:

- the prompt is `input_tokens + cache_read_tokens + cache_write_tokens`;
- the whole spend on the message's model is that plus `output_tokens`;
- a consumer prices each class at the rate of the model that produced it and adds the results, with no overlap to subtract.

This is Anthropic's usage convention. Some sources use OpenAI's instead, where the input count includes cache reads and, where the source reports them, cache writes; producers convert at derivation, clamping a converted count at zero. Where a source's inclusive convention is a recommendation rather than a guarantee (the OpenTelemetry GenAI semantic conventions, OpenInference), an input count smaller than the source's cache counts is read as already exclusive.

| Source      | Wire fields                                                                                                                   | Additive `input_tokens`                                                     | `cache_read_tokens`       | `cache_write_tokens`          |
| ----------- | ----------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- | ------------------------- | ----------------------------- |
| Claude Code | `input_tokens` (excludes cache), `cache_read_input_tokens`, `cache_creation_input_tokens`                                     | `input_tokens`                                                              | `cache_read_input_tokens` | `cache_creation_input_tokens` |
| Codex       | `input_tokens` (includes cache reads, and from Codex 0.145.0 cache writes), `cached_input_tokens`, `cache_write_input_tokens` | `input_tokens − cached_input_tokens − cache_write_input_tokens`             | `cached_input_tokens`     | `cache_write_input_tokens`    |
| Gemini CLI  | `input` (includes cache), `cached`, `tool` (tool-use prompt tokens, outside `input`)                                          | `input − cached + tool`                                                     | `cached`                  | —                             |
| opencode    | `input`, `cache.read`, `cache.write`                                                                                          | `input` (current opencode; see [Known producer gaps](#known-producer-gaps)) | `cache.read`              | `cache.write`                 |
| pi          | `input`, `cacheRead`, `cacheWrite` (pi normalizes every provider's usage to exclude cache)                                    | `input` (current pi; see [Known producer gaps](#known-producer-gaps))       | `cacheRead`               | `cacheWrite`                  |
| Copilot CLI | `tokenDetails.{input,cache_read,cache_write}`, session totals on `session.shutdown`                                           | `input`                                                                     | `cache_read`              | `cache_write`                 |

Copilot does not document whether `tokenDetails.input` includes cache; the row follows recorded sessions, where `modelMetrics.*.usage.inputTokens` equals `input + cache_read + cache_write`. A projector writing back to a source with inclusive input adds the cached counts back in, so a session round-trips to its original numbers. Gemini CLI writes 0 for a count it lacks, so its documents cannot tell an unreported class from a zero one.

#### Examples

A Claude Code message that read most of its prompt from cache and wrote some of it back. The prompt was `6 + 18183 + 8404 = 26593` tokens:

```json
{
  "input_tokens": 6,
  "output_tokens": 218,
  "cache_read_tokens": 18183,
  "cache_write_tokens": 8404
}
```

A Gemini CLI message whose wire tokens were `{ "input": 9133, "output": 59, "cached": 7498, "thoughts": 10 }`. Gemini's `input` includes `cached`, so `input_tokens` is `9133 − 7498`; Gemini's `output` excludes `thoughts`, so `output_tokens` is `59 + 10`, and the breakdown says how much of it was reasoning:

```json
{
  "input_tokens": 1635,
  "output_tokens": 69,
  "cache_read_tokens": 7498,
  "breakdowns": { "output": { "reasoning": 10 } }
}
```

A Codex 0.145+ round of two API calls. Over the round, the wire's cumulative counters grew by input 300, cached 200, cache write 50 and output 40, so the round's `input_tokens` is `300 − 200 − 50`. Each step's `attributed_token_usage` holds its own call's share, and the shares sum to the total on the round's last step:

```json
[
  {
    "group_id": "turn-2",
    "attributed_token_usage": {
      "input_tokens": 30,
      "output_tokens": 20,
      "cache_read_tokens": 100,
      "cache_write_tokens": 20
    }
  },
  {
    "group_id": "turn-2",
    "token_usage": {
      "input_tokens": 50,
      "output_tokens": 40,
      "cache_read_tokens": 200,
      "cache_write_tokens": 50
    },
    "attributed_token_usage": {
      "input_tokens": 20,
      "output_tokens": 20,
      "cache_read_tokens": 100,
      "cache_write_tokens": 30
    }
  }
]
```

### `breakdowns`

`breakdowns` is an **optional, informational** decomposition of a top-level class into named sub-classes. It is keyed by the class being broken down, and each value is a map of sub-class → tokens. Breakdowns are **never summed into any total**: the parent class already counts these tokens, and a breakdown only says _how_ that class divides. The field is omitted entirely when empty. The same shape and rule apply on `attributed_token_usage`.

| Key           | Parent class         |
| ------------- | -------------------- |
| `input`       | `input_tokens`       |
| `output`      | `output_tokens`      |
| `cache_read`  | `cache_read_tokens`  |
| `cache_write` | `cache_write_tokens` |

Invariant: **`Σ(inner) ≤` the parent class's value**; a breakdown of an absent or `null` class may hold only zeros. The sub-classes need not cover the whole class; the rest is simply not itemized. `path p validate` checks the bound and rejects any other key.

Sub-class names are open, but these carry a fixed meaning:

| Breakdown                      | Meaning                                                                                 |
| ------------------------------ | --------------------------------------------------------------------------------------- |
| `output.reasoning`             | reasoning or thinking tokens, part of the output                                        |
| `input.tool_use`               | prompt tokens from tool use that the source counts outside its prompt (Gemini's `tool`) |
| `cache_write.ttl_5m`, `ttl_1h` | cache writes by lifetime; Anthropic prices the two tiers differently                    |

Valid — reasoning is part of the output:

```json
{
  "input_tokens": 1635,
  "output_tokens": 69,
  "breakdowns": { "output": { "reasoning": 10 } }
}
```

Valid — a class divided into several sub-classes, together within the parent (`450 + 50 ≤ 500`):

```json
{
  "input_tokens": 1200,
  "output_tokens": 500,
  "breakdowns": { "output": { "reasoning": 450, "text": 50 } }
}
```

Invalid — the breakdown exceeds its parent (`450 > 400`). A producer that holds reasoning outside the output count must fold it into `output_tokens` first:

```json
{
  "input_tokens": 1200,
  "output_tokens": 400,
  "breakdowns": { "output": { "reasoning": 450 } }
}
```

Among current producers, Gemini, OpenCode, and Codex record `output → { reasoning }` (their reasoning/thoughts tokens are part of `output_tokens`), and Gemini records `input → { tool_use }`. Claude Code's JSONL reports `output_tokens_details.thinking_tokens`, a subset of output (Anthropic's re-tokenized estimate, present only on a message's final record), and splits `cache_creation` into `ephemeral_5m_input_tokens` and `ephemeral_1h_input_tokens`; the current Claude producer records neither.

### Known producer gaps

The rules above are what a v1.2.0 document means. These producers, as first released with this version, fall short of them for some sessions:

- **Copilot CLI** records input and cache counts only as session totals on `session.shutdown`; its per-call usage is never written to disk. Derived steps carry output tokens alone, so summing a Copilot path's steps undercounts its input and cache.
- **opencode** sessions are read with current opencode semantics, because opencode records no per-message version to convert by. Messages written before opencode 1.3.16 overcount output when the model reasoned (from opencode 1.3.4 every provider's `output` already included `reasoning`; before that, OpenAI-family models'). Messages written by opencode 1.3.4–1.3.5 overcount input for Anthropic and Bedrock models, whose `input` then included cache reads and writes, and messages written before opencode 1.0.62 overcount input for other providers, whose `input` then included cache reads.
- **pi** sessions are read with current pi semantics. Sessions written before pi 0.63.0 (Google and Vertex models) or 0.12.10 (OpenAI models) include cache reads in `input`, and before pi 0.70.0 OpenAI-compatible models double-counted reasoning in `output`.
- **Claude Code** messages are read from their top-level `usage`, so compaction iterations are not yet counted.
- **Codex** forked sessions book the parent session's replayed spend into the fork's first round, and spend after a counter reset (context-window overflow) can be lost.
- **Gemini CLI** sub-agent spend sits only in `delegations[].turns`, on no step, so summing a path's steps undercounts it. Gemini CLI has written sessions as `.jsonl` since 0.39.0, and the Gemini producer reads only `.json` sessions.
- **Cursor** reports a per-bubble `tokenCount` whose meaning is not documented and is usually zero; it is carried as-is.
- **Resumed sessions.** A session that Toolpath wrote into a harness from a v1.1.0 Codex or Gemini CLI document carries that document's overlapping counts, and derives with them.

### `environment`

`{ "working_dir"?: string, "vcs_branch"?: string, "vcs_revision"?: string }`; every field optional.

### `delegations`

Each element is `{ "agent_id": string, "prompt": string, "turns"?: array, "result"?: string }`. `turns` holds the sub-agent's own turns when the producer inlines them.

## File changes

When a turn writes files, its step carries sibling `change` entries keyed by file path, each with `structural.type` of `"file.write"`. The unified diff, when available, is on the change's `raw`, not inside `structural`. The `structural` object holds, all optional:

| Field              | Meaning                                                            |
| ------------------ | ------------------------------------------------------------------ |
| `tool_id`          | the `tool_uses[].id` that produced the mutation, when attributable |
| `tool`             | that tool's `name`                                                 |
| `operation`        | `"add"`, `"update"`, `"delete"`, or a producer-specific tag        |
| `before` / `after` | file contents before / after, when known                           |
| `rename_to`        | the new path, for a rename                                         |

## Non-turn entries

Entries that aren't turns (attachments, preamble lines, snapshots, hook results) become steps with `structural.type` of `"conversation.event"`, carrying `entry_type` and sometimes `event_source_id` plus the producer's event data. They exist so a document round-trips back to the source format. They are not part of the transcript.

## Actors

`step.actor` follows the `type:name` convention, assigned by role:

| Actor             | Turn                                                                                           |
| ----------------- | ---------------------------------------------------------------------------------------------- |
| `human:user`      | a user message                                                                                 |
| `agent:<model>`   | a model reply, named by the recorded model, or `agent:unknown` when none was recorded          |
| `tool:<provider>` | a system turn (session init, system prompt), any other producer role, or a non-turn event step |

`meta.actors` defines each actor the steps reference; `agent:` entries carry `provider` and `model`. A turn's original role is always in its `role` field, so collapsing system and other roles onto `tool:<provider>` loses nothing. Walk steps in `head`-ancestry order for the linear transcript.

## Path metadata

| Field                | Meaning                                                                                               |
| -------------------- | ----------------------------------------------------------------------------------------------------- |
| `meta.kind`          | this URI                                                                                              |
| `meta.source`        | the producing harness: `claude-code`, `gemini-cli`, `codex`, `copilot`, `opencode`, `cursor`, or `pi` |
| `meta.title`         | session title                                                                                         |
| `meta.actors`        | the actor definitions the steps reference                                                             |
| `meta.files_changed` | file paths touched across the session                                                                 |
| `meta.vcs_remote`    | repository URL, when known                                                                            |
| `meta.producer`      | `{ "name": string, "version"?: string }`, the software that produced the session                      |

`files_changed`, `vcs_remote`, and `producer` sit directly under `meta` (they ride `PathMeta`'s flattened `extra`), not under a nested `meta.extra`.
