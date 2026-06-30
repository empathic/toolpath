# `~/.openclaw/` directory layout

Everything OpenClaw persists lives under a single state directory, by
default `~/.openclaw/`. There is **no XDG split and no per-OS special
directory** — the layout is identical on macOS, Linux, and Windows; the
only platform difference is how the home directory itself is found. The
directory is created on first run and populated lazily.

## Resolving the state directory

The root is chosen by `resolveStateDir`
(`src/config/paths.ts:209-273`), in this precedence:

1. `OPENCLAW_STATE_DIR` (explicit override; `~` is expanded).
2. An existing `~/.openclaw` (the current default name).
3. An existing legacy `~/.clawdbot` (the project's former name).
4. Default `~/.openclaw` (created if nothing above matched).

The `~` used above is itself resolved by `resolveOsHomeDir`
(`src/infra/home-dir.ts:35-54`): `OPENCLAW_HOME` wins (also `~`-expanding),
otherwise `HOME` → `USERPROFILE` → Termux home → `os.homedir()`.

Literal defaults for a default user with no overrides:

| OS      | State directory                       |
|---------|---------------------------------------|
| macOS   | `/Users/<user>/.openclaw`             |
| Linux   | `/home/<user>/.openclaw`              |
| Windows | `C:\Users\<user>\.openclaw`           |

Related overrides: `OPENCLAW_CONFIG_PATH` (the config file, default
`<stateDir>/openclaw.json`; legacy `clawdbot.json` is tolerated),
`OPENCLAW_OAUTH_DIR` (default `<stateDir>/credentials`), and
`OPENCLAW_TRAJECTORY_DIR` (see [known-issues.md](known-issues.md) and
below). There is **no** `OPENCLAW_CONFIG_DIR` and **no** `XDG_*` lookup.

### Daemon path divergence

The managed-service daemon has its own resolver
(`src/daemon/paths.ts:116-127`) that reads `HOME`/`USERPROFILE` directly
(ignoring `OPENCLAW_HOME`) and appends a profile suffix: a non-default
`OPENCLAW_PROFILE` yields `~/.openclaw-<profile>`. The daemon then launches
the gateway with `OPENCLAW_STATE_DIR` pointed at that profiled directory.
With the default profile both resolvers agree on `~/.openclaw`. If you ever
find sessions missing, check whether a profile is in play.

## The tree

```
~/.openclaw/                                  # = state dir (override: OPENCLAW_STATE_DIR)
├── openclaw.json                             # user config (override: OPENCLAW_CONFIG_PATH)
├── credentials/
│   └── oauth.json                            # OAuth creds (override dir: OPENCLAW_OAUTH_DIR)
├── state/
│   └── openclaw.sqlite                       # shared state DB — NOT conversation content
└── agents/
    └── <agentId>/                            # default agentId = "main"
        ├── agent/
        │   └── openclaw-agent.sqlite         # per-agent DB (caches, RAG index) — NOT content
        └── sessions/                         # ← canonical transcripts live here
            ├── sessions.json                 # index: sessionKey -> { sessionId, sessionFile, … }
            ├── <sessionId>.jsonl             # canonical session transcript
            ├── <sessionId>-topic-<topicId>.jsonl   # topic-scoped transcript
            ├── <ISO-ts>_<sessionId>.jsonl    # forked / rotated transcript
            ├── <sessionId>.trajectory.jsonl  # runtime telemetry sidecar — NOT canonical
            └── <sessionId>.trajectory-path.json    # pointer to the runtime trace file
```

Only the per-session `*.jsonl` transcript under `agents/<agentId>/sessions/`
is needed to reconstruct a conversation. Everything else is supporting
state (see [Stores that are not the transcript](#stores-that-are-not-the-transcript)).

Path builders:

- Sessions directory — `src/config/sessions/paths.ts:653-661`:
  `<stateDir>/agents/<normalizedAgentId>/sessions`, default agent id
  `"main"` (`src/routing/session-key.ts`, `DEFAULT_AGENT_ID`;
  `normalizeAgentId` lowercases and path-sanitizes).
- Transcript filename — `src/config/sessions/paths.ts:898-902`:
  `<sessionId>.jsonl`, or `<sessionId>-topic-<topicId>.jsonl` when a topic
  is set.
- Session index — `src/config/sessions/paths.ts:678-680`:
  `<sessionsDir>/sessions.json`.

## Naming and keying

Sessions are **bucketed by agent** (the directory) and **keyed by
channel/peer/thread** (the routing key, used as the key in `sessions.json`).
They are **not** bucketed by project (like Claude/Gemini) or by date (like
Codex).

- **Directory bucket** = `agentId` (default `main`).
- **Logical key** = a composite routing key such as
  `agent:main:whatsapp:group:12345` — see
  [channels-and-actors.md](channels-and-actors.md) for the full grammar.
- **Filename** = the generated `sessionId`, a UUID. A forked or rotated
  transcript prefixes an ISO timestamp (`<ISO-ts>_<sessionId>.jsonl`); a
  topic-scoped one suffixes `-topic-<topicId>`. (Entry ids *inside* a file
  are only 8-char UUIDv7 prefixes — distinct from the file's full UUID; see
  [jsonl-envelope.md](jsonl-envelope.md).)

The `sessions.json` index is a JSON object mapping each routing key to an
entry of roughly `{ sessionId, sessionFile, updatedAt, sessionStartedAt, …
delivery state }`. A reader should resolve keys to concrete files through
this index rather than guessing filenames.

## Stores that are not the transcript

Three other on-disk stores sit next to the transcript. None of them is the
canonical conversation record; do not reconstruct conversations from them.

| Store | Path | What it holds |
|---|---|---|
| Trajectory trace | `…/sessions/<sessionId>.trajectory.jsonl` (+ `.trajectory-path.json` pointer) | Append-only `openclaw-trajectory` runtime telemetry (prompts, compiled context, tool calls, richer usage). The diagnostic exporter *joins* this with the transcript to build a support bundle, so the transcript is the source of truth and this is auxiliary. Advisory sidecar. |
| Shared state DB | `~/.openclaw/state/openclaw.sqlite` | Auth profiles, device/node pairing, push, model-capability cache, cron jobs/logs, task/subagent/flow runs, delivery/ingress queues, `current_conversation_bindings` (a routing pointer, no message text). The only message-shaped table, `acp_replay_events`, is a transient ACP replay buffer, not the agent's own transcript. |
| Per-agent DB | `~/.openclaw/agents/<id>/agent/openclaw-agent.sqlite` | `cache_entries`, auth-profile rows, a `memory_index_*` / embedding RAG index. No transcript table. |

A diagnostic **trajectory export** (a different root entirely) lands under
`<workspaceDir>/.openclaw/trajectory-exports/openclaw-trajectory-<id8>-<ts>/`
with `events.jsonl`, `session-branch.json`, `manifest.json`, etc.
(`src/trajectory/export.ts`). That is a redacted support bundle, not part
of the live store.

## Permissions

| Path | Mode |
|---|---|
| `…/sessions/<sessionId>.jsonl` | `0600` (append opens `a+`, `0o600`; `src/config/sessions/transcript-jsonl.ts:98`) |
| `…/sessions/sessions.json` | `0600` (atomic write; dir `0o777 & ~umask`) |
| `…/sessions/<sessionId>.trajectory.jsonl` | `0600` |
| `~/.openclaw/state/openclaw.sqlite` | dir `0700`, file `0600` |
| `~/.openclaw/agents/<id>/agent/openclaw-agent.sqlite` | dir `0700`, file `0600` |
| `~/.openclaw/openclaw.json`, `credentials/oauth.json` | `0600` |

No protobuf anywhere in these paths. The session transcript is JSONL;
indexes and pointers are JSON; the two databases are SQLite (node:sqlite,
WAL).
