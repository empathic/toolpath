# test(webmock): every agent's cassette replays green through its real CLI (`just replay-agents`)

Status: draft · Base: pr/103 · Source: Bobby 2026-10-22 "ok lets do it" (make the replay row green)

## Why
pr/103 added one replay test per agent (copilot, claude, pi, opencode, gemini) but could not run them: no cells, no CLIs, no bound port where it was written. Run for the first time (2026-10-22, pinned CLIs from nixpkgs, network blocked except loopback), every agent missed or failed:
- claude and copilot: the request carries the current projection, so the exact key no longer matches the 10-15 recording, and the loose key missed too.
  - Claude Code now sends the previous turn's `tool_result` in the same user message as the probe, so the "last user text" included tool output.
  - copilot stamps `<current_datetime>` into the prompt and adds a `<system_reminder>` (underscore) with local table state.
- pi 0.84.3 zstd-compresses its request body (keyed on raw bytes without the `zstd-bodies` feature), and a fresh cell has no pi `auth.json`, so the fake login lacked `"type":"oauth"` ("No API key found for openai-codex").
- opencode: `path resume` needs an existing database; a schema-only database made opencode re-run its migrations ("table `project` already exists").

## What changes
- `toolpath-webmock` loose key (`keys`, `last_user_raw`, `without_reminders`):
  - the last user text skips tool output blocks (`tool_result`, `function_call_output`, Gemini `functionResponse`);
  - it is read from the normalized body (dates replaced);
  - both reminder spellings, `<system-reminder>` and `<system_reminder>`, are dropped.
  Three unit tests pin each rule. The exact key is unchanged.
- `run::prepare` (pi, replay): adds `"type":"oauth"` to the `openai-codex` entry when the cell has none.
- `scripts/replay-cells.sh <path-binary> <out> [agent…]`: builds `<out>/<agent>/{home,project,sid}` the way the cassettes were recorded:
  - the codex golden → `path p derive codex --all` → `path resume --harness <agent>`, with the harness replaced by an argv-recording shim, so no agent runs and no model is called;
  - the session id is read back from that argv;
  - for opencode, the pinned opencode creates its own database first (`session list`, offline).
- `just replay-agents [CELLS]`: builds `path` and the cells, then runs `tests/replay_agents.rs` with `--features zstd-bodies` and `WEBMOCK_REQUIRE` naming all five agents, so a skip fails.

## Verification
- Run 2026-10-22 on phobos, wip 334cd39d plus this change, pinned CLIs on PATH:
  - claude-code 2.1.296;
  - github-copilot-cli 1.0.61 (unwrapped);
  - pi-coding-agent 0.84.3;
  - opencode 1.18.18;
  - gemini-cli 0.47.0.
- The test binary ran under `sandbox-exec` with outbound network denied except loopback: `test result: ok. 6 passed; 0 failed`, `replay_zz_coverage` ok.

| agent | how its requests were served |
|---|---|
| claude | LOOSE ×1 |
| copilot | EXACT ×9 (MCP, models, user) plus LOOSE ×1 (`/v1/messages`) |
| gemini | LOOSE ×8 (the recorded 429 → retry loop, then the 200 stream) |
| opencode | LOOSE ×1 |
| pi | LOOSE ×1 (zstd body decoded) |

No agent reached a non-loopback host.

- `just replay-agents` was also run end to end: 6 passed.

## What CI needs (not done here)
- The five pinned CLIs on PATH, from the agents.lock fetch (pr/92), in the Actions job or under `act`.
- `just replay-agents`. The cells are built in the job; nothing is committed.
- codex (needs a TLS listener or an http base_url, NEXT 10) and cursor (no cassette yet, NEXT 11) are still outside this test.

## For the next PR
Re-record the cassettes from the current projection, so claude, opencode, pi and gemini replay EXACT, not only LOOSE. Loose proves the agent resumes and is answered; exact also pins the request body the projection produces.
