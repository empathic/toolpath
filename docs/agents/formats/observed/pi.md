<!-- Generated from the baseline's recordings by `crates/toolpath-goldens/tests/observed.rs`; do not edit.
     Regenerate: OBSERVED_UPDATE=1 cargo test -p toolpath-goldens --test observed -->

# pi: fields observed in the recordings

Computed from every recording of this harness the baseline runs. **required** = present in every record of its kind in these recordings; **optional** = seen missing at least once. Shapes are the JSON types seen. Arrays are collapsed to `[]`; a key that is data (a path, an id) is shown as `<key>`. This is what the recordings contain, not a specification: a field absent here may exist in versions we have no recording of.

## Recordings

- `test-fixtures/pi/convo.jsonl`

## Versions the recordings name

- `version` = `3` (`test-fixtures/pi/convo.jsonl`)

## `message` (22 records)

| field | shape | presence |
|---|---|---|
| `id` | string | required |
| `message.api` | string | optional (11/22) |
| `message.content[].arguments.command` | string | optional (4/22) |
| `message.content[].arguments.content` | string | optional (2/22) |
| `message.content[].arguments.edits[].newText` | string | optional (1/22) |
| `message.content[].arguments.edits[].oldText` | string | optional (1/22) |
| `message.content[].arguments.path` | string | optional (6/22) |
| `message.content[].id` | string | optional (10/22) |
| `message.content[].name` | string | optional (10/22) |
| `message.content[].text` | string | optional (17/22) |
| `message.content[].thinking` | string | optional (6/22) |
| `message.content[].thinkingSignature` | string | optional (6/22) |
| `message.content[].type` | string | required |
| `message.details` | object | optional (1/22) |
| `message.details.diff` | string | optional (1/22) |
| `message.details.firstChangedLine` | number | optional (1/22) |
| `message.isError` | bool | optional (10/22) |
| `message.model` | string | optional (11/22) |
| `message.provider` | string | optional (11/22) |
| `message.responseId` | string | optional (11/22) |
| `message.role` | string | required |
| `message.stopReason` | string | optional (11/22) |
| `message.timestamp` | number | required |
| `message.toolCallId` | string | optional (10/22) |
| `message.toolName` | string | optional (10/22) |
| `message.usage.cacheRead` | number | optional (11/22) |
| `message.usage.cacheWrite` | number | optional (11/22) |
| `message.usage.cost.cacheRead` | number | optional (11/22) |
| `message.usage.cost.cacheWrite` | number | optional (11/22) |
| `message.usage.cost.input` | number | optional (11/22) |
| `message.usage.cost.output` | number | optional (11/22) |
| `message.usage.cost.total` | number | optional (11/22) |
| `message.usage.input` | number | optional (11/22) |
| `message.usage.output` | number | optional (11/22) |
| `message.usage.totalTokens` | number | optional (11/22) |
| `parentId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `model_change` (1 record)

| field | shape | presence |
|---|---|---|
| `id` | string | required |
| `modelId` | string | required |
| `parentId` | null | required |
| `provider` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `session` (1 record)

| field | shape | presence |
|---|---|---|
| `cwd` | string | required |
| `id` | string | required |
| `timestamp` | string | required |
| `type` | string | required |
| `version` | number | required |

## `thinking_level_change` (1 record)

| field | shape | presence |
|---|---|---|
| `id` | string | required |
| `parentId` | string | required |
| `thinkingLevel` | string | required |
| `timestamp` | string | required |
| `type` | string | required |
