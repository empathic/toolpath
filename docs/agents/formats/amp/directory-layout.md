# Directory layout

Everything Amp writes locally. **None of it is a transcript** — see
[RECON.md](RECON.md#q1--reconstruction).

All observations `[observed, 0.0.1785170481-ga5b614]` on macOS unless noted.

## The binary

```
~/.amp/bin/amp                # 71.7 MB Bun single-file executable, arm64
~/.local/bin/amp -> ~/.amp/bin/amp
```

It is a Bun compiled binary (`argv: ["bun", "/$bunfs/root/amp-darwin-arm64"]`),
so the whole application is embedded minified JavaScript and is
`strings`-mineable — the source of every `[reverse-eng]` claim in this folder.
`amp update` replaces it in place; the version is build-timestamped
(`0.0.<epoch>-g<sha>`), so **the binary changes often** — it moved
`gd1fcef` → `ga5b614` within 1.5 hours during this recon.

## Data directory — `~/.local/share/amp/`

Mode `0700`; files `0600`.

| File | Size | Contents |
| --- | --- | --- |
| `session.json` | ~455 B | Local **UI/app state**, not conversation state. See [session-state.md](session-state.md#sessionjson--local-app-state). |
| `secrets.json` | 141 B | The stored API key. **Never read, never committed.** Recreated by the login flow. |
| `device-id.json` | 62 B | A single key, `installationID` (a UUID). Stamped into every thread's `env.initial.platform`. |
| `history.jsonl` | grows | Prompt history: one object per line, `{"text": "...", "cwd": "..."}`. **Prompts only** — no responses, no ids, no thread linkage. |

There is **no** thread directory, **no** SQLite database, and no per-thread
file of any kind under this root.

## Cache directory — `~/.cache/amp/`

```
~/.cache/amp/logs/cli.log                  # process-wide structured JSONL
~/.cache/amp/logs/threads/T-<id>.log       # per-thread structured JSONL
```

Both are **operational logs**, not session records. Each line is a JSON object
with `@timestamp`, `level`, `message`, `logger`, `pid`, plus a message-specific
key set. `cli.log` is global; the `threads/` files are the same stream filtered
to one `threadId`.

Representative `message` values in a per-thread log (24-message session, 501
lines):

| `message` | n | What it is |
| --- | --- | --- |
| `[thread-client] Received server message` | 269 | Inbound frame **metadata** (`type`, `frameLength`, `blockCount`, `hasUsage`) — never the frame body |
| `websocket message` | 48 | Transport-level |
| `[thread-client] JSON-RPC request sent` / `completed` | 46 / 46 | RPC bookkeeping |
| `onToolLease` | 12 | **The only content-bearing line**: `{type, toolCallId, toolName, args, messageId}` — tool *inputs* |
| `onExecutorToolResultAck` | 12 | Acknowledgement only; no result payload |
| `… executing tool: <name>` | 12 | Executor trace |
| `[observer] onMessageAdded` | — | `messageId`, `role`, `blockCount`, `blockTypes`, `blockStates`, `hasUsage` — shape, never content |

Maximum line length observed: **548 bytes**. The log is structurally incapable
of holding message bodies.

Two useful details for anyone reading these logs:

- `hasUsage: true` appears on `delta/assistant.complete`,
  `delta/assistant.tool_use`, and assistant `message_added` lines, and `false`
  on `assistant.start` / `.generating` / `.error` / `.aborted`. It is a
  boolean flag — **the usage numbers themselves are not in the log**.
- One `onMessageAdded` was seen with `seq: 9007199254740991`
  (`Number.MAX_SAFE_INTEGER`), immediately followed by the same `messageId`
  with a real `seq`. Treat `seq` as advisory. `[observed, 0.0.1785164324-gd1fcef]`

## Config directory — `~/.config/amp/`

```
~/.config/amp/settings.json     # absent until you create it
```

`amp --help` names this path as the default settings location `[official]`.
It was **absent** on the capture machine and Amp ran fine without it.

Workspace-scoped settings live at `<workspaceRoot>/.amp/settings.json`
`[reverse-eng]`; `.amp/services.yaml` is referenced by the `orb service`
family `[official]`.

## Relocating any of it

Precedence is **XDG variables first, then `HOME`** — verified by probe, see
[RECON.md](RECON.md#q3--isolation).

| Variable | Moves |
| --- | --- |
| `XDG_DATA_HOME` | → `<value>/amp/` (secrets, device id, session.json, history) |
| `XDG_CACHE_HOME` | → `<value>/amp/logs/` |
| `XDG_CONFIG_HOME` | → `<value>/amp/` (settings) |
| `HOME` | fallback root for all three |
| `AMP_SETTINGS_FILE` / `--settings-file` | the settings **file** only `[official]` |
| `AMP_LOG_FILE` / `--log-file` | the log **file** only `[official]` |

There is **no** `AMP_DATA_DIR`-style override.

> **Do not run an isolated home without `AMP_API_KEY`** — Amp will open a
> browser login flow that can complete unattended and mint a real token into
> the scratch directory. See [RECON.md](RECON.md#️-the-gotcha-that-must-go-in-verify-amp-livesh).

## What lives server-side instead

Threads themselves — bodies, tool results, usage, titles, visibility,
labels, sharing state. Reached through `amp threads …` (see
[resume-and-sessions.md](resume-and-sessions.md)) or, underneath, through
`https://ampcode.com` HTTP + a Rivet-actor websocket
(see [known-gaps-and-sourcing.md](known-gaps-and-sourcing.md#server-api-surface-reverse-eng)).
