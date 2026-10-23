<!-- Generated from the baseline's recordings by `crates/toolpath-goldens/tests/observed.rs`; do not edit.
     Regenerate: OBSERVED_UPDATE=1 cargo test -p toolpath-goldens --test observed -->

# opencode: fields observed in the recordings

Computed from every recording of this harness the baseline runs. **required** = present in every record of its kind in these recordings; **optional** = seen missing at least once. Shapes are the JSON types seen. Arrays are collapsed to `[]`; a key that is data (a path, an id) is shown as `<key>`. This is what the recordings contain, not a specification: a field absent here may exist in versions we have no recording of.

## Recordings

- `test-fixtures/opencode/convo.json`

## Versions the recordings name

- `info.version` = `1.14.41` (`test-fixtures/opencode/convo.json`)

## `document` (1 record)

| field | shape | presence |
|---|---|---|
| `info.directory` | string | required |
| `info.id` | string | required |
| `info.path` | string | required |
| `info.permission[].action` | string | required |
| `info.permission[].pattern` | string | required |
| `info.permission[].permission` | string | required |
| `info.projectID` | string | required |
| `info.slug` | string | required |
| `info.summary.additions` | number | required |
| `info.summary.deletions` | number | required |
| `info.summary.files` | number | required |
| `info.time.created` | number | required |
| `info.time.updated` | number | required |
| `info.title` | string | required |
| `info.version` | string | required |
| `messages[].info.agent` | string | required |
| `messages[].info.cost` | number | required |
| `messages[].info.finish` | string | required |
| `messages[].info.id` | string | required |
| `messages[].info.mode` | string | required |
| `messages[].info.model.modelID` | string | required |
| `messages[].info.model.providerID` | string | required |
| `messages[].info.modelID` | string | required |
| `messages[].info.parentID` | string | required |
| `messages[].info.path.cwd` | string | required |
| `messages[].info.path.root` | string | required |
| `messages[].info.providerID` | string | required |
| `messages[].info.role` | string | required |
| `messages[].info.sessionID` | string | required |
| `messages[].info.summary.diffs` | array | required |
| `messages[].info.time.completed` | number | required |
| `messages[].info.time.created` | number | required |
| `messages[].info.tokens.cache.read` | number | required |
| `messages[].info.tokens.cache.write` | number | required |
| `messages[].info.tokens.input` | number | required |
| `messages[].info.tokens.output` | number | required |
| `messages[].info.tokens.reasoning` | number | required |
| `messages[].info.tokens.total` | number | required |
| `messages[].parts[].callID` | string | required |
| `messages[].parts[].cost` | number | required |
| `messages[].parts[].id` | string | required |
| `messages[].parts[].messageID` | string | required |
| `messages[].parts[].reason` | string | required |
| `messages[].parts[].sessionID` | string | required |
| `messages[].parts[].state.error` | string | required |
| `messages[].parts[].state.input.command` | string | required |
| `messages[].parts[].state.input.content` | string | required |
| `messages[].parts[].state.input.description` | string | required |
| `messages[].parts[].state.input.filePath` | string | required |
| `messages[].parts[].state.input.newString` | string | required |
| `messages[].parts[].state.input.oldString` | string | required |
| `messages[].parts[].state.input.path` | string | required |
| `messages[].parts[].state.input.pattern` | string | required |
| `messages[].parts[].state.input.prompt` | string | required |
| `messages[].parts[].state.input.subagent_type` | string | required |
| `messages[].parts[].state.input.workdir` | string | required |
| `messages[].parts[].state.metadata.count` | number | required |
| `messages[].parts[].state.metadata.description` | string | required |
| `messages[].parts[].state.metadata.diagnostics` | object | required |
| `messages[].parts[].state.metadata.diff` | string | required |
| `messages[].parts[].state.metadata.exists` | bool | required |
| `messages[].parts[].state.metadata.exit` | number | required |
| `messages[].parts[].state.metadata.filediff.additions` | number | required |
| `messages[].parts[].state.metadata.filediff.deletions` | number | required |
| `messages[].parts[].state.metadata.filediff.file` | string | required |
| `messages[].parts[].state.metadata.filediff.patch` | string | required |
| `messages[].parts[].state.metadata.filepath` | string | required |
| `messages[].parts[].state.metadata.loaded` | array | required |
| `messages[].parts[].state.metadata.matches` | number | required |
| `messages[].parts[].state.metadata.model.modelID` | string | required |
| `messages[].parts[].state.metadata.model.providerID` | string | required |
| `messages[].parts[].state.metadata.output` | string | required |
| `messages[].parts[].state.metadata.preview` | string | required |
| `messages[].parts[].state.metadata.sessionId` | string | required |
| `messages[].parts[].state.metadata.truncated` | bool | required |
| `messages[].parts[].state.output` | string | required |
| `messages[].parts[].state.status` | string | required |
| `messages[].parts[].state.time.end` | number | required |
| `messages[].parts[].state.time.start` | number | required |
| `messages[].parts[].state.title` | string | required |
| `messages[].parts[].text` | string | required |
| `messages[].parts[].time.end` | number | required |
| `messages[].parts[].time.start` | number | required |
| `messages[].parts[].tokens.cache.read` | number | required |
| `messages[].parts[].tokens.cache.write` | number | required |
| `messages[].parts[].tokens.input` | number | required |
| `messages[].parts[].tokens.output` | number | required |
| `messages[].parts[].tokens.reasoning` | number | required |
| `messages[].parts[].tokens.total` | number | required |
| `messages[].parts[].tool` | string | required |
| `messages[].parts[].type` | string | required |
