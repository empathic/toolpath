# Qwen Code session format (draft)

Reference for the on-disk format produced by Alibaba's
[Qwen Code](https://github.com/QwenLM/qwen-code) CLI (Apache-2.0, a
Gemini CLI fork), as intended input for a future `toolpath-qwen` provider.
Compiled from:

1. **Observed**: `qwen` `0.23.4` from `nixpkgs#qwen-code`, run as
   `qwen -o json "say hi"` under a temporary `HOME` on 2026-10-15. With no
   auth type configured it exits with a JSON `result` carrying a
   `session_id`, and writes **no** chat file — only `~/.qwen/` bootstrap
   files (`installation_id`, `extension-store/`, `extensions/`, `skills/`).
2. **Read**: upstream source at tag `v0.23.4`:
   `packages/core/src/config/storage.ts`,
   `packages/core/src/utils/paths.ts`,
   `packages/core/src/services/chatRecordingService.ts` (the `ChatRecord`
   type), plus `qwen --help`, `qwen sessions --help`.
3. Field-level shapes of `message`/`usageMetadata` are Gemini SDK types
   (`Content`, `GenerateContentResponseUsageMetadata`) and transfer from
   `docs/agents/formats/gemini.md` / `toolpath-gemini`.

No record has been observed on this machine. Date: **2026-10-15**.

## Install route and version

- `nix shell nixpkgs#qwen-code` → `qwen 0.23.4` (Node bundle under
  `share/qwen-code/`; honours `HOME`, observed).
- Vendor: `npm i -g @qwen-code/qwen-code`.
- Isolation: `QWEN_HOME=<dir>` moves config **and** runtime data;
  `QWEN_RUNTIME_DIR=<dir>` moves only runtime output (sessions, temp,
  debug logs) (read: `storage.ts:169-200`).

## Storage root

```
$QWEN_HOME (default ~/.qwen/)
  settings.json, oauth_creds.json, installation_id, …
  projects/                                   PROJECT_DIR_NAME (storage.ts:20)
    <sanitizeCwd(projectRoot)>/               every non-[a-zA-Z0-9] char → "-"  (paths.ts:388)
      chats/
        <sessionId>.jsonl                     one ChatRecord per line, append-only
        <sessionId>.runtime.json              live-status sidecar for external observers (storage.ts:734)
      workflows/                              workflow run snapshots/journals
  tmp/<sha256(projectRoot)>/                  getProjectTempDir: tool-results/, checkpoints/, shell_history
```

The survey's alternative `~/.qwen/sessions/<hash>/<id>.json` does not
exist in 0.23.4 (read).

### Project id

`sanitizeCwd` replaces every character outside `[a-zA-Z0-9]` with `-`
(lowercased first on Windows), so

```
/Users/ben/empathic/oss/toolpath  ──▶  -Users-ben-empathic-oss-toolpath
```

Unlike Pi's encoder it also maps `.`, `_` and ` ` to `-`, so it is lossy
for more paths; there is no `--…--` wrapper. The sibling `tmp/` tree uses
`sha256(projectRoot)` instead (`getProjectHash`).

### Session id

UUIDv4 (observed in the headless result; `--session-id <id>` lets the
caller choose). The chat file is created with `wx` (exclusive) on first
write and then appended (read: `chatRecordingService.ts:1130-1145`).

## `ChatRecord` lines

From `chatRecordingService.ts:279-422`:

```ts
{
  uuid: string,                     // this record
  parentUuid: string | null,        // previous record; null for the root
  sessionId: string,
  timestamp: string,                // ISO-8601
  type: "user" | "assistant" | "tool_result" | "system",
  subtype?: "chat_compression" | "slash_command" | "ui_telemetry" | "at_command"
          | "attribution_snapshot" | "notification" | "cron" | "mid_turn_user_message"
          | "custom_title" | "parent_session" | "session_source" | "session_model"
          | "rewind" | "agent_bootstrap" | "agent_launch_prompt" | "agent_retry"
          | "agent_session_ready" | "file_history_snapshot" | "user_text_elements"
          | "session_artifact_event" | "session_artifact_snapshot" | "session_sources_snapshot"
          | "branch_checkpoint" | "goal_state" | "goal_runtime" | "realtime_message" | "turn_result",
  provenance?: "real_user" | "assistant_output" | "tool_result" | "goal_control" | "goal_runtime" | "system",
  goalContext?: { goalId, revision, turnId },
  cwd: string,
  version: string,                  // CLI version
  gitBranch?: string,
  daemonPromptId?: string,

  message?: Content,                // Gemini { role: "user"|"model", parts: [{text}|{functionCall}|{functionResponse}|{thought…}] }
  usageMetadata?: GenerateContentResponseUsageMetadata,   // promptTokenCount, candidatesTokenCount, thoughtsTokenCount, cachedContentTokenCount, totalTokenCount
  model?: string,
  contextWindowSize?: number,
  toolCallResult?: Partial<ToolCallResponseInfo> & { status?: Status },  // UI-enriched result for type: "tool_result"
  systemPayload?: …,                // per-subtype payload for type: "system"

  agentId?, agentName?, agentColor?, isSidechain?, agentRunId?, agentRound?,   // subagent records
  externalInputKind?: "message" | "notification",
  forkedFrom?: { sessionId, messageUuid }   // set on every record of a /branch copy
}
```

### Reconstructed example

```jsonl
{"uuid":"a1","parentUuid":null,"sessionId":"04b00b54-d411-44aa-8797-ee0b4fee9d18","timestamp":"2026-10-15T19:20:00.000Z","type":"user","cwd":"/Users/me/proj","version":"0.23.4","gitBranch":"main","message":{"role":"user","parts":[{"text":"list the files"}]}}
{"uuid":"a2","parentUuid":"a1","sessionId":"04b00b54-…","timestamp":"2026-10-15T19:20:01.200Z","type":"assistant","cwd":"/Users/me/proj","version":"0.23.4","model":"qwen3-coder-plus","message":{"role":"model","parts":[{"thought":true,"text":"…"},{"functionCall":{"id":"call-1","name":"run_shell_command","args":{"command":"ls"}}}]},"usageMetadata":{"promptTokenCount":1200,"candidatesTokenCount":40,"thoughtsTokenCount":12,"cachedContentTokenCount":0,"totalTokenCount":1252}}
{"uuid":"a3","parentUuid":"a2","sessionId":"04b00b54-…","timestamp":"2026-10-15T19:20:01.400Z","type":"tool_result","cwd":"/Users/me/proj","version":"0.23.4","message":{"role":"user","parts":[{"functionResponse":{"id":"call-1","name":"run_shell_command","response":{"output":"a.rs\nb.rs\n"}}}]},"toolCallResult":{"callId":"call-1","status":"success"}}
{"uuid":"a4","parentUuid":"a3","sessionId":"04b00b54-…","timestamp":"2026-10-15T19:20:02.000Z","type":"assistant","cwd":"/Users/me/proj","version":"0.23.4","model":"qwen3-coder-plus","message":{"role":"model","parts":[{"text":"Two files: a.rs and b.rs."}]},"usageMetadata":{"promptTokenCount":1300,"candidatesTokenCount":15,"totalTokenCount":1315}}
```

(Reconstructed from the type; part shapes follow the Gemini SDK as in
`gemini.md`. Exact tool names, the `functionResponse.response` envelope
and whether `thought` parts carry `thoughtSignature` are to be confirmed
on a real file.)

Tool call ↔ result: the call is a `functionCall` part inside an
`assistant` record's `message`; the result is a separate `tool_result`
record whose `message.parts[].functionResponse.id` (or
`toolCallResult.callId`) matches. Pi-style separate entries, Gemini-style
parts.

## Session resolution / resume

- `qwen -r/--resume <id>` (picker without an id), `-c/--continue` (most
  recent for the project), `--session-id <id>` (pin a new id),
  `--fork-session` with either; all work with a positional prompt in
  headless mode (read: `--help`).
- `--chat-recording false` disables writing; then `--continue/--resume`
  cannot work (read: `--help`). Default appears to be on (the headless
  run was going to record; **inferred**).
- Resume reads `projects/<slug>/chats/<id>.jsonl` for the **current
  project's** slug and rebuilds `Content[]` from the `message` fields,
  honouring `system` records (`chat_compression`, `rewind`,
  `branch_checkpoint`) (read: `ChatRecord` doc comments;
  `sessionStorageUtils.ts` not read in full).
- `qwen sessions list` / `sessions ps` (running) / `sessions controllers`.

## Headless mode

`qwen [-o text|json|stream-json] [--include-partial-messages]
[--json-schema <schema|@file>] [--max-session-turns N] [--max-wall-time
30s] [--max-tool-calls N] [--approval-mode plan|default|auto-edit|auto|yolo]
[-y] [--auth-type openai|openai-responses|anthropic|qwen-oauth|gemini|vertex-ai]
"<prompt>"`; `--input-format stream-json` for multi-turn drive;
`--json-fd`/`--json-file` for dual output; `--acp`; `qwen serve` daemon.
Observed failure result (JSON array, one `result` object):

```json
[{"type":"result","subtype":"error_during_execution","uuid":"326bcf8a-…","session_id":"04b00b54-…","is_error":true,"duration_ms":0,"duration_api_ms":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0},"permission_denials":[],"error":{"message":"No auth type is selected. …"}}]
```

## What a `toolpath-qwen` provider needs

| Qwen construct | `toolpath-convo` mapping |
|---|---|
| `sessionId` | `ConversationView.id` |
| first record's `cwd`, `gitBranch`, `version` | `SessionBase { working_dir, vcs_branch }`, `ProducerInfo { name: "qwen-code", version }` |
| `uuid` / `parentUuid` | `Turn.id` / `Turn.parent_id` |
| `type: user` | `Turn { role: User }` (text parts joined) |
| `type: assistant` | `Turn { role: Assistant, model }`; `thought` parts → `thinking`; `text` → `text`; `functionCall` → `tool_uses[]` |
| `type: tool_result` | `ToolInvocation.result` by `functionResponse.id`; `toolCallResult.status` → `is_error` |
| `usageMetadata` | `Turn.token_usage` (per call; `thoughtsTokenCount` folded into output as for Gemini) |
| `type: system` + `subtype` | `ConversationEvent { event_type: "system." + subtype, data: systemPayload }`; `chat_compression` → compaction marker, `rewind` → drop later turns from the active chain |
| `isSidechain` / `agentId` records | `DelegatedWork` turns grouped by `agentRunId` |
| `forkedFrom` | predecessor link |

Derive cost: **easy–medium**: reuse `toolpath-gemini`'s part modelling;
new work is the JSONL envelope, the uuid chain and ~25 system subtypes
(preserve unknown ones verbatim). Project cost: **medium**: write
`projects/<sanitizeCwd(cwd)>/chats/<id>.jsonl` with a consistent chain
and `version`; optionally the `.runtime.json` sidecar (shape not read).
The survey's "repeated uuids across resumed sessions" report should be
checked against `forkedFrom` semantics (a `/branch` copy keeps the same
`uuid`s by design — read: doc comment on `forkedFrom`).

## Open questions

1. A real `.jsonl` sample: part shapes, `toolCallResult` fields,
   `thoughtSignature`.
2. `<sessionId>.runtime.json` contents.
3. Whether chat recording is on by default in headless mode.
4. How `--resume <id>` finds a session recorded under a different
   project slug (same cwd, different realpath).

Needs credentials to go further: any `--auth-type openai --openai-api-key
… --openai-base-url …` endpoint (an OpenAI-compatible local server would
do) for one recorded session.

## References

- Storage paths: <https://github.com/QwenLM/qwen-code/blob/v0.23.4/packages/core/src/config/storage.ts>
- `sanitizeCwd` / `getProjectHash`: <https://github.com/QwenLM/qwen-code/blob/v0.23.4/packages/core/src/utils/paths.ts>
- `ChatRecord`: <https://github.com/QwenLM/qwen-code/blob/v0.23.4/packages/core/src/services/chatRecordingService.ts>
- Local copies: `<scratchpad>/harness-qwen/src/`, temp home
  `<scratchpad>/harness-qwen/home/`
- Gemini part shapes: `docs/agents/formats/gemini.md`
