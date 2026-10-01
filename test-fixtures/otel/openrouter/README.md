# OpenRouter Broadcast fixtures

These are sanitized OpenRouter Broadcast deliveries (OTLP/HTTP JSON). They were captured during M0 on 2026-09-28 with a Broadcast "OpenTelemetry Collector" destination on a Personal workspace, then sanitized. Each `.ndjson` file has **one delivery per line**, in capture order. Each delivery is one API request: a root `LLM Generation` span plus its `generation` and `provider attempt N: …` children.

| File | What it is |
|---|---|
| `claude-code.ndjson` | Claude Code 2.1.283 over Anthropic `/messages`: 5 generations, `session.id` = Claude session UUID. The only file that keeps the duplicate `span.input`/`span.output` attributes. |
| `codex.ndjson` | Codex 0.156.1 over `/responses`: the first 6 generations of the session, `session.id` = Codex UUIDv7. Uses `developer` role messages. |
| `opencode.ndjson` | opencode 1.18.31 over `/chat/completions`: 9 generations, `session.id` = `ses_…`. |
| `pi.ndjson` | pi: 8 generations with **no `session.id` and no `user.id`**. It must be grouped by prefix clustering. |
| `codex-error-span.json` | The one error generation (`status.code = 2`, "The client has disconnected", no cost), from later in the Codex session. It must be dropped. |
| `connection-test.json` | OpenRouter's destination test (`openrouter-connection-test`, no attributes). It must be ignored. |
| `synthetic-fork.ndjson` | **Hand-built** from the Claude fixture, with session id `fixture-synthetic-fork`. Every root span carries `fixture.synthetic = true`. It has a retry that forks at the assistant turn, and a compaction-like request that shares only the system prompt. |
| `expected.json` | Assertions: per-session generation ids, tool-call ids, cost totals, cwd, and harness. Also the Claude tool calls whose history echo was rewritten by the harness, the harness-side import summary for the cross-check test, and the expected forks. |

## What sanitizing changed

- **System and developer messages, and injected user context** (system reminders, AGENTS.md, environment blocks) became deterministic stubs such as `[sanitized system d01fe707]`. They keep the harness's cwd marker (`Primary working directory: /work/project`, or `<environment_context><cwd>/work/project</cwd>…`). Identical originals map to identical stubs, so prefix stitching still holds.
- **The real task prompt is unchanged:** "Create wc.py …".
- **Paths:** the run directory became `/work/project`, and the home directory became `/home/user`. Names and emails became `Dev User` / `dev@example.com`.
- **Attribution:** user, device and entity ids were replaced with fixed fakes. `api_key_name` became `"fixture key"`.
- **`gen_ai.completion.tools` was dropped.** `rawRequest` is reduced to its keys (values `<omitted>`), except `session_id` and a faked `user`.

What was *not* changed:
- tool-call ids, generation ids, timestamps, models, usage, cost;
- tool arguments and results (apart from path and identity rewrites);
- the three Claude Code tool calls whose history echo differs from what the model emitted (the `cd /work/project &&` prefix is stripped, and `Write` content is normalized).

All of these are real behavior the converter must handle.

**Verified at generation time:** every consecutive generation pair in each real session is a clean extension (claude 4/4, codex 5/5, opencode 8/8, pi 7/7), and the leak scan is clean.
