<!-- Generated from the baseline's recordings by `crates/toolpath-goldens/tests/observed.rs`; do not edit.
     Regenerate: OBSERVED_UPDATE=1 cargo test -p toolpath-goldens --test observed -->

# codex: fields observed in the recordings

Computed from every recording of this harness the baseline runs. **required** = present in every record of its kind in these recordings; **optional** = seen missing at least once. Shapes are the JSON types seen. Arrays are collapsed to `[]`; a key that is data (a path, an id) is shown as `<key>`. This is what the recordings contain, not a specification: a field absent here may exist in versions we have no recording of.

## Recordings

- `test-fixtures/codex/convo.jsonl`

## Versions the recordings name

- `payload.cli_version` = `0.128.0` (`test-fixtures/codex/convo.jsonl`)

## `event_msg/agent_message` (13 records)

| field | shape | presence |
|---|---|---|
| `payload.memory_citation` | null | required |
| `payload.message` | string | required |
| `payload.phase` | string | required |
| `payload.type` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `event_msg/task_complete` (1 record)

| field | shape | presence |
|---|---|---|
| `payload.completed_at` | number | required |
| `payload.duration_ms` | number | required |
| `payload.last_agent_message` | string | required |
| `payload.time_to_first_token_ms` | number | required |
| `payload.turn_id` | string | required |
| `payload.type` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `event_msg/task_started` (2 records)

| field | shape | presence |
|---|---|---|
| `payload.collaboration_mode_kind` | string | required |
| `payload.model_context_window` | number | required |
| `payload.started_at` | number | required |
| `payload.turn_id` | string | required |
| `payload.type` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `event_msg/token_count` (13 records)

| field | shape | presence |
|---|---|---|
| `payload.info` | null | optional (1/13) |
| `payload.info.last_token_usage.cached_input_tokens` | number | optional (12/13) |
| `payload.info.last_token_usage.input_tokens` | number | optional (12/13) |
| `payload.info.last_token_usage.output_tokens` | number | optional (12/13) |
| `payload.info.last_token_usage.reasoning_output_tokens` | number | optional (12/13) |
| `payload.info.last_token_usage.total_tokens` | number | optional (12/13) |
| `payload.info.model_context_window` | number | optional (12/13) |
| `payload.info.total_token_usage.cached_input_tokens` | number | optional (12/13) |
| `payload.info.total_token_usage.input_tokens` | number | optional (12/13) |
| `payload.info.total_token_usage.output_tokens` | number | optional (12/13) |
| `payload.info.total_token_usage.reasoning_output_tokens` | number | optional (12/13) |
| `payload.info.total_token_usage.total_tokens` | number | optional (12/13) |
| `payload.rate_limits.credits` | null | required |
| `payload.rate_limits.limit_id` | string | required |
| `payload.rate_limits.limit_name` | null | required |
| `payload.rate_limits.plan_type` | string | required |
| `payload.rate_limits.primary.resets_at` | number | required |
| `payload.rate_limits.primary.used_percent` | number | required |
| `payload.rate_limits.primary.window_minutes` | number | required |
| `payload.rate_limits.rate_limit_reached_type` | null | required |
| `payload.rate_limits.secondary.resets_at` | number | required |
| `payload.rate_limits.secondary.used_percent` | number | required |
| `payload.rate_limits.secondary.window_minutes` | number | required |
| `payload.type` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `event_msg/user_message` (2 records)

| field | shape | presence |
|---|---|---|
| `payload.images` | array | required |
| `payload.local_images` | array | required |
| `payload.message` | string | required |
| `payload.text_elements` | array | required |
| `payload.type` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `response_item/function_call` (1 record)

| field | shape | presence |
|---|---|---|
| `payload.arguments` | string | required |
| `payload.call_id` | string | required |
| `payload.name` | string | required |
| `payload.type` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `response_item/function_call_output` (1 record)

| field | shape | presence |
|---|---|---|
| `payload.call_id` | string | required |
| `payload.output` | string | required |
| `payload.type` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `response_item/message` (8 records)

| field | shape | presence |
|---|---|---|
| `payload.content[].text` | string | required |
| `payload.content[].type` | string | required |
| `payload.phase` | string | optional (2/8) |
| `payload.role` | string | required |
| `payload.type` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `response_item/reasoning` (1 record)

| field | shape | presence |
|---|---|---|
| `payload.content` | null | required |
| `payload.encrypted_content` | string | required |
| `payload.summary` | array | required |
| `payload.type` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `session_meta` (2 records)

| field | shape | presence |
|---|---|---|
| `payload.agent_nickname` | string | optional (1/2) |
| `payload.base_instructions.text` | string | required |
| `payload.cli_version` | string | required |
| `payload.cwd` | string | required |
| `payload.forked_from_id` | string | optional (1/2) |
| `payload.id` | string | required |
| `payload.model_provider` | string | required |
| `payload.originator` | string | required |
| `payload.source` | string | optional (1/2) |
| `payload.source.subagent.thread_spawn.agent_nickname` | string | optional (1/2) |
| `payload.source.subagent.thread_spawn.agent_path` | null | optional (1/2) |
| `payload.source.subagent.thread_spawn.agent_role` | null | optional (1/2) |
| `payload.source.subagent.thread_spawn.depth` | number | optional (1/2) |
| `payload.source.subagent.thread_spawn.parent_thread_id` | string | optional (1/2) |
| `payload.timestamp` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `turn_context` (1 record)

| field | shape | presence |
|---|---|---|
| `payload.approval_policy` | string | required |
| `payload.collaboration_mode.mode` | string | required |
| `payload.collaboration_mode.settings.developer_instructions` | null | required |
| `payload.collaboration_mode.settings.model` | string | required |
| `payload.collaboration_mode.settings.reasoning_effort` | string | required |
| `payload.current_date` | string | required |
| `payload.cwd` | string | required |
| `payload.effort` | string | required |
| `payload.file_system_sandbox_policy.entries[].access` | string | required |
| `payload.file_system_sandbox_policy.entries[].path.path` | string | required |
| `payload.file_system_sandbox_policy.entries[].path.type` | string | required |
| `payload.file_system_sandbox_policy.entries[].path.value.kind` | string | required |
| `payload.file_system_sandbox_policy.entries[].path.value.subpath` | string | required |
| `payload.file_system_sandbox_policy.kind` | string | required |
| `payload.model` | string | required |
| `payload.permission_profile.file_system.entries[].access` | string | required |
| `payload.permission_profile.file_system.entries[].path.path` | string | required |
| `payload.permission_profile.file_system.entries[].path.type` | string | required |
| `payload.permission_profile.file_system.entries[].path.value.kind` | string | required |
| `payload.permission_profile.file_system.entries[].path.value.subpath` | string | required |
| `payload.permission_profile.file_system.type` | string | required |
| `payload.permission_profile.network` | string | required |
| `payload.permission_profile.type` | string | required |
| `payload.personality` | string | required |
| `payload.realtime_active` | bool | required |
| `payload.sandbox_policy.exclude_slash_tmp` | bool | required |
| `payload.sandbox_policy.exclude_tmpdir_env_var` | bool | required |
| `payload.sandbox_policy.network_access` | bool | required |
| `payload.sandbox_policy.type` | string | required |
| `payload.sandbox_policy.writable_roots[]` | string | required |
| `payload.summary` | string | required |
| `payload.timezone` | string | required |
| `payload.truncation_policy.limit` | number | required |
| `payload.truncation_policy.mode` | string | required |
| `payload.turn_id` | string | required |
| `timestamp` | string | required |
| `type` | string | required |
