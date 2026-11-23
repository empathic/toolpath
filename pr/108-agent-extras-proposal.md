# design: an agent-tagged extras bag for what only one agent can hold (PROPOSAL, nothing built)

Status: proposal for Bobby, docs only · Base: pr/115 · Asked by controller-core

## The problem
After pr/114/107, same-agent round trips still lose values the agent's format holds and the IR has no slot for: opencode `state.metadata.*` (13 paths) and session `summary_*` (3) = 16 of 88; gemini `toolCalls[].resultDisplay.*` (~40 of 86) and `agentId`.
The writer cannot give back what the IR never kept.

## Proposal
- `ToolResult.agent_extras: Option<AgentExtras>` and `ConversationView.agent_extras: Option<AgentExtras>` (session level), where `AgentExtras { provider: String, value: serde_json::Value }`, `provider` = the agent that produced it ("opencode", "gemini").
- Reader (the agent's own adapter) puts the raw JSON it has no typed home for there. Only that agent's writer reads it back; every other writer ignores it (and the contract declares it dropped, like `thinking_signature.*`).
- In the document: under the step's `structural`, as `agent_extras.{provider,value}`; unknown to the kind, so it needs a place in the kind's schema (an optional object) or it rides as an undeclared extra.

## How it differs from `thinking_signature`
`thinking_signature` is bound to its text by a hash (`text_sha256`): a writer returns it only when the thinking still hashes to it, so a stale signature is never replayed. A free bag has no such binding: if another tool edits the step (the tool's output, the diff), the bag still describes the old result and the agent's writer would write contradicting metadata.
Mitigation: bind it the same way (hash of the result content it was recorded with; the writer returns the bag only when the content still hashes to it). Cost: an edited result silently loses the extras, which is the correct failure.

## Risks
- Size: opencode `filediff.patch`/`preview` and gemini `resultDisplay` (file contents, ANSI cells) duplicate data already in the step; the document grows (gemini's resultDisplay is the larger share of the recording).
- Privacy: these fields carry paths and content the typed fields were scrubbed of; `resultDisplay.originalContent/newContent` is whole files. The leak/redaction rules (`toolpath-goldens::leaks`) must read the bag; a re-share of a document would carry it to Pathbase.
- Kind: a new optional object in agent-coding-session needs the owners (Ben/Robert/Alex) and a version; I do not propose adding it ahead of v1.2.0.

## Who would use it
opencode (tool metadata, session summary): yes, 16 paths. gemini (`resultDisplay`, `agentId`): yes, ~41. claude/codex/pi/copilot/cursor: not needed for today's losses; codex's remaining 31 (token_count, exec output) are writer enrichment, not this.

## The alternative: declare the 16 (and the gemini ~41)
Cost: nothing built, no schema, no size or privacy growth. Each path gets a contract entry with the reason ("the IR has no slot; the agent computes it"), same-agent round trips are not byte-faithful for these fields, and an opencode/gemini session resumed through toolpath loses its tool-result metadata (UI details such as the rendered diff; the model's history is unaffected, because the model sees the output text).

## Recommendation
Declare now. The lost fields are UI and bookkeeping metadata, not model-visible history, and the free bag carries the size, privacy and kind-version costs above for no change in what the model sees. Revisit with the hash-bound variant if "same-agent drops nothing" must be literal for opencode/gemini; build it for opencode first (16 paths, small values) and leave gemini `resultDisplay` declared.
