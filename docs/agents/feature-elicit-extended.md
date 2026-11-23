# Extended elicitation prompt: design record (for Bobby's review; nothing is recorded or run)

Status: DESIGN · 2026-10-21 · Draft prompt: `docs/agents/feature-elicit-extended.prompt.txt` · Ben's 10-step `feature-elicit.prompt.txt` is unchanged and stays the core recording.

## What the core recording misses (and the turn that exercises it)
| gap in the core recording | extended turn | what it should exercise |
|---|---|---|
| one user turn only | 1-10 are separate user turns on one session | multi-turn session shape, `parent` chains across turns, group ids per turn |
| a follow-up that changes direction | 4 (revert the rename, keep README) | undo as new edits; file-change `before`/`after` chains on the same file (the file-change-detail unexplained cluster; Bobby's count is 125 paths, which I have not recomputed) |
| an interrupted or retried turn | 5 (cancel after the first tool call, resend) | an aborted turn's partial tool call, dangling tool_use without a result, abort/retry events |
| a mid-session model switch | 6 (model flag for this turn only) | the model setting that pi and Copilot lose on share: Copilot `session.model_change` events (Bobby's count is 21 Copilot model-setting paths, not recomputed), pi `model_change` entries; claude/codex per-turn `model` |
| an image / attachment | 7 | image content parts, attachment events, what each reader keeps or drops |
| a multi-file edit + one larger file | 2 (three files in one request) and 3 (400-line file) | several file-change artifacts in one step, large `after` bodies, truncation, the file-change detail clusters |
| an MCP / extension tool call | 8 | namespaced tool names, tool categories for non-builtin tools, MCP events (Copilot `tools/list`) |
| a required sub-agent step | 9 (required, not "if you have one") | delegations: the delegation helper (`as_delegation`), `invoke_agent`/`task` naming, sub-agent turns |
| a long tail / compaction | NOT in this prompt | compaction is forced by Ben's #108 capture script per agent; this design reuses that forcing on top of the extended session rather than duplicating it |

## How each agent can be driven headless (to be VERIFIED against the pinned versions before any run)
Multi-turn needs one invocation per turn on the same session, or stdin for a persistent process. What the repo documents today (docs/agents/feature-elicit.md) is single-prompt only:
`claude -p`, `codex exec`, `copilot -p` (isolated `COPILOT_HOME`), `gemini -p`, `pi -p`, `opencode run`; Cursor is manual and `cursor-agent` is separate. I did NOT verify the multi-turn
and attachment flags below; they are my reading of each CLI's documented options and must be checked against the pinned binary (pinning.md) first.
| agent | next turn on the same session (believed) | model for one turn (believed) | attachment (believed) | MCP | sub-agent |
|---|---|---|---|---|---|
| claude | `claude -p --continue` or `--resume <id>` | `--model` | `@path` in the prompt / image path | `--mcp-config` | `Task` (built in) |
| codex | `codex exec resume <id>` | `-m` | `-i <image>` | `[mcp_servers]` in config | spawn_agent (feature flag?) |
| copilot | `copilot --continue` / `--resume <id>` | `--model` | image path in prompt (unclear) | `--additional-mcp-config` | `task` agent type |
| gemini | `gemini --resume <id|latest> -p` | `-m` | `@path` | `settings.json` mcpServers | `invoke_agent` (flag?) |
| pi | `pi -c -p` (continue) | `--model` / `/model` | `@file` | extension, not MCP | none known |
| opencode | `opencode run --continue` / `--session <id>` | `-m` | `-f <file>` | `opencode.json` mcp | `task` (subtask) |
| cursor-agent | `cursor-agent --resume <chatId> -p` | `-m` | unclear | `mcp.json` | `task_v2` |
A turn the agent cannot do headless is SKIPPED BY NAME in that agent's recording (the driver writes a `skipped: <turn> <reason>` line), never faked. The interrupt (turn 5) needs the driver to send
SIGINT to the process after the first tool call and re-run the turn; agents that do not persist an aborted turn will show nothing, which is itself the finding.

## Cost and safety (nothing is run until approved)
- Order: gemini first (a click, as for the core capture), then the others one agent per CI job with its own key behind the API proxy (pinning.md), caps mandatory for codex and pi.
- 10 turns is about 3-5x the core recording per agent; estimate before any run from the cassette sizes of the core capture, shown on Bobby's For-you page.
- The image is a committed 16x16 PNG (`fixtures/pixel.png`, to be added); the MCP server is a 20-line local stdio echo, no network.
- Each recording lands as `test-fixtures/<agent>/convo-extended.*` and becomes a baseline set `<agent>-extended`, the same way `convo-compacted.*` became `<agent>-forced`.

## Open (Bobby)
1. Approve the ten turns, or cut: the two with the lowest expected yield are 7 (attachment: few readers model images) and 8 (MCP: needs per-agent config).
2. Compaction: stack #108's forcing on the extended session (one more recording per agent), or leave compaction on the core session only (default).
3. Cursor: manual capture only, as today (default), or add `cursor-agent` once its multi-turn driver is verified.

## Not run
Design only: no recording, no live calls, no code, no gates.
