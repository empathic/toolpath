# Antigravity CLI conversation format

Reference for the on-disk state written by Google's Antigravity CLI
(`agy`, <https://github.com/google-antigravity/antigravity-cli>), the
product Google points individual Gemini CLI users at since 2026-06-18.
It is **not** a Gemini CLI revision: a different binary (Go, closed
source, the Windsurf/Codeium "Cascade" engine under
`google3/third_party/jetski`), a different store (protobuf and SQLite,
not JSON), a different id (conversation UUID, not `session-*` stem),
flat rather than per project. Only the `~/.gemini` root is shared with
`toolpath-gemini`; nothing in `gemini.md` applies to the records below.

**Sourcing posture, 2026-10-15, updated 2026-11-25.** Originally not
sample-grounded (`agy` needs sign-in before it will run a turn, and on
this Mac the OAuth step opens a real, visible Chrome window with no way
to suppress it — so `agy` cannot run headlessly on the host at all).
Unblocked 2026-11-25 by running `agy` 1.4.8 in a disposable Linux VM
(`planar-lima`, never the host) with `"modelProvider": "gemini"` in
`settings.json` plus an AI Studio `GEMINI_API_KEY` — no sign-in, no
browser. Three real conversations were captured this way (plain text,
one tool call, one leg of a `--conversation` resume) and are now the
`toolpath-antigravity` adapter's test fixtures
(`test-fixtures/antigravity/SOURCE.md` has the exact commands and
provenance). Every claim below is still tagged:

- **observed** — read from a real run of `agy` (1.1.13 for the
  directory-tree/pre-auth claims below, 1.4.8 for everything confirmed
  against the three captured conversations — field numbers for the
  messages `toolpath-antigravity` reads are cited against those exact
  samples in `crates/toolpath-antigravity/src/proto.rs` and
  `test-fixtures/antigravity/SOURCE.md`, not asserted from this doc
  alone);
- **binary** — recovered from the `agy` 1.1.13 Mach-O: embedded
  protobuf `FileDescriptorProto`s (so the field names and numbers are
  exact), GORM struct tags, SQL fragments and log format strings;
- **docs** — <https://antigravity.google/docs/cli/*> or the upstream
  `CHANGELOG.md`;
- **3p** — a third-party reader (getagentseal/codeburn
  `docs/providers/antigravity.md`, MIT); unverified here;
- **inferred** — our inference, stated as such.

Upstream moves fast (1.0.0 → 1.3.6 between May and October 2026; nixpkgs
carries 1.2.16; the profile here has 1.1.13; the Linux build used to
capture real samples was 1.4.8). Field numbers for the messages this
doc and the adapter read were checked on both 1.1.13 (binary) and 1.4.8
(real samples) wherever possible and matched exactly, with one
confirmed drift: `CortexStepUserInput`'s populated text field is 2 in
1.4.8, not the `query = 1` this doc's binary-recovered descriptor
names (see the `## conversations/<uuid>.db` section below). Re-verify
on the version a reader will actually run.

## Storage root

```
~/.gemini/
  config/                              shared with the Antigravity app (observed)
    config.json                        {"userSettings":{"remoteControlHostname":…}}
    mcp_config.json                    MCP servers, shared with the app
    projects/<project-id>.json         {"id","name","projectResources"}; default-cli-project
    .migrated
  antigravity-cli/                     ← the CLI's own tree (observed)
    conversation_summaries.db          SQLite index of every conversation (+ -wal, -shm)
    conversations/                     one file per conversation (see below)
      <uuid>.pb                        protobuf store (older; "ProtoStore")       (binary, 3p)
      <uuid>.db                        SQLite store (current; "SQLiteStore")      (docs 1.0.4, binary)
      <uuid>.db-wal / .db-shm          WAL side files, scanned by /resume          (docs 1.0.5)
      implicit/                        implicit (background) trajectories          (3p; binary has implicit.UserImplicitTrajectoryManager)
    cache/
      default_project_id.txt           "default-cli-project"                       (observed)
      projects.json                    workspace dir → project id                  (docs 1.0.4)
      last_conversations.json          absolute cwd → last conversation UUID       (docs; drives `agy -c`)
    jetski_state.pbtxt                 text proto: onboarding steps, installation_uuid, migrations (observed)
    installation_id                    UUID                                        (observed)
    settings.json                      CLI settings; created on first change      (docs)
    history.jsonl                      prompt history                              (binary string; 3p)
    log/cli-<YYYYMMDD_HHMMSS>.log      per-run log; cli.log symlinks the latest   (observed)
    crashes/crash_<pid>_<uuid>.log     (observed, empty)
    knowledge/knowledge.lock, brain/   knowledge-base state                        (observed, empty)
    builtin/skills/…                   bundled skills incl. antigravity_guide/references/{cli,app,sdk,ide}.md (observed)
```

There is **no per-project slot**: conversations are flat under
`conversations/` and tied to a workspace through
`conversation_summaries.workspace_uris`, `cache/last_conversations.json`
and the trajectory's `metadata.workspace_uris` (observed schema; binary).
The `~/.gemini/tmp/<slot>/chats/` tree that Gemini CLI writes is never
read by `agy`; `agy plugin import gemini` imports extensions, skills and
settings, not chats (observed `agy plugin --help`; docs).

`--gemini_dir=<path>` exists as a flag string in the binary (binary); it
is not in `agy --help`. `HOME` override is what we used to relocate the
tree (observed).

## `conversation_summaries.db` (observed)

Created on first run, before any conversation. One table:

```sql
CREATE TABLE `conversation_summaries` (
  `conversation_id` text,
  `title` text NOT NULL DEFAULT "",
  `preview` text NOT NULL DEFAULT "",
  `step_count` integer NOT NULL DEFAULT 0,
  `last_modified_time` datetime NOT NULL,
  `workspace_uris` text NOT NULL,
  `status` text NOT NULL DEFAULT "",
  `source` text NOT NULL DEFAULT "",
  `project_id` text NOT NULL DEFAULT "",
  `agent_name` text NOT NULL DEFAULT "",
  `parent_conversation_id` text NOT NULL DEFAULT "",
  `nesting_depth` integer NOT NULL DEFAULT 0,
  `battle_id` text NOT NULL DEFAULT "",
  `winning_conversation_id` text NOT NULL DEFAULT "",
  `not_fully_idle` numeric NOT NULL DEFAULT false,
  `killed` numeric NOT NULL DEFAULT false,
  `last_user_input_time` datetime NOT NULL,
  `last_user_input_step_index` integer NOT NULL DEFAULT -1,
  `app_data_dir` text NOT NULL DEFAULT "",
  PRIMARY KEY (`conversation_id`)
);
CREATE INDEX `idx_conversation_summaries_last_user_input_time` ON `conversation_summaries`(`last_user_input_time`);
CREATE INDEX `idx_conversation_summaries_last_modified_time` ON `conversation_summaries`(`last_modified_time`);
```

Backtick quoting and `numeric` booleans are GORM's SQLite dialect; the
binary links `gorm.io` and `PRAGMA user_version` is read on open
(`summary_store: read user_version`). It is a **cache**: the log says
`ListConversations` reads the `conversations/` directory and the
changelog (1.2.2) calls it the "summary cache", so a reader must not
treat a missing row as a missing conversation. `workspace_uris` is a
text column; its encoding (JSON array of `file://` URIs is the obvious
guess) is **inferred**, not observed. `parent_conversation_id` /
`nesting_depth` are how sub-agent conversations hang off their parent
(binary: `CortexTrajectoryMetadata.parent_conversation_id`,
`nesting_depth`).

## `conversations/<uuid>.pb` — the protobuf store (binary; 3p)

The `trajectorystore.ProtoStore` (`proto_store.go`) saves, lists and
`loadAndMigrate`s whole files; the `.pb` extension and the
"`%s.pb`" format string are in the binary; codeburn reports `.pb` as the
older of the two layouts. The message is **inferred** to be
`gemini_coder.Conversation` — the only top-level message in the
trajectory proto that carries a `conversation_id` — serialized with the
standard wire format. Not confirmed on a sample.

The recovered descriptor (`third_party/gemini_coder/proto/trajectory.proto`,
package `gemini_coder`; exact names and numbers, binary):

```proto
message Conversation {
  string conversation_id = 1;
  Trajectory trajectory = 2;
  ConversationState state = 3;
}
message ConversationState {
  string trajectory_id = 1;
  ExecutionStatus status = 2;
  repeated Step staged_steps = 3;
  exa.cortex_pb.CascadeConfig execute_config = 4;
}
message Trajectory {
  string trajectory_id = 1;
  repeated Step steps = 2;
  repeated exa.cortex_pb.CortexStepGeneratorMetadata generator_metadata = 3;
  exa.cortex_pb.CortexTrajectoryType trajectory_type = 4;
  repeated exa.cortex_pb.CortexTrajectoryReference parent_references = 5;
  string cascade_id = 6;
  exa.cortex_pb.CortexTrajectoryMetadata metadata = 7;
  exa.cortex_pb.CortexTrajectorySource source = 8;      // …_CLI = 17, …_SDK = 15, …_SUBAGENT = 16
  repeated exa.cortex_pb.ExecutorMetadata executor_metadatas = 9;
  repeated exa.cortex_pb.BattleModeInfo battle_mode_infos = 10;
}
message Step {
  exa.cortex_pb.CortexStepType type = 1;
  exa.cortex_pb.CortexStepStatus status = 4;            // PENDING=1 RUNNING=2 DONE=3 INVALID=4 CLEARED=5 CANCELED=6 ERROR=7 GENERATING=8 WAITING=9 QUEUED=11 INTERRUPTED=12
  exa.cortex_pb.CortexStepMetadata metadata = 5;
  exa.cortex_pb.CortexErrorDetails error = 31;
  exa.cortex_pb.TrajectoryPermissions permissions = 133;
  TaskDetails task_details = 148;
  repeated google.protobuf.Any attachments = 155;
  // …then one body per step type (a oneof in all but name), ~130 members. The ones toolpath cares about:
  exa.cortex_pb.CortexStepUserInput user_input = 19;
  exa.cortex_pb.CortexStepPlannerResponse planner_response = 20;
  exa.cortex_pb.CortexStepRunCommand run_command = 28;
  exa.cortex_pb.CortexStepCodeAction code_action = 10;
  exa.cortex_pb.CortexStepWriteToFile write_to_file = 23;
  exa.cortex_pb.CortexStepFileChange file_change = 98;
  exa.cortex_pb.CortexStepViewFile view_file = 14;
  exa.cortex_pb.CortexStepListDirectory list_directory = 15;
  exa.cortex_pb.CortexStepGrepSearch grep_search = 13;
  exa.cortex_pb.CortexStepFind find = 34;
  exa.cortex_pb.CortexStepCodeSearch code_search = 85;
  exa.cortex_pb.CortexStepReadUrlContent read_url_content = 40;
  exa.cortex_pb.CortexStepSearchWeb search_web = 42;
  exa.cortex_pb.CortexStepMcpTool mcp_tool = 47;
  exa.cortex_pb.CortexStepInvokeSubagent invoke_subagent = 143;
  exa.cortex_pb.CortexStepAskQuestion ask_question = 154;
  exa.cortex_pb.CortexStepCheckpoint checkpoint = 30;    // compaction summary
  exa.cortex_pb.CortexStepSystemMessage system_message = 114;
  exa.cortex_pb.CortexStepErrorMessage error_message = 24;
  exa.cortex_pb.CortexStepFinish finish = 12;
  exa.cortex_pb.CortexStepTaskBoundary task_boundary = 93;
  exa.cortex_pb.CortexStepNotifyUser notify_user = 94;
  // … browser_*, notebook_*, applet_*, firebase/cloudsql, cider (internal Google) steps omitted
}
```

Step bodies and metadata that carry what a `ConversationView` needs
(`third_party/jetski/cortex_pb/cortex.proto`, package `exa.cortex_pb`;
`codeium_common.proto`, package `exa.codeium_common_pb`; binary):

```proto
message CortexStepMetadata {
  google.protobuf.Timestamp created_at = 1;
  google.protobuf.Timestamp viewable_at = 6;
  google.protobuf.Timestamp finished_generating_at = 7;
  google.protobuf.Timestamp completed_at = 8;
  google.protobuf.Timestamp started_at = 32;
  CortexStepSource source = 3;
  exa.codeium_common_pb.ChatToolCall tool_call = 4;      // the model's call that produced a tool step
  exa.codeium_common_pb.ModelUsageStats model_usage = 9;
  float model_cost = 10;
  exa.codeium_common_pb.Model generator_model = 11;      // enum, closed
  exa.codeium_common_pb.ModelOrAlias requested_model = 13;
  exa.codeium_common_pb.ModelInfo model_info = 24;
  string execution_id = 12;
  int32 tool_call_output_tokens = 23;
  string tool_summary = 30;
  string tool_action = 31;
  // credits, retries, hooks, snapshot and remote-control fields omitted
}
message ModelUsageStats {
  exa.codeium_common_pb.Model model = 1;
  uint64 input_tokens = 2;
  uint64 output_tokens = 3;
  uint64 cache_write_tokens = 4;
  uint64 cache_read_tokens = 5;
  APIProvider api_provider = 6;
  string message_id = 7;
  uint64 thinking_output_tokens = 9;
  uint64 response_output_tokens = 10;
  string response_id = 11;
  string provider_assigned_message_id = 12;
  string service_tier = 13;
  repeated ModalityTokenCount prompt_tokens_details = 14;      // per-modality splits
  repeated ModalityTokenCount cache_tokens_details = 15;
  repeated ModalityTokenCount candidates_tokens_details = 16;
  repeated ModalityTokenCount tool_use_prompt_tokens_details = 17;
}
message ChatToolCall {
  string id = 1;
  string name = 2;
  string arguments_json = 3;
  string invalid_json_str = 4;
  string invalid_json_err = 5;
  string thought_signature = 6;
  bytes thinking_signature = 7;
  string original_arguments_json = 8;
  string original_name = 9;
}
message CortexStepUserInput {
  string query = 1;
  string user_response = 2;
  repeated exa.codeium_common_pb.TextOrScopeItem items = 3;
  repeated exa.codeium_common_pb.ImageData images = 5;
  bool is_queued_message = 6;
  repeated exa.codeium_common_pb.Media media = 9;
  CascadeConfig user_config = 12;
  repeated string tags = 16;
  UserIdentity user_identity = 17;
}
message CortexStepPlannerResponse {
  string response = 1;
  string thinking = 3;
  string signature = 4;
  bool thinking_redacted = 5;
  string message_id = 6;
  repeated exa.codeium_common_pb.ChatToolCall tool_calls = 7;
  string modified_response = 8;
  google.protobuf.Duration thinking_duration = 11;
  exa.codeium_common_pb.StopReason stop_reason = 12;
  bytes thinking_signature = 14;
  string provider_assigned_message_id = 15;
  string raw_thinking = 16;
}
message CortexStepRunCommand {
  string command = 1;  repeated string args = 3;  string cwd = 2;
  string stdout = 4;   string stderr = 5;         int32 exit_code = 6;
  string command_line = 23;  string proposed_command_line = 25;
  bool user_rejected = 14;   bool blocking = 11;   string command_id = 13;
  RunCommandOutput combined_output = 21;  bool ran_in_sandbox = 34;  string shell_name = 31;
}
message CortexStepMcpTool {
  string server_name = 1;  exa.codeium_common_pb.ChatToolCall tool_call = 2;
  string result_string = 3;  string result_uri = 8;  bool user_rejected = 7;
  repeated ImageData images = 5;  repeated Media media = 6;
}
message CortexStepViewFile   { string absolute_path_uri = 1; uint32 start_line = 2; uint32 end_line = 3; string content = 4; uint32 num_lines = 11; uint32 num_bytes = 12; … }
message CortexStepWriteToFile{ string target_file_uri = 1; repeated string code_content = 2; exa.diff_action_pb.DiffBlock diff = 3; bool file_created = 4; … }
message CortexStepFileChange { string absolute_path_uri = 1; FileChangeType file_change_type = 2; repeated ReplacementChunk replacement_chunks = 3; exa.diff_action_pb.DiffBlock diff = 4; string instruction = 5; bool overwrite = 8; … }
message CortexStepInvokeSubagent { string subagent_name = 1; string prompt = 2; string conversation_id = 5; repeated SubagentSpec subagents = 9; repeated SubagentResult results = 10; bool task_mode = 11; }
message CortexStepCheckpoint { uint32 checkpoint_index = 1; string conversation_title = 10; string user_intent = 4; string session_summary = 5; string code_change_summary = 6; repeated uint32 included_step_indices = 3; … }
message CortexTrajectoryMetadata { repeated CortexWorkspaceMetadata workspaces = 1; google.protobuf.Timestamp created_at = 2; string parent_conversation_id = 5; string root_conversation_id = 6; repeated string workspace_uris = 7; string project_id = 18; int32 nesting_depth = 17; … }
```

How this maps onto the IR (observed against three real 1.4.8 samples —
`test-fixtures/antigravity/SOURCE.md`; `toolpath-antigravity/src/
reader.rs` cites every field number below against those exact bytes):

- A **turn** is a `user_input`-carrying step (`Step.type` **14**,
  observed — not the doc's Step-body field-number namespace; see the
  note two paragraphs down) followed by the steps up to the next one.
  User text is at `CortexStepUserInput` field **2** (`user_response`,
  observed) — **not** `query` (field 1), which is empty in every 1.4.8
  sample; fall back to `items[]` (field 3) when 2 is empty too.
- The **assistant message** is `planner_response.response` (field 1,
  confirmed exact). `planner_response` can appear on more than one
  step within a single logical turn: one generation decides to call a
  tool (response text often empty), a later one carries the final
  answer. Reasoning is `thinking` (field 3) or `raw_thinking` (field
  16) — not yet confirmed against a sample with visible thinking text.
- **Tool call and result live on the *same* step**, confirmed in the
  `tool` fixture (idx 2, `Step.type` **132**): `metadata.tool_call`
  (field 4 of `CortexStepMetadata`) carries the call
  (`id`/`name`/`arguments_json` = fields 1/2/3, all exact), and the
  step's own top-level field **140** carries the result for a
  `read_file` call specifically (`{1: {1: "Path", 2: <path>}, 2: {1:
  <result text>}}`). This **contradicts the earlier inferred claim**
  that results are a separate, call-id-correlated step "like Claude
  Code" — that was never sample-grounded; the real shape is simpler.
  Field 140 isn't in the binary-recovered descriptor table above, and
  no other tool type's result shape has been observed yet.
- **Token usage** is per model generation: `metadata.model_usage`
  (field 9) on a `planner_response`-carrying step.
  **`output_tokens` (field 3) already includes thinking** — confirmed
  exactly: in every sample, `thinking_output_tokens` (field 9) +
  `response_output_tokens` (field 10) sums to `output_tokens` (field
  3) precisely (e.g. 70 + 33 = 103 in one generation step of the
  `tool` fixture's dirty capture used to derive this mapping). No
  folding is needed, unlike `toolpath-gemini`'s older-format quirk.
  `cache_read_tokens`/`cache_write_tokens` (fields 5/4) and the
  per-modality `*_tokens_details` splits are unconfirmed against a
  sample that actually populates them.
- **Model** is an enum (`exa.codeium_common_pb.Model`, closed vocabulary,
  not extracted here) plus `model_info`; a reader needs the enum's names
  to print a model string.
- **Sub-agents** are separate conversations: `invoke_subagent.conversation_id`
  on the parent, `metadata.parent_conversation_id` / `nesting_depth` on
  the child, and a `conversation_summaries` row each.
- **Compaction** is a `checkpoint` step in the same trajectory, not a
  new file (`included_step_indices`, summaries).
- Timestamps are `google.protobuf.Timestamp` (UTC seconds + nanos);
  paths are `file://` URIs (`absolute_path_uri`, `workspace_uris`).

## `conversations/<uuid>.db` — the SQLite store (binary; docs; 3p)

"CLI's conversation format" since 1.0.4 (changelog), read and written by
`dbtrajectory.openOrCreateSQLiteStore` (`sqlite_store.go`) through GORM,
with `PRAGMA user_version` for schema versioning and WAL journaling
(`.db-wal`/`.db-shm` scanned by `/resume` since 1.0.5; checkpointed on
exit since 1.1.26). Tables, from the GORM `TableName` implementations and
struct tags in the binary (names are the Go receivers; the SQL table
names are **inferred** from codeburn plus the SQL fragments):

| Go record | SQL (inferred) | Columns (binary) | Content |
|---|---|---|---|
| `stepEntry` | `steps` | `idx, step_type, status, has_subtrajectory, metadata, error_details, permissions, task_details` (+ a `step_payload`; the SELECT `idx, step_payload` and `decompose step %d` suggest the body is stored apart from the header) | one row per `Step`; `metadata`/`error_details`/`permissions`/`task_details` are protobuf BLOBs of the corresponding `Step` fields |
| `genMetadata` | `gen_metadata` | `idx` (PK, no autoincrement), `data` BLOB, `size` | one row per `CortexStepGeneratorMetadata` (the per-generation model usage lives under its `chat_model`) |
| `executor_metadata` | `executor_metadata` | `idx, data, size` | `ExecutorMetadata[]` |
| `parentReference` | `parent_references` | `idx, data[, size]` | `CortexTrajectoryReference[]` |
| `battleModeInfoRecord` | `battle_mode_infos` | `cascade_id`, … | `BattleModeInfo[]` |
| `trajectoryMetadataBlob` | (single-row metadata table) | BLOB | `CortexTrajectoryMetadata` + ids/type/source |
| `coldRange` | (range table) | `busy, log, checkpointed` | which step ranges are "cold" (not loaded) |

codeburn reads `gen_metadata.data` as a protobuf whose field 1 is
`ChatModel` (token usage at its field 4, model config id at 19), field 4
the generation UUID and field 9 a timestamp, and `steps.metadata` field 4
as `ToolCallMetadata{call_id=1, tool_name=2, argumentsJson=3}` — which
matches `CortexStepMetadata.tool_call = 4` → `ChatToolCall{id=1, name=2,
arguments_json=3}` above exactly, so the BLOBs are the `cortex.proto`
messages, not a private serialization. "`.pb` conversations need the
live process" (codeburn) describes *their* RPC route, not a property of
the file: the ProtoStore reads `.pb` from disk itself.

## Headless mode (observed; docs)

`agy -p "<prompt>" --output-format json` prints one envelope. Observed
on the unauthenticated run:

```json
{"conversation_id":"","status":"ERROR","response":"","error":"authentication failed or timed out",
 "duration_seconds":0,"num_turns":0,
 "usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0}}
```

Docs add `structured_output` (with `--json-schema`), `status` values
`SUCCESS | ERROR | CANCELED | INTERRUPTED | INVALID | WAITING | RUNNING`,
exit 0 only on `SUCCESS` (exit 3 with an `AGY_ERROR` line on a mid-stream
model error since 1.2.10). `--output-format stream-json` is NDJSON with
`init`, `step_update` and a final `result` event, `step_type` as a closed
vocabulary, and `subagent_info{conversation_id, log_uri}` for delegated
sub-agents (docs; changelog 1.1.8). `--input-format stream-json` (1.1.15)
reads `{"event":"user","message":{"content":"…"}}` lines from stdin, one
turn each, in one conversation — a driver can keep the session open.

Before anything runs, print mode (1.1.13) prints the Google OAuth URL,
waits 60 s for the browser callback or a pasted code, then emits the
`ERROR` envelope above. Credentials go to the OS keyring (docs), so a
`HOME` override alone does not isolate sign-in state. An API-key path
exists for headless use: `"modelProvider": "gemini"` in `settings.json`
plus `GEMINI_API_KEY` (docs; the binary has the matching error strings),
and `AGY_ADC_AUTH` selects Application Default Credentials (binary;
changelog 1.3.3).

The run also started a "language server" child on two localhost ports
(gRPC + HTTP; observed log) — the Windsurf engine — and left a
`Library/Caches/ms-playwright-go/1.57.0` directory under `HOME`.

## Resume (docs; changelog)

- `agy --conversation <uuid>` — resume by id; also with `-p` (fixed in
  1.0.9 to print only the new response).
- `agy -c` / `--continue` — most recent conversation for the cwd, via
  `cache/last_conversations.json` (`{"<abs cwd>": "<uuid>", …}`); the
  CLI "verifies the conversation still exists" before loading, else
  starts fresh.
- `/resume` (`/switch`, `/conversation`) — picker over
  `conversation_summaries`, with an "Other" tab that imports conversations
  from the Antigravity app (1.3.2), and `@conversation:` mentions.
- There is no `--list-sessions`; `conversation_summaries.db` is the
  listing.

A conversation id is a UUID v4 (`a1b2c3d4-e5f6-7890-abcd-ef1234567890`,
docs); it is both the file stem and `Conversation.conversation_id`
(inferred). Whether `--conversation` accepts a prefix is unknown.

## Round-trip gotchas for a projector

Updated 2026-11-25 with how `toolpath_antigravity::project` actually
handles each one — "inferred" tags removed where a real sample or the
built projector settled the question; still open where it didn't.

1. **Three places must agree**: the `conversations/<uuid>.db` (or
   `.pb`), a `conversation_summaries` row, and
   `cache/last_conversations.json` if `-c` should find it. The summary
   is a cache, so a missing row may be tolerated — still untested.
   **Handling: write all three, from one id.**
   `AntigravityStore::write_to` creates the `.db` named by the
   conversation id, inserts the matching `conversation_summaries` row,
   and merges a `cache/last_conversations.json` entry keyed by the
   caller's `cwd` — never clobbering other projects' entries. Tested
   (`project::tests::write_to_keeps_three_stores_consistent`); not yet
   checked against `agy -c` itself (needs the live check).
2. **Schema-sensitive reader.** 1.2.2 fixed "deleted conversations being
   recreated as empty, schema-less SQLite database files"; a file whose
   `user_version` or tables do not match what `openOrCreateSQLiteStore`
   expects will be migrated or rejected by a closed binary we cannot
   patch. **Handling: match the real schema byte-for-byte** (table and
   column names, `PRAGMA user_version = 1`) — copied from the real
   `.schema` dump in `SOURCE.md`, not reconstructed from the doc's
   earlier inference. Whether a closed-binary migration path still
   rejects it for some other reason is a live-check item.
3. **Closed enums.** `ModelUsageStats.model` is an
   `exa.codeium_common_pb.Model` enum value; a projected session must
   pick a value the binary knows, and the enum changes per release.
   **Handling: carry through when the source is agy's own, synthesize
   otherwise — and say so.** `AntigravityProjector` writes
   `SYNTHESIZED_MODEL_ENUM` (`1073`, observed on a real 1.4.8
   `gemini-3.6-flash-low` run) for every generation step, including
   agy→agy round trips (the source `Turn` doesn't carry the original
   enum value through — see gotcha 5). A foreign source has no agy
   enum value to carry through in the first place, so it gets the same
   synthesized constant. Whether a stale or made-up value is silently
   accepted, migrated, or rejected by the closed binary on load is an
   explicit live-check item, not assumed either way.
4. **Signatures.** `CortexStepPlannerResponse.thinking_signature` and
   `ChatToolCall.thought_signature` are provider-issued; a projector
   cannot forge them. **Handling: omit, never synthesize.** Neither
   field is written by `AntigravityProjector` — there is no placeholder
   value that would pass whatever validation the API does. Whether
   `agy` resends/re-checks them on resume (and fails on a missing one,
   versus tolerating it for injected history it didn't generate itself)
   is unknown and is the single most load-bearing live-check question
   for the tool-call-bearing fixture.
5. **Version drift**, and a related gap this adapter's own writer
   introduces: `.pb` → `.db` happened within the 1.0.x line; the `Step`
   message already has ~130 body fields; `loadAndMigrate` exists for a
   reason. Pin the version a fixture was captured on (1.4.8 here).
   Separately: this crate's **reader never decodes the model enum,
   message ids, or trajectory/cascade ids into anything the projector
   can read back** (see `provider.rs`'s `thinking_signature` doc
   comment on why they don't reach `Turn` at all) — so even a true
   agy→agy round trip through this crate synthesizes those fields
   fresh rather than carrying the original ones through. A future pass
   that threads them through `ToolInvocation`/`Turn` more precisely
   would shrink gotcha 3 and 4's synthesized surface, but needs a
   lossless-extras mechanism this version of `toolpath_convo::Turn`
   doesn't have (checked against #375 — no generic extras map exists
   on `Turn`; see `project.rs`'s module doc).

## What a toolpath adapter costs (built 2026-11-25)

- **Derive (read → `ConversationView`)**: built, as `toolpath-antigravity`
  (`crates/toolpath-antigravity/`). `paths.rs` is exactly as trivial as
  predicted (flat dir + summaries db + `last_conversations.json`); the
  reader is `rusqlite` over `steps` plus a small **hand-written,
  schema-free wire-format reader** (`src/proto.rs`) instead of a
  generated `prost` client — the on-disk schema is reverse-engineered
  from a closed binary and only partially known, so a hand-maintained
  `.proto` would claim more precision than we have, and would need
  re-pinning on every drift like the `CortexStepUserInput` one above.
  Every field number the reader uses is cited against real 1.4.8 samples
  in that file and in `test-fixtures/antigravity/SOURCE.md`. Known gaps:
  no per-project filtering (workspace_uris' encoding is still
  unconfirmed), tool *result* bodies are only decoded for `read_file`
  (the call itself decodes for every tool), and `ModelUsageStats.model`
  stays a raw enum integer (no name table). See the crate's README for
  the full list.
- **Project (IR → a session `agy` reopens)**: built at the library
  level (`toolpath_antigravity::project`), **not yet live-verified**.
  Correction to the paragraph this replaces: the OAuth-URL-open is a
  property of the *unauthenticated* host, not of `--conversation <uuid>
  -p` itself — once `"modelProvider": "gemini"` plus a real
  `GEMINI_API_KEY` is in place (as it was for the three captured
  fixtures), every run went straight to the model with no browser
  opened at all. So the write path's live-check is blocked by *auth*
  availability, not by the OAuth flow:
  - **Route 1 (render as the first `stream-json` user event)**: built
    and offline-tested (`toolpath_antigravity::render`). Version-proof
    by construction — every byte `agy` holds afterwards is one `agy`
    wrote itself — but lossy (no replayable tool-call records).
  - **Route 2 (SDK's `initial_trajectory`)**: assessed, not built.
    `google/antigravity/proto/localharness.proto`'s `HarnessConfig`
    does have `bytes initial_trajectory = 11` and a
    `SessionContinuationMode` enum (`CREATE_OR_RESUME = 2`), but
    neither is documented, neither has an example use, and nothing in
    the SDK repo ties the bytes to `gemini_coder.Trajectory` (the
    on-disk message) rather than `genai.Step` (the SDK's own, separate
    schema) or something else entirely. The SDK's only *documented*
    resume path is `AgentConfig.conversation_id` + `save_dir`
    (SDK-managed storage), not injected bytes. Not pursued further —
    "relying on an undocumented wire field of unknown schema" is a
    worse bet than the store projector below.
  - **The store projector** (`AntigravityProjector`,
    `AntigravityStore`): built, unit- and round-trip-tested against all
    three real fixtures plus a cross-harness Claude fixture (see the
    crate's `tests/projection_roundtrip.rs`). Not yet checked against a
    live `agy --conversation <uuid> -p "…"` — see the Pitfalls section
    below for what's synthesized, what's omitted, and why.
  - **Live-check status, both routes**: the AI Studio free-tier key
    used to capture the fixtures above hit its daily quota (20
    requests/day, `gemini-3.6-flash`) partway through capture, so no
    live check — Route 1, the projector, or a cloned-store probe — can
    run until the quota resets (~13:00 UTC on 2026-11-26, per the 429's
    `retryDelay`) or a billing-enabled key is used.
  - One mechanical simplification worth recording for whoever runs the
    check: conversation ids are fixed-length UUIDs, so cloning a real
    `.db` under a new id is a **textual find/replace** of the old UUID
    string for the new one
  (same length ⇒ every protobuf length prefix that embeds it stays
  valid) — no re-encoding needed for that part. What still needs a live
  turn to check is whether `agy` accepts the result past that point. See
  the toolpath PR description for the exact probes attempted and their
  output.

## References

- Repo (README, CHANGELOG, examples only; `licenseInfo: null`):
  <https://github.com/google-antigravity/antigravity-cli>
- Docs: <https://antigravity.google/docs/cli/overview>,
  <https://antigravity.google/docs/cli/install>,
  <https://antigravity.google/docs/cli/headless>,
  <https://antigravity.google/docs/cli/commands/resume>
- Gemini CLI consumer deprecation (2026-06-18):
  <https://developers.google.com/gemini-code-assist/docs/deprecations/code-assist-individuals>
- Python SDK (Apache-2.0, protos for the SDK wire, not the store):
  <https://github.com/google-antigravity/antigravity-sdk-python>
- Third-party reader: <https://github.com/getagentseal/codeburn>
  (`docs/providers/antigravity.md`)
- Local evidence: `agy --help`, `agy plugin --help`, `agy changelog`,
  `agy -p … --output-format json` under a throwaway `HOME`, and
  descriptors recovered from the 1.1.13 binary (see the spike notes in
  the scratchpad review, 2026-10-15).
