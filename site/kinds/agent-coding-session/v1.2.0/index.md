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

Every such path comes from one place: the shared `ConversationView → Path` derivation in `toolpath-convo` (`derive_path`), which the provider crates (`toolpath-claude`, `toolpath-gemini`, `toolpath-codex`, `toolpath-opencode`, `toolpath-cursor`, `toolpath-pi`) all call. The field shapes below are therefore exact. The only producer-specific parts are the contents of a tool's `input`, the diff text in a change's `raw`, and the value (not the meaning) of `group_id`.

Constraints apply by structural `type`, not by artifact key: a `change` entry is checked only when its `structural.type` is one named here, and extra properties never make a path invalid. [`schema.json`](./schema.json) encodes the rules; apply it alongside the base schema. The URI is immutable. Later revisions ship under a new version URI.

**Changed from [v1.1.0](/kinds/agent-coding-session/v1.1.0/):** adds the [**compaction boundary**](#compaction-boundary) step type (`conversation.compact`). v1.1.0's `group_id` and [group accounting](#group-accounting) carry forward unchanged. v1.2.0 documents are structurally valid v1.1.0 documents; the new version exists so consumers can rely on the compaction step type.

## The turn payload

One entry in a turn's `change` map has `structural.type` of `"conversation.append"`. Find it by that type: the artifact key is producer-specific, formed as `<source>://<conversation-id>` from the harness in `meta.source` (e.g. `claude-code://…`, `gemini-cli://…`, `codex://…`, `opencode://…`, `cursor://…`, `pi://…`).

Its `structural` object always carries:

| Field  | Type   | Meaning                                                            |
| ------ | ------ | ------------------------------------------------------------------ |
| `type` | string | the literal `"conversation.append"`                                |
| `role` | string | `"user"`, `"assistant"`, `"system"`, or a producer-specific string |
| `text` | string | the visible prose; present even when empty (`""`)                  |

It may also carry any of the following, present only when the turn has them:

| Field         | Type   | Meaning                                                       |
| ------------- | ------ | ------------------------------------------------------------- |
| `thinking`    | string | the model's reasoning text                                    |
| `group_id`  | string | groups the steps derived from one source accounting unit (see below) |
| `tool_uses`   | array  | tools the agent invoked (shape below)                         |
| `token_usage` | object | the group's token counts (shape and rule below)      |
| `attributed_token_usage` | object | this step's own attributed spend, when known (see below) |
| `stop_reason` | string | why the model stopped (`end_turn`, `tool_use`, …)             |
| `delegations` | array  | sub-agent work spawned from this turn (shape below)           |
| `environment` | object | working environment at this turn (shape below)                |

The model identifier is not on the change. It lives in `step.actor` (`agent:<model>`) and `meta.actors`. There is no provider-specific blob: every field the derivation captures is one of those listed above.

### `group_id`

The provider's identifier for the **source accounting unit** these steps were derived from — Claude Code's `message.id` (`msg_…`) for one split message, Codex's round `turn_id` for one round (which may itself contain several messages). It is a **grouping key, not a step identifier**: when a producer derives several steps from one accounting unit (Claude Code writes one JSONL line per content block; a Codex round emits a commentary turn plus a final turn), every sibling step carries the same `group_id`. A step without a `group_id` is its own group of one. The stored value is the provider's verbatim id; only its *meaning* (which unit it names) is provider-specific.

### Group accounting

How `token_usage` on steps relates to the source's accounting units:

1. `token_usage` records a group's spend — a **per-group amount, never a cumulative session counter**.
2. Within a run of consecutive steps sharing a `group_id` (document order), the run's **last step carries the group's total `token_usage`, verbatim from the source**. In this version, the run's other steps carry none.
3. A step without a `group_id` is its own group and carries its own `token_usage` (when the source records one).

Consequence: **summing `token_usage` over a v1.2.0 path's steps yields the session totals.** Consumers need no dedup heuristics. (JSON Schema cannot express the once-per-run rule, so it is normative prose, enforced by producer test suites.)

`token_usage` has **one meaning everywhere it appears: the total for a group**. A step without a `group_id` is a one-step group, so its `token_usage` is that group's total (which is also its own spend — the two coincide for a group of one). Within a multi-step group, the total sits on the final step. Interpreting a value never requires reading the rest of its group: the key tells you it is a total, and `group_id` on the same payload tells you which group it totals. Per-step spend, when the source has it, rides a separate [`attributed_token_usage`](#per-step-attribution-attributed_token_usage) key — never `token_usage`. When a source format offers both a group total and a finer breakdown (Claude's `usage.iterations`, opencode's per-part `step-finish` tokens), `token_usage` carries the total; the breakdown is subordinate detail and does not ride `token_usage`.

### Per-step attribution: `attributed_token_usage`

Some sources expose, per step, the spend attributable to that step alone — distinct from the group total. Where a producer has it, the step carries an **`attributed_token_usage`** object (same shape as [`token_usage`](#token_usage)) holding *this step's own share*. It is **optional and orthogonal to `token_usage`**: whether a number is a group total or a step share is structural — the key it sits under — never positional. This is the rule that lets per-step accounting be added by any producer at any time without a new kind version.

How it relates to the group total:

- Within a `group_id` group, `Σ attributed_token_usage` over the group's steps is the group's attributed spend. The **unattributed remainder** — anything the source could not pin to a step — is *computed* by a consumer as `group's token_usage − Σ group's attributed_token_usage`; it is never recorded, so stored values stay verbatim source observations and source inconsistencies stay visible.
- For a group where the source attributes everything (e.g. Codex, where each step is a separate API call and the per-call delta is reported directly), the remainder is zero and `Σ attributed_token_usage == token_usage`.
- A group with no per-step data carries no `attributed_token_usage` at all — only the group total. Producers must not fabricate a split.

A producer populates `attributed_token_usage` only when the source genuinely reports per-step spend. Among current producers, **Codex does** (its `token_count` events carry a per-call delta). **Claude does not**: its per-content-block `usage` values are cumulative streaming snapshots stamped at flush time, not per-block costs, so deriving a split from them would be fabrication — Claude-derived steps carry the group total only.

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

| Field                | Type            | Notes                           |
| -------------------- | --------------- | ------------------------------- |
| `input_tokens`       | integer \| null | always present                  |
| `output_tokens`      | integer \| null | always present                  |
| `cache_read_tokens`  | integer         | only when the source records it |
| `cache_write_tokens` | integer         | only when the source records it |
| `breakdowns`         | object          | only when the source itemizes a class (see below) |

Values follow the [group accounting](#group-accounting) rule above.

`breakdowns` is an **optional, informational** decomposition of a top-level class into named sub-classes. It is keyed by the class being broken down (e.g. `"output"`); each value is a map of sub-class → tokens (e.g. `{ "output": { "reasoning": 450 } }`). Breakdowns are **never summed into any total** — the parent class already counts these tokens; a breakdown only says *how* that class divides. Invariant: **`Σ(inner) ≤` the parent class's value**. The field is omitted entirely when empty. The same shape and rule apply on `attributed_token_usage`. Among current producers, Gemini, OpenCode, and Codex record `output → { reasoning }` (their reasoning/thoughts tokens are part of `output_tokens`); Claude records none (its JSONL `usage` does not itemize thinking tokens).

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

## Compaction boundary

When a harness compacts its context, the derivation emits one step whose `change` entry has `structural.type` of `"conversation.compact"`. It uses the same `<source>://<conversation-id>` artifact key as the turn payload. The step sits between the turns it separates: the turns after the boundary parent on it, so the `head`-ancestry walk crosses the compaction in order.

Only `type` is always present. Every other field appears only when the source records it:

| Field        | Type             | Meaning                                                                          |
| ------------ | ---------------- | -------------------------------------------------------------------------------- |
| `type`       | string           | the literal `"conversation.compact"`                                             |
| `trigger`    | string           | `"auto"` (context overflow) or `"manual"` (user-invoked), when known             |
| `summary`    | string           | the compaction summary text the harness produced, when one was recorded          |
| `pre_tokens` | number           | the context token count immediately before the boundary, when known             |
| `kept`       | array            | ids of the prior turns that survive verbatim into the post-compaction window (may be non-contiguous; empty = wholesale) |

A compaction step has no `text`, `role`, or `tool_uses` — it is not a turn. Consumers that only care about the transcript can skip it; consumers reconstructing the source format use it to place the boundary. The `kept` ids are the harness-agnostic payload: each harness's projector renders that set in its own form (Claude re-emits those turns on-chain before the boundary; opencode/Pi anchor a kept tail at the earliest id; Codex keeps none).

## Non-turn entries

Entries that aren't turns (attachments, preamble lines, snapshots, hook results) become steps with `structural.type` of `"conversation.event"`, carrying `entry_type` and sometimes `event_source_id` plus the producer's event data. They exist so a document round-trips back to the source format. They are not part of the transcript.

## Actors

`step.actor` follows the `type:name` convention, assigned by role:

| Actor             | Turn                                                                                           |
| ----------------- | ---------------------------------------------------------------------------------------------- |
| `human:user`      | a user message                                                                                 |
| `agent:<model>`   | a model reply, named by the recorded model, or `agent:unknown` when none was recorded          |
| `tool:<provider>` | a system turn (session init, system prompt), a compaction boundary, any other producer role, or a non-turn event step |

`meta.actors` defines each actor the steps reference; `agent:` entries carry `provider` and `model`. A turn's original role is always in its `role` field, so collapsing system and other roles onto `tool:<provider>` loses nothing. Walk steps in `head`-ancestry order for the linear transcript.

## Path metadata

| Field                | Meaning                                                                          |
| -------------------- | -------------------------------------------------------------------------------- |
| `meta.kind`          | this URI                                                                         |
| `meta.source`        | the producing harness: `claude-code`, `gemini-cli`, `codex`, `opencode`, `cursor`, or `pi` |
| `meta.title`         | session title                                                                    |
| `meta.actors`        | the actor definitions the steps reference                                        |
| `meta.files_changed` | file paths touched across the session                                            |
| `meta.vcs_remote`    | repository URL, when known                                                       |
| `meta.producer`      | `{ "name": string, "version"?: string }`, the software that produced the session |

`files_changed`, `vcs_remote`, and `producer` sit directly under `meta` (they ride `PathMeta`'s flattened `extra`), not under a nested `meta.extra`.
