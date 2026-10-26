<!-- Generated from the baseline's recordings by `crates/toolpath-goldens/tests/observed.rs`; do not edit.
     Regenerate: OBSERVED_UPDATE=1 cargo test -p toolpath-goldens --test observed -->

# copilot: fields observed in the recordings

Computed from every recording of this harness the baseline runs. **required** = present in every record of its kind in these recordings; **optional** = seen missing at least once. Shapes are the JSON types seen. Arrays are collapsed to `[]`; a key that is data (a path, an id) is shown as `<key>`. This is what the recordings contain, not a specification: a field absent here may exist in versions we have no recording of.

## Recordings

- `test-fixtures/copilot/convo.jsonl`

## Versions the recordings name

- `data.copilotVersion` = `1.0.68` (`test-fixtures/copilot/convo.jsonl`)
- `data.version` = `1` (`test-fixtures/copilot/convo.jsonl`)

## `assistant.message` (13 records)

| field | shape | presence |
|---|---|---|
| `agentId` | string | optional (2/13) |
| `data.apiCallId` | string | required |
| `data.content` | string | required |
| `data.interactionId` | string | required |
| `data.messageId` | string | required |
| `data.model` | string | required |
| `data.outputTokens` | number | required |
| `data.parentToolCallId` | string | optional (2/13) |
| `data.reasoningOpaque` | string | required |
| `data.reasoningText` | string | required |
| `data.requestId` | string | required |
| `data.serviceRequestId` | string | required |
| `data.toolRequests` | array | optional (2/13) |
| `data.toolRequests[].arguments.agent_type` | string | optional (1/13) |
| `data.toolRequests[].arguments.command` | string | optional (1/13) |
| `data.toolRequests[].arguments.description` | string | optional (2/13) |
| `data.toolRequests[].arguments.file_text` | string | optional (2/13) |
| `data.toolRequests[].arguments.mode` | string | optional (1/13) |
| `data.toolRequests[].arguments.name` | string | optional (1/13) |
| `data.toolRequests[].arguments.new_str` | string | optional (1/13) |
| `data.toolRequests[].arguments.old_str` | string | optional (1/13) |
| `data.toolRequests[].arguments.path` | string | optional (7/13) |
| `data.toolRequests[].arguments.pattern` | string | optional (2/13) |
| `data.toolRequests[].arguments.prompt` | string | optional (1/13) |
| `data.toolRequests[].intentionSummary` | string | optional (11/13) |
| `data.toolRequests[].name` | string | optional (11/13) |
| `data.toolRequests[].toolCallId` | string | optional (11/13) |
| `data.toolRequests[].type` | string | optional (11/13) |
| `data.turnId` | string | required |
| `id` | string | required |
| `parentId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `assistant.turn_end` (11 records)

| field | shape | presence |
|---|---|---|
| `data.turnId` | string | required |
| `id` | string | required |
| `parentId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `assistant.turn_start` (11 records)

| field | shape | presence |
|---|---|---|
| `data.interactionId` | string | required |
| `data.turnId` | string | required |
| `id` | string | required |
| `parentId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `session.model_change` (1 record)

| field | shape | presence |
|---|---|---|
| `data.contextTier` | null | required |
| `data.newModel` | string | required |
| `data.reasoningEffort` | null | required |
| `id` | string | required |
| `parentId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `session.shutdown` (1 record)

| field | shape | presence |
|---|---|---|
| `data.codeChanges.filesModified[]` | string | required |
| `data.codeChanges.linesAdded` | number | required |
| `data.codeChanges.linesRemoved` | number | required |
| `data.conversationTokens` | number | required |
| `data.currentModel` | string | required |
| `data.currentTokens` | number | required |
| `data.eventsFileSizeBytes` | number | required |
| `data.modelMetrics.claude-haiku-4.5.requests.cost` | number | required |
| `data.modelMetrics.claude-haiku-4.5.requests.count` | number | required |
| `data.modelMetrics.claude-haiku-4.5.tokenDetails.cache_read.tokenCount` | number | required |
| `data.modelMetrics.claude-haiku-4.5.tokenDetails.cache_write.tokenCount` | number | required |
| `data.modelMetrics.claude-haiku-4.5.tokenDetails.input.tokenCount` | number | required |
| `data.modelMetrics.claude-haiku-4.5.tokenDetails.output.tokenCount` | number | required |
| `data.modelMetrics.claude-haiku-4.5.totalNanoAiu` | number | required |
| `data.modelMetrics.claude-haiku-4.5.usage.cacheReadTokens` | number | required |
| `data.modelMetrics.claude-haiku-4.5.usage.cacheWriteTokens` | number | required |
| `data.modelMetrics.claude-haiku-4.5.usage.inputTokens` | number | required |
| `data.modelMetrics.claude-haiku-4.5.usage.outputTokens` | number | required |
| `data.modelMetrics.claude-haiku-4.5.usage.reasoningTokens` | number | required |
| `data.sessionStartTime` | number | required |
| `data.shutdownType` | string | required |
| `data.systemTokens` | number | required |
| `data.tokenDetails.cache_read.tokenCount` | number | required |
| `data.tokenDetails.cache_write.tokenCount` | number | required |
| `data.tokenDetails.input.tokenCount` | number | required |
| `data.tokenDetails.output.tokenCount` | number | required |
| `data.toolDefinitionsTokens` | number | required |
| `data.totalApiDurationMs` | number | required |
| `data.totalNanoAiu` | number | required |
| `data.totalPremiumRequests` | number | required |
| `id` | string | required |
| `parentId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `session.start` (1 record)

| field | shape | presence |
|---|---|---|
| `data.alreadyInUse` | bool | required |
| `data.context.cwd` | string | required |
| `data.contextTier` | null | required |
| `data.copilotVersion` | string | required |
| `data.producer` | string | required |
| `data.remoteSteerable` | bool | required |
| `data.sessionId` | string | required |
| `data.startTime` | string | required |
| `data.version` | number | required |
| `id` | string | required |
| `parentId` | null | required |
| `timestamp` | string | required |
| `type` | string | required |

## `subagent.completed` (1 record)

| field | shape | presence |
|---|---|---|
| `agentId` | string | required |
| `data.agentDisplayName` | string | required |
| `data.agentName` | string | required |
| `data.model` | string | required |
| `data.toolCallId` | string | required |
| `id` | string | required |
| `parentId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `subagent.started` (1 record)

| field | shape | presence |
|---|---|---|
| `agentId` | string | required |
| `data.agentDescription` | string | required |
| `data.agentDisplayName` | string | required |
| `data.agentName` | string | required |
| `data.model` | string | required |
| `data.toolCallId` | string | required |
| `id` | string | required |
| `parentId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `system.message` (1 record)

| field | shape | presence |
|---|---|---|
| `data.content` | string | required |
| `data.role` | string | required |
| `id` | string | required |
| `parentId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `tool.execution_complete` (11 records)

| field | shape | presence |
|---|---|---|
| `agentId` | string | optional (1/11) |
| `data.error.code` | string | optional (1/11) |
| `data.error.message` | string | optional (1/11) |
| `data.interactionId` | string | required |
| `data.model` | string | required |
| `data.parentToolCallId` | string | optional (1/11) |
| `data.result.content` | string | optional (10/11) |
| `data.result.detailedContent` | string | optional (10/11) |
| `data.success` | bool | required |
| `data.toolCallId` | string | required |
| `data.toolTelemetry.metrics.commandTimeout` | number | optional (1/11) |
| `data.toolTelemetry.metrics.file_count` | number | optional (1/11) |
| `data.toolTelemetry.metrics.linesAdded` | number | optional (3/11) |
| `data.toolTelemetry.metrics.linesRemoved` | number | optional (3/11) |
| `data.toolTelemetry.metrics.numberOfToolCallsMadeByAgent` | number | optional (1/11) |
| `data.toolTelemetry.metrics.responseTokenLimit` | number | optional (7/11) |
| `data.toolTelemetry.metrics.response_length` | number | optional (1/11) |
| `data.toolTelemetry.metrics.resultForLlmLength` | number | optional (7/11) |
| `data.toolTelemetry.metrics.resultLength` | number | optional (7/11) |
| `data.toolTelemetry.metrics.result_length` | number | optional (2/11) |
| `data.toolTelemetry.properties.agent_name` | string | optional (1/11) |
| `data.toolTelemetry.properties.agent_type` | string | optional (1/11) |
| `data.toolTelemetry.properties.asyncOnlyShell` | string | optional (1/11) |
| `data.toolTelemetry.properties.codeBlocks` | string | optional (3/11) |
| `data.toolTelemetry.properties.command` | string | optional (7/11) |
| `data.toolTelemetry.properties.customTimeout` | string | optional (1/11) |
| `data.toolTelemetry.properties.detached` | string | optional (1/11) |
| `data.toolTelemetry.properties.executionMode` | string | optional (1/11) |
| `data.toolTelemetry.properties.execution_mode` | string | optional (1/11) |
| `data.toolTelemetry.properties.fileExtension` | string | optional (7/11) |
| `data.toolTelemetry.properties.files_found` | string | optional (1/11) |
| `data.toolTelemetry.properties.inputs` | string | optional (7/11) |
| `data.toolTelemetry.properties.languageId` | string | optional (3/11) |
| `data.toolTelemetry.properties.matches_found` | string | optional (1/11) |
| `data.toolTelemetry.properties.options` | string | optional (7/11) |
| `data.toolTelemetry.properties.output_mode` | string | optional (1/11) |
| `data.toolTelemetry.properties.prompt_length` | string | optional (1/11) |
| `data.toolTelemetry.properties.resolvedPathAgainstCwd` | string | optional (7/11) |
| `data.toolTelemetry.properties.response_length` | string | optional (1/11) |
| `data.toolTelemetry.properties.sandboxApplied` | string | optional (3/11) |
| `data.toolTelemetry.properties.sandboxOptOutRequested` | string | optional (1/11) |
| `data.toolTelemetry.properties.viewType` | string | optional (4/11) |
| `data.toolTelemetry.restrictedProperties` | object | optional (5/11) |
| `data.toolTelemetry.restrictedProperties.agent_name` | string | optional (1/11) |
| `data.toolTelemetry.restrictedProperties.filePaths` | string | optional (3/11) |
| `data.toolTelemetry.restrictedProperties.path` | string | optional (1/11) |
| `data.toolTelemetry.restrictedProperties.pattern` | string | optional (1/11) |
| `data.turnId` | string | required |
| `id` | string | required |
| `parentId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `tool.execution_start` (11 records)

| field | shape | presence |
|---|---|---|
| `agentId` | string | optional (1/11) |
| `data.arguments.agent_type` | string | optional (1/11) |
| `data.arguments.command` | string | optional (1/11) |
| `data.arguments.description` | string | optional (2/11) |
| `data.arguments.file_text` | string | optional (2/11) |
| `data.arguments.mode` | string | optional (1/11) |
| `data.arguments.name` | string | optional (1/11) |
| `data.arguments.new_str` | string | optional (1/11) |
| `data.arguments.old_str` | string | optional (1/11) |
| `data.arguments.path` | string | optional (7/11) |
| `data.arguments.pattern` | string | optional (2/11) |
| `data.arguments.prompt` | string | optional (1/11) |
| `data.model` | string | required |
| `data.parentToolCallId` | string | optional (1/11) |
| `data.shellToolInfo.hasWriteFileRedirection` | bool | optional (1/11) |
| `data.shellToolInfo.possiblePaths[]` | string | optional (1/11) |
| `data.toolCallId` | string | required |
| `data.toolName` | string | required |
| `data.turnId` | string | required |
| `id` | string | required |
| `parentId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |

## `user.message` (1 record)

| field | shape | presence |
|---|---|---|
| `data.attachments` | array | required |
| `data.content` | string | required |
| `data.delivery` | string | required |
| `data.interactionId` | string | required |
| `data.parentAgentTaskId` | string | required |
| `data.supportedNativeDocumentMimeTypes` | array | required |
| `data.transformedContent` | string | required |
| `id` | string | required |
| `parentId` | string | required |
| `timestamp` | string | required |
| `type` | string | required |
