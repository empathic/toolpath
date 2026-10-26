<!-- Generated from the baseline's recordings by `crates/toolpath-goldens/tests/observed.rs`; do not edit.
     Regenerate: OBSERVED_UPDATE=1 cargo test -p toolpath-goldens --test observed -->

# claude: fields observed in the recordings

Computed from every recording of this harness the baseline runs. **required** = present in every record of its kind in these recordings; **optional** = seen missing at least once. Shapes are the JSON types seen. Arrays are collapsed to `[]`; a key that is data (a path, an id) is shown as `<key>`. This is what the recordings contain, not a specification: a field absent here may exist in versions we have no recording of.

## Recordings

- `goldens/claude-compacted/input.jsonl`
- `goldens/claude-session/input.jsonl`
- `test-fixtures/claude/convo.jsonl`

## Versions the recordings name

- `version` = `2.1.132` (`test-fixtures/claude/convo.jsonl`)
- `version` = `2.1.285` (`goldens/claude-session/input.jsonl`)

## `ai-title` (4 records)

| field | shape | presence |
|---|---|---|
| `aiTitle` | string | required |
| `sessionId` | string | required |
| `type` | string | required |

## `assistant` (54 records)

| field | shape | presence |
|---|---|---|
| `apiBlockIndex` | number | optional (5/54) |
| `cwd` | string | optional (51/54) |
| `entrypoint` | string | optional (51/54) |
| `gitBranch` | string | optional (51/54) |
| `isSidechain` | bool | optional (51/54) |
| `message.container` | null | optional (5/54) |
| `message.content` | string | optional (1/54) |
| `message.content[].caller.type` | string | optional (21/54) |
| `message.content[].id` | string | optional (23/54) |
| `message.content[].input.command` | string | optional (9/54) |
| `message.content[].input.content` | string | optional (5/54) |
| `message.content[].input.description` | string | optional (10/54) |
| `message.content[].input.file_path` | string | optional (10/54) |
| `message.content[].input.limit` | number | optional (1/54) |
| `message.content[].input.new_string` | string | optional (1/54) |
| `message.content[].input.old_string` | string | optional (1/54) |
| `message.content[].input.output_mode` | string | optional (1/54) |
| `message.content[].input.pattern` | string | optional (2/54) |
| `message.content[].input.prompt` | string | optional (1/54) |
| `message.content[].input.questions[].header` | string | optional (1/54) |
| `message.content[].input.questions[].multiSelect` | bool | optional (1/54) |
| `message.content[].input.questions[].options[].description` | string | optional (1/54) |
| `message.content[].input.questions[].options[].label` | string | optional (1/54) |
| `message.content[].input.questions[].question` | string | optional (1/54) |
| `message.content[].input.subagent_type` | string | optional (1/54) |
| `message.content[].name` | string | optional (23/54) |
| `message.content[].signature` | string | optional (15/54) |
| `message.content[].text` | string | optional (17/54) |
| `message.content[].thinking` | string | optional (15/54) |
| `message.content[].type` | string | optional (53/54) |
| `message.context_management` | null | optional (5/54) |
| `message.diagnostics` | null | optional (5/54) |
| `message.id` | string | optional (51/54) |
| `message.input_transformations` | array | optional (5/54) |
| `message.model` | string | required |
| `message.role` | string | required |
| `message.stop_details` | null | optional (51/54) |
| `message.stop_reason` | string | required |
| `message.stop_sequence` | null | optional (51/54) |
| `message.type` | string | optional (51/54) |
| `message.usage.cache_creation.ephemeral_1h_input_tokens` | number | optional (51/54) |
| `message.usage.cache_creation.ephemeral_5m_input_tokens` | number | optional (51/54) |
| `message.usage.cache_creation_input_tokens` | number | optional (51/54) |
| `message.usage.cache_read_input_tokens` | number | optional (51/54) |
| `message.usage.fallback_credit` | null | optional (5/54) |
| `message.usage.inference_geo` | string | optional (51/54) |
| `message.usage.input_tokens` | number | required |
| `message.usage.iterations[].cache_creation.ephemeral_1h_input_tokens` | number | optional (51/54) |
| `message.usage.iterations[].cache_creation.ephemeral_5m_input_tokens` | number | optional (51/54) |
| `message.usage.iterations[].cache_creation_input_tokens` | number | optional (51/54) |
| `message.usage.iterations[].cache_read_input_tokens` | number | optional (51/54) |
| `message.usage.iterations[].input_tokens` | number | optional (51/54) |
| `message.usage.iterations[].output_tokens` | number | optional (51/54) |
| `message.usage.iterations[].type` | string | optional (51/54) |
| `message.usage.output_tokens` | number | required |
| `message.usage.output_tokens_details.thinking_tokens` | number | optional (5/54) |
| `message.usage.server_tool_use.web_fetch_requests` | number | optional (51/54) |
| `message.usage.server_tool_use.web_search_requests` | number | optional (51/54) |
| `message.usage.service_tier` | string | optional (51/54) |
| `message.usage.speed` | string | optional (51/54) |
| `parentUuid` | string | required |
| `perTurnEffort` | null | optional (5/54) |
| `requestId` | string | optional (51/54) |
| `sessionId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |
| `userType` | string | optional (51/54) |
| `uuid` | string | required |
| `version` | string | optional (51/54) |
| `wireToolInputs.toolu_016Jcqs5U6qdvMJZjjrcRFvq.command` | string | optional (1/54) |
| `wireToolInputs.toolu_016Jcqs5U6qdvMJZjjrcRFvq.description` | string | optional (1/54) |
| `wireToolInputs.toolu_01GjqjxW9tnUh9qzsRWnoEWW.file_path` | string | optional (1/54) |
| `wireToolInputs.toolu_01GjqjxW9tnUh9qzsRWnoEWW.limit` | number | optional (1/54) |

## `atis-latch` (3 records)

| field | shape | presence |
|---|---|---|
| `atis` | string | required |
| `sessionId` | string | required |
| `type` | string | required |

## `attachment` (17 records)

| field | shape | presence |
|---|---|---|
| `attachment.addedLines[]` | string | optional (3/17) |
| `attachment.addedNames[]` | string | optional (2/17) |
| `attachment.addedTypes[]` | string | optional (1/17) |
| `attachment.builtInTypes[]` | string | optional (1/17) |
| `attachment.cliPrefix` | string | optional (1/17) |
| `attachment.commit` | string | optional (1/17) |
| `attachment.content` | array, string | optional (5/17) |
| `attachment.context` | object | optional (1/17) |
| `attachment.contextRendering` | string | optional (2/17) |
| `attachment.date` | string | optional (1/17) |
| `attachment.echoWireToolInputs` | bool | optional (2/17) |
| `attachment.failedMcpServers` | array | optional (1/17) |
| `attachment.identity.knowledgeCutoff` | string | optional (1/17) |
| `attachment.identity.marketingName` | string | optional (1/17) |
| `attachment.identity.modelId` | string | optional (1/17) |
| `attachment.isInitial` | bool | optional (3/17) |
| `attachment.itemCount` | number | optional (3/17) |
| `attachment.managedCommit` | bool | optional (1/17) |
| `attachment.managedPr` | bool | optional (1/17) |
| `attachment.names[]` | string | optional (1/17) |
| `attachment.needsAuthMcpServers` | array | optional (1/17) |
| `attachment.pendingMcpServers` | array | optional (1/17) |
| `attachment.pr` | string | optional (1/17) |
| `attachment.readdedNames` | array | optional (2/17) |
| `attachment.reminderFold` | bool | optional (2/17) |
| `attachment.removedNames` | array | optional (2/17) |
| `attachment.removedTypes` | array | optional (1/17) |
| `attachment.sendUserFileHint` | bool | optional (1/17) |
| `attachment.showConcurrencyNote` | bool | optional (1/17) |
| `attachment.skillCount` | number | optional (2/17) |
| `attachment.snapshot.additionalWorkingDirectories` | array | optional (1/17) |
| `attachment.snapshot.isGitRepo` | bool | optional (1/17) |
| `attachment.snapshot.isWorktree` | bool | optional (1/17) |
| `attachment.snapshot.osVersion` | string | optional (1/17) |
| `attachment.snapshot.platform` | string | optional (1/17) |
| `attachment.snapshot.shell` | string | optional (1/17) |
| `attachment.snapshot.workingDirectory` | string | optional (1/17) |
| `attachment.systemPrompt[]` | string | optional (2/17) |
| `attachment.text` | string | optional (3/17) |
| `attachment.tools[].description` | string | optional (1/17) |
| `attachment.tools[].name` | string | optional (1/17) |
| `attachment.tools[].schema.description` | string | optional (1/17) |
| `attachment.tools[].schema.eager_input_streaming` | bool | optional (1/17) |
| `attachment.tools[].schema.input_schema.$schema` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.additionalProperties` | bool | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.-A.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.-A.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.-B.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.-B.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.-C.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.-C.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.-i.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.-i.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.-n.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.-n.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.-o.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.-o.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.args.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.args.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.channel.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.channel.maxLength` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.channel.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.command.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.command.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.content.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.content.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.context.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.context.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.dangerouslyDisableSandbox.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.dangerouslyDisableSandbox.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.delaySeconds.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.delaySeconds.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.description.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.description.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.file_path.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.file_path.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.additionalProperties` | bool | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.category.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.category.maxLength` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.category.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.failure_scenario.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.failure_scenario.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.file.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.file.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.line.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.line.maximum` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.line.minimum` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.line.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.outcome.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.outcome.enum[]` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.outcome.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.short_summary.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.short_summary.maxLength` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.short_summary.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.summary.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.summary.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.verdict.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.verdict.enum[]` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.properties.verdict.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.required[]` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.items.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.maxItems` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.findings.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.glob.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.glob.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.head_limit.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.head_limit.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.isolation.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.isolation.enum[]` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.isolation.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.level.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.level.enum[]` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.level.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.limit.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.limit.exclusiveMinimum` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.limit.maximum` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.limit.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.max_results.default` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.max_results.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.max_results.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.model.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.model.enum[]` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.model.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.multiline.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.multiline.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.name.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.name.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.new_string.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.new_string.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.noop.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.noop.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.offset.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.offset.maximum` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.offset.minimum` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.offset.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.old_string.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.old_string.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.output_mode.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.output_mode.enum[]` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.output_mode.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.pages.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.pages.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.path.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.path.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.pattern.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.pattern.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.prompt.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.prompt.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.q.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.q.maxLength` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.q.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.query.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.query.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.reason.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.reason.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.replace_all.default` | bool | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.replace_all.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.replace_all.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.resumeFromRunId.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.resumeFromRunId.pattern` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.resumeFromRunId.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.run_in_background.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.run_in_background.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.script.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.script.maxLength` | number | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.script.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.scriptPath.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.scriptPath.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.skill.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.skill.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.stop.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.stop.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.subagent_type.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.subagent_type.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.timeout.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.timeout.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.title.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.title.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.type.description` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.properties.type.type` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.required[]` | string | optional (1/17) |
| `attachment.tools[].schema.input_schema.type` | string | optional (1/17) |
| `attachment.tools[].schema.name` | string | optional (1/17) |
| `attachment.type` | string | required |
| `attachment.url` | null | optional (1/17) |
| `attachment.wireHiddenNames` | array | optional (1/17) |
| `cwd` | string | required |
| `entrypoint` | string | required |
| `gitBranch` | string | required |
| `isSidechain` | bool | required |
| `parentUuid` | string | required |
| `renderedRole` | string | optional (9/17) |
| `rendered[].content` | string | optional (9/17) |
| `sessionId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |
| `userType` | string | required |
| `uuid` | string | required |
| `version` | string | required |

## `compact_boundary` (1 record)

| field | shape | presence |
|---|---|---|
| `compactMetadata.preTokens` | number | required |
| `compactMetadata.trigger` | string | required |
| `logicalParentUuid` | string | required |
| `parentUuid` | null | required |
| `sessionId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |
| `uuid` | string | required |

## `cost-state` (1 record)

| field | shape | presence |
|---|---|---|
| `hasUnknownModelCost` | bool | required |
| `modelUsage.<key>.cacheCreationInputTokens` | number | required |
| `modelUsage.<key>.cacheReadInputTokens` | number | required |
| `modelUsage.<key>.costUSD` | number | required |
| `modelUsage.<key>.inputTokens` | number | required |
| `modelUsage.<key>.outputTokens` | number | required |
| `modelUsage.<key>.thinkingTokens` | number | required |
| `modelUsage.<key>.webSearchRequests` | number | required |
| `sessionId` | string | required |
| `startTime` | number | required |
| `totalAPIDuration` | number | required |
| `totalAPIDurationWithoutRetries` | number | required |
| `totalCostUSD` | number | required |
| `totalDuration` | number | required |
| `totalLinesAdded` | number | required |
| `totalLinesRemoved` | number | required |
| `totalToolDuration` | number | required |
| `type` | string | required |

## `last-prompt` (6 records)

| field | shape | presence |
|---|---|---|
| `lastPrompt` | string | required |
| `leafUuid` | string | required |
| `sessionId` | string | required |
| `type` | string | required |

## `queue-operation` (4 records)

| field | shape | presence |
|---|---|---|
| `content` | string | optional (2/4) |
| `operation` | string | required |
| `sessionId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `user` (28 records)

| field | shape | presence |
|---|---|---|
| `cwd` | string | optional (24/28) |
| `entrypoint` | string | optional (23/28) |
| `gitBranch` | string | optional (24/28) |
| `isCompactSummary` | bool | optional (1/28) |
| `isSidechain` | bool | optional (23/28) |
| `isVisibleInTranscriptOnly` | bool | optional (1/28) |
| `message.content` | string | optional (5/28) |
| `message.content[].content` | string | optional (22/28) |
| `message.content[].content[].text` | string | optional (1/28) |
| `message.content[].content[].type` | string | optional (1/28) |
| `message.content[].is_error` | bool | optional (19/28) |
| `message.content[].tool_use_id` | string | optional (23/28) |
| `message.content[].type` | string | optional (23/28) |
| `message.role` | string | required |
| `parentUuid` | null, string | optional (27/28) |
| `permissionMode` | string | optional (2/28) |
| `promptId` | string | optional (23/28) |
| `promptSource` | string | optional (1/28) |
| `sessionId` | string | required |
| `sourceToolAssistantUUID` | string | optional (21/28) |
| `timestamp` | string | required |
| `toolUseResult` | string | optional (13/28) |
| `toolUseResult.agentId` | string | optional (1/28) |
| `toolUseResult.agentType` | string | optional (1/28) |
| `toolUseResult.content` | string | optional (1/28) |
| `toolUseResult.content[].text` | string | optional (1/28) |
| `toolUseResult.content[].type` | string | optional (1/28) |
| `toolUseResult.durationMs` | number | optional (1/28) |
| `toolUseResult.file.content` | string | optional (1/28) |
| `toolUseResult.file.filePath` | string | optional (1/28) |
| `toolUseResult.file.numLines` | number | optional (1/28) |
| `toolUseResult.file.startLine` | number | optional (1/28) |
| `toolUseResult.file.totalLines` | number | optional (1/28) |
| `toolUseResult.filenames` | array | optional (2/28) |
| `toolUseResult.interrupted` | bool | optional (4/28) |
| `toolUseResult.isImage` | bool | optional (4/28) |
| `toolUseResult.mode` | string | optional (1/28) |
| `toolUseResult.noOutputExpected` | bool | optional (4/28) |
| `toolUseResult.numFiles` | number | optional (2/28) |
| `toolUseResult.numLines` | number | optional (1/28) |
| `toolUseResult.prompt` | string | optional (1/28) |
| `toolUseResult.status` | string | optional (1/28) |
| `toolUseResult.stderr` | string | optional (4/28) |
| `toolUseResult.stdout` | string | optional (4/28) |
| `toolUseResult.toolStats.bashCount` | number | optional (1/28) |
| `toolUseResult.toolStats.editFileCount` | number | optional (1/28) |
| `toolUseResult.toolStats.linesAdded` | number | optional (1/28) |
| `toolUseResult.toolStats.linesRemoved` | number | optional (1/28) |
| `toolUseResult.toolStats.otherToolCount` | number | optional (1/28) |
| `toolUseResult.toolStats.readCount` | number | optional (1/28) |
| `toolUseResult.toolStats.searchCount` | number | optional (1/28) |
| `toolUseResult.totalDurationMs` | number | optional (1/28) |
| `toolUseResult.totalTokens` | number | optional (1/28) |
| `toolUseResult.totalToolUseCount` | number | optional (1/28) |
| `toolUseResult.truncated` | bool | optional (1/28) |
| `toolUseResult.type` | string | optional (1/28) |
| `toolUseResult.usage.cache_creation.ephemeral_1h_input_tokens` | number | optional (1/28) |
| `toolUseResult.usage.cache_creation.ephemeral_5m_input_tokens` | number | optional (1/28) |
| `toolUseResult.usage.cache_creation_input_tokens` | number | optional (1/28) |
| `toolUseResult.usage.cache_read_input_tokens` | number | optional (1/28) |
| `toolUseResult.usage.inference_geo` | string | optional (1/28) |
| `toolUseResult.usage.input_tokens` | number | optional (1/28) |
| `toolUseResult.usage.iterations[].cache_creation.ephemeral_1h_input_tokens` | number | optional (1/28) |
| `toolUseResult.usage.iterations[].cache_creation.ephemeral_5m_input_tokens` | number | optional (1/28) |
| `toolUseResult.usage.iterations[].cache_creation_input_tokens` | number | optional (1/28) |
| `toolUseResult.usage.iterations[].cache_read_input_tokens` | number | optional (1/28) |
| `toolUseResult.usage.iterations[].input_tokens` | number | optional (1/28) |
| `toolUseResult.usage.iterations[].output_tokens` | number | optional (1/28) |
| `toolUseResult.usage.iterations[].type` | string | optional (1/28) |
| `toolUseResult.usage.output_tokens` | number | optional (1/28) |
| `toolUseResult.usage.server_tool_use.web_fetch_requests` | number | optional (1/28) |
| `toolUseResult.usage.server_tool_use.web_search_requests` | number | optional (1/28) |
| `toolUseResult.usage.service_tier` | string | optional (1/28) |
| `toolUseResult.usage.speed` | string | optional (1/28) |
| `turnOrigin` | string | optional (1/28) |
| `turnPosition.promptIndex` | number | optional (1/28) |
| `turnPosition.turnIndex` | number | optional (1/28) |
| `type` | string | required |
| `userType` | string | optional (23/28) |
| `uuid` | string | required |
| `version` | string | optional (23/28) |
