<!-- Generated from the baseline's recordings by `crates/toolpath-goldens/tests/observed.rs`; do not edit.
     Regenerate: OBSERVED_UPDATE=1 cargo test -p toolpath-goldens --test observed -->

# gemini: fields observed in the recordings

Computed from every recording of this harness the baseline runs. **required** = present in every record of its kind in these recordings; **optional** = seen missing at least once. Shapes are the JSON types seen. Arrays are collapsed to `[]`; a key that is data (a path, an id) is shown as `<key>`. This is what the recordings contain, not a specification: a field absent here may exist in versions we have no recording of.

## Recordings

- `test-fixtures/gemini/convo.jsonl`

## Versions the recordings name

None: these recordings do not record the harness version.

## `$set` (12 records)

| field | shape | presence |
|---|---|---|
| `$set.lastUpdated` | string | required |

## `(no type)` (1 record)

| field | shape | presence |
|---|---|---|
| `kind` | string | required |
| `lastUpdated` | string | required |
| `projectHash` | string | required |
| `sessionId` | string | required |
| `startTime` | string | required |

## `gemini` (21 records)

| field | shape | presence |
|---|---|---|
| `content` | string | required |
| `id` | string | required |
| `model` | string | required |
| `thoughts` | array | optional (19/21) |
| `thoughts[].description` | string | optional (2/21) |
| `thoughts[].subject` | string | optional (2/21) |
| `thoughts[].timestamp` | string | optional (2/21) |
| `timestamp` | string | required |
| `tokens.cached` | number | required |
| `tokens.input` | number | required |
| `tokens.output` | number | required |
| `tokens.thoughts` | number | required |
| `tokens.tool` | number | required |
| `tokens.total` | number | required |
| `toolCalls[].agentId` | string | optional (1/21) |
| `toolCalls[].args.agent_name` | string | optional (1/21) |
| `toolCalls[].args.command` | string | optional (1/21) |
| `toolCalls[].args.content` | string | optional (2/21) |
| `toolCalls[].args.context` | number | optional (1/21) |
| `toolCalls[].args.description` | string | optional (1/21) |
| `toolCalls[].args.dir_path` | string | optional (1/21) |
| `toolCalls[].args.file_path` | string | optional (5/21) |
| `toolCalls[].args.instruction` | string | optional (1/21) |
| `toolCalls[].args.new_string` | string | optional (1/21) |
| `toolCalls[].args.old_string` | string | optional (1/21) |
| `toolCalls[].args.pattern` | string | optional (2/21) |
| `toolCalls[].args.prompt` | string | optional (1/21) |
| `toolCalls[].description` | string | optional (10/21) |
| `toolCalls[].displayName` | string | optional (10/21) |
| `toolCalls[].id` | string | optional (10/21) |
| `toolCalls[].name` | string | optional (10/21) |
| `toolCalls[].renderOutputAsMarkdown` | bool | optional (10/21) |
| `toolCalls[].resultDisplay` | string | optional (3/21) |
| `toolCalls[].resultDisplay.agentName` | string | optional (1/21) |
| `toolCalls[].resultDisplay.diffStat.model_added_chars` | number | optional (3/21) |
| `toolCalls[].resultDisplay.diffStat.model_added_lines` | number | optional (3/21) |
| `toolCalls[].resultDisplay.diffStat.model_removed_chars` | number | optional (3/21) |
| `toolCalls[].resultDisplay.diffStat.model_removed_lines` | number | optional (3/21) |
| `toolCalls[].resultDisplay.diffStat.user_added_chars` | number | optional (3/21) |
| `toolCalls[].resultDisplay.diffStat.user_added_lines` | number | optional (3/21) |
| `toolCalls[].resultDisplay.diffStat.user_removed_chars` | number | optional (3/21) |
| `toolCalls[].resultDisplay.diffStat.user_removed_lines` | number | optional (3/21) |
| `toolCalls[].resultDisplay.fileDiff` | string | optional (3/21) |
| `toolCalls[].resultDisplay.fileName` | string | optional (3/21) |
| `toolCalls[].resultDisplay.filePath` | string | optional (3/21) |
| `toolCalls[].resultDisplay.files[]` | string | optional (1/21) |
| `toolCalls[].resultDisplay.isNewFile` | bool | optional (3/21) |
| `toolCalls[].resultDisplay.isSubagentProgress` | bool | optional (1/21) |
| `toolCalls[].resultDisplay.matches[].absolutePath` | string | optional (1/21) |
| `toolCalls[].resultDisplay.matches[].filePath` | string | optional (1/21) |
| `toolCalls[].resultDisplay.matches[].line` | string | optional (1/21) |
| `toolCalls[].resultDisplay.matches[].lineNumber` | number | optional (1/21) |
| `toolCalls[].resultDisplay.newContent` | string | optional (3/21) |
| `toolCalls[].resultDisplay.originalContent` | string | optional (3/21) |
| `toolCalls[].resultDisplay.recentActivity[].args` | string | optional (1/21) |
| `toolCalls[].resultDisplay.recentActivity[].content` | string | optional (1/21) |
| `toolCalls[].resultDisplay.recentActivity[].description` | string | optional (1/21) |
| `toolCalls[].resultDisplay.recentActivity[].displayName` | string | optional (1/21) |
| `toolCalls[].resultDisplay.recentActivity[].id` | string | optional (1/21) |
| `toolCalls[].resultDisplay.recentActivity[].status` | string | optional (1/21) |
| `toolCalls[].resultDisplay.recentActivity[].type` | string | optional (1/21) |
| `toolCalls[].resultDisplay.result` | string | optional (1/21) |
| `toolCalls[].resultDisplay.state` | string | optional (1/21) |
| `toolCalls[].resultDisplay.summary` | string | optional (2/21) |
| `toolCalls[].resultDisplay.terminateReason` | string | optional (1/21) |
| `toolCalls[].resultDisplay[][].bg` | string | optional (1/21) |
| `toolCalls[].resultDisplay[][].bold` | bool | optional (1/21) |
| `toolCalls[].resultDisplay[][].dim` | bool | optional (1/21) |
| `toolCalls[].resultDisplay[][].fg` | string | optional (1/21) |
| `toolCalls[].resultDisplay[][].inverse` | bool | optional (1/21) |
| `toolCalls[].resultDisplay[][].isUninitialized` | bool | optional (1/21) |
| `toolCalls[].resultDisplay[][].italic` | bool | optional (1/21) |
| `toolCalls[].resultDisplay[][].text` | string | optional (1/21) |
| `toolCalls[].resultDisplay[][].underline` | bool | optional (1/21) |
| `toolCalls[].result[].functionResponse.id` | string | optional (10/21) |
| `toolCalls[].result[].functionResponse.name` | string | optional (10/21) |
| `toolCalls[].result[].functionResponse.response.error` | string | optional (1/21) |
| `toolCalls[].result[].functionResponse.response.output` | string | optional (9/21) |
| `toolCalls[].status` | string | optional (10/21) |
| `toolCalls[].timestamp` | string | optional (10/21) |
| `type` | string | required |

## `user` (1 record)

| field | shape | presence |
|---|---|---|
| `content[].text` | string | required |
| `id` | string | required |
| `timestamp` | string | required |
| `type` | string | required |
