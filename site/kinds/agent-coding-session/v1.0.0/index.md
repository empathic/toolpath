---
layout: base.njk
title: "Kind: agent-coding-session v1.0.0"
permalink: /kinds/agent-coding-session/v1.0.0/
---

# Kind: `agent-coding-session` v1.0.0

<dl class="kind-meta">
  <dt>URI</dt>
  <dd><code>https://toolpath.dev/kinds/agent-coding-session/v1.0.0</code></dd>
  <dt>Schema</dt>
  <dd><a href="./schema.json"><code>schema.json</code></a></dd>
</dl>

A Toolpath path whose `meta.kind` is this URI records an AI coding conversation. It is an ordinary path with the extra structure described here. `head`-ancestry, dead ends, signatures, and `base` all behave as in the [base format](/format/).

Every such path comes from one place: the shared `ConversationView → Path` derivation in `toolpath-convo` (`derive_path`), which the provider crates (`toolpath-claude`, `toolpath-gemini`, `toolpath-codex`, `toolpath-opencode`, `toolpath-pi`) all call. The field shapes below are therefore exact. The only producer-specific parts are the contents of a tool's `input` and the diff text in a change's `raw`.

Constraints apply by structural `type`, not by artifact key: a `change` entry is checked only when its `structural.type` is one named here, and extra properties never make a path invalid. [`schema.json`](./schema.json) encodes the rules; apply it alongside the base schema. The URI is immutable. Later revisions ship under a new version URI.

## The turn payload

One entry in a turn's `change` map has `structural.type` of `"conversation.append"`. Find it by that type: the artifact key is producer-specific, formed as `<source>://<conversation-id>` from the harness in `meta.source` (e.g. `claude-code://…`, `gemini-cli://…`, `codex://…`, `opencode://…`, `pi://…`).

Its `structural` object always carries:

| Field  | Type   | Meaning                                                            |
| ------ | ------ | ------------------------------------------------------------------ |
| `type` | string | the literal `"conversation.append"`                                |
| `role` | string | `"user"`, `"assistant"`, `"system"`, or a producer-specific string |
| `text` | string | the visible prose; present even when empty (`""`)                  |

It may also carry any of the following, present only when the turn has them:

| Field         | Type   | Meaning                                             |
| ------------- | ------ | --------------------------------------------------- |
| `thinking`    | string | the model's reasoning text                          |
| `tool_uses`   | array  | tools the agent invoked (shape below)               |
| `token_usage` | object | per-turn token counts (shape below)                 |
| `stop_reason` | string | why the model stopped (`end_turn`, `tool_use`, …)   |
| `delegations` | array  | sub-agent work spawned from this turn (shape below) |
| `environment` | object | working environment at this turn (shape below)      |

The model identifier is not on the change. It lives in `step.actor` (`agent:<model>`) and `meta.actors`. There is no provider-specific blob: every field the derivation captures is one of those listed above.

### `tool_uses`

Each element is an object:

| Field      | Type           | Notes                                                                                                                              |
| ---------- | -------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `id`       | string         | provider-assigned invocation id                                                                                                    |
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
| `meta.source`        | the producing harness: `claude-code`, `gemini-cli`, `codex`, `opencode`, or `pi` |
| `meta.title`         | session title                                                                    |
| `meta.actors`        | the actor definitions the steps reference                                        |
| `meta.files_changed` | file paths touched across the session                                            |
| `meta.vcs_remote`    | repository URL, when known                                                       |
| `meta.producer`      | `{ "name": string, "version"?: string }`, the software that produced the session |

`files_changed`, `vcs_remote`, and `producer` sit directly under `meta` (they ride `PathMeta`'s flattened `extra`), not under a nested `meta.extra`.
