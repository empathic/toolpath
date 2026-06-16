---
layout: base.njk
title: "Kind: agent-coding-session v1.1.0"
permalink: /kinds/agent-coding-session/v1.1.0/
---

# Kind: `agent-coding-session` v1.1.0

<dl class="kind-meta">
  <dt>URI</dt>
  <dd><code>https://toolpath.net/kinds/agent-coding-session/v1.1.0</code></dd>
  <dt>Schema</dt>
  <dd><a href="./schema.json"><code>schema.json</code></a></dd>
</dl>

A Toolpath path whose `meta.kind` is this URI records an AI coding conversation. It is an ordinary path with the extra structure described here. `head`-ancestry, dead ends, signatures, and `base` all behave as in the [base format](/format/).

Every such path comes from one place: the shared `ConversationView → Path` derivation in `toolpath-convo` (`derive_path`), which the provider crates (`toolpath-claude`, `toolpath-gemini`, `toolpath-codex`, `toolpath-opencode`, `toolpath-cursor`, `toolpath-pi`) all call. The field shapes below are therefore exact. The only producer-specific parts are the contents of a tool's `input`, the diff text in a change's `raw`, and the value (not the meaning) of `message_id`.

Constraints apply by structural `type`, not by artifact key: a `change` entry is checked only when its `structural.type` is one named here, and extra properties never make a path invalid. [`schema.json`](./schema.json) encodes the rules; apply it alongside the base schema. The URI is immutable. Later revisions ship under a new version URI.

**Changed from [v1.0.0](/kinds/agent-coding-session/v1.0.0/):** the turn payload gains an optional `message_id`, and message-level token accounting is now specified — see [Message accounting](#message-accounting). v1.1.0 documents are structurally valid v1.0.0 documents; the new version exists so consumers can rely on the accounting rule.

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
| `message_id`  | string | provider-assigned ID of the source message (see below)        |
| `tool_uses`   | array  | tools the agent invoked (shape below)                         |
| `token_usage` | object | the source message's token counts (shape and rule below)      |
| `attributed_token_usage` | object | this step's own attributed spend, when known (see below) |
| `stop_reason` | string | why the model stopped (`end_turn`, `tool_use`, …)             |
| `delegations` | array  | sub-agent work spawned from this turn (shape below)           |
| `environment` | object | working environment at this turn (shape below)                |

The model identifier is not on the change. It lives in `step.actor` (`agent:<model>`) and `meta.actors`. There is no provider-specific blob: every field the derivation captures is one of those listed above.

### `message_id`

The provider-assigned ID of the source message this turn was derived from — Claude Code's `message.id` (`msg_…`), for example. It is a **grouping key, not a turn identifier**: when a producer splits one provider message across several steps (Claude Code writes one JSONL line per content block), every sibling step carries the same `message_id`. A step without a `message_id` is its own message.

### Message accounting

How `token_usage` on steps relates to API-message accounting:

1. `token_usage` records the source message's spend — a **per-message amount, never a cumulative session counter**.
2. Within a run of consecutive steps sharing a `message_id` (document order), the run's **last step carries the message's total `token_usage`, verbatim from the source**. In this version, the run's other steps carry none.
3. A step without a `message_id` is its own message and carries its own `token_usage` (when the source records one).

Consequence: **summing `token_usage` over a v1.1.0 path's steps yields the session totals.** Consumers need no dedup heuristics. (JSON Schema cannot express the once-per-run rule, so it is normative prose, enforced by producer test suites.)

`token_usage` has **one meaning everywhere it appears: the total for a message**. A step without a `message_id` is a one-step message, so its `token_usage` is that message's total (which is also its own spend — the two coincide for a group of one). Within a multi-step group, the total sits on the final step. Interpreting a value never requires reading the rest of its group: the key tells you it is a total, and `message_id` on the same payload tells you which message it totals. Per-step spend, when the source has it, rides a separate [`attributed_token_usage`](#per-step-attribution-attributed_token_usage) key — never `token_usage`. When a source format offers both a message total and a finer breakdown (Claude's `usage.iterations`, opencode's per-part `step-finish` tokens), `token_usage` carries the total; the breakdown is subordinate detail and does not ride `token_usage`.

### Per-step attribution: `attributed_token_usage`

Some sources expose, per step, the spend attributable to that step alone — distinct from the message total. Where a producer has it, the step carries an **`attributed_token_usage`** object (same shape as [`token_usage`](#token_usage)) holding *this step's own share*. It is **optional and orthogonal to `token_usage`**: whether a number is a message total or a step share is structural — the key it sits under — never positional. This is the rule that lets per-step accounting be added by any producer at any time without a new kind version.

How it relates to the message total:

- Within a `message_id` group, `Σ attributed_token_usage` over the group's steps is the group's attributed spend. The **unattributed remainder** — anything the source could not pin to a step — is *computed* by a consumer as `group's token_usage − Σ group's attributed_token_usage`; it is never recorded, so stored values stay verbatim source observations and source inconsistencies stay visible.
- For a group where the source attributes everything (e.g. Codex, where each step is a separate API call and the per-call delta is reported directly), the remainder is zero and `Σ attributed_token_usage == token_usage`.
- A group with no per-step data carries no `attributed_token_usage` at all — only the message total. Producers must not fabricate a split.

A producer populates `attributed_token_usage` only when the source genuinely reports per-step spend. Among current producers, **Codex does** (its `token_count` events carry a per-call delta). **Claude does not**: its per-content-block `usage` values are cumulative streaming snapshots stamped at flush time, not per-block costs, so deriving a split from them would be fabrication — Claude-derived steps carry the message total only.

`Σ token_usage` over a path's steps is unaffected by `attributed_token_usage` (they are separate keys), so the session-total guarantee above always holds. A consumer wanting per-step cost reads `attributed_token_usage` where present and falls back to the message total otherwise.

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

Values follow the [message accounting](#message-accounting) rule above.

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
