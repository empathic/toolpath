# Piece 00 recon — the four gating answers

Everything in `roger-amp-plan/PLAN.md` downstream of piece 00 depends on these
four answers. Each is stated in one sentence, then evidenced.

All captures: Amp **`0.0.1785170481-ga5b614`**, macOS 25.5.0 arm64,
2026-07-27, agent mode `medium` (model `gpt-5.6-sol`), three private threads.

---

## Q1 — Reconstruction

> **Can a completed thread be fully rebuilt from this machine afterwards?**

**Yes — but only via `amp threads export <thread-id>`, which is a
CLI-mediated *server fetch*; no local file contains message bodies.**
`[observed, 0.0.1785170481-ga5b614]`

This is a **fourth option** the plan did not enumerate (it anticipated
local-log reconstruction, a raw server API, or capture-time-only).

### Evidence

**The local per-thread log is not a transcript.**
`~/.cache/amp/logs/threads/T-<id>.log` is structured JSONL *telemetry*.
For the 24-message feature-elicit thread it is 203 KB / 501 lines with a
**maximum line length of 548 bytes** — far too small to hold message bodies.
Its `[observer] onMessageAdded` lines carry shape, never content:

```json
{"@timestamp":"…","message":"[observer] onMessageAdded","type":"message_added",
 "messageId":"M-033wpEBLQcKPOxKqsoxPpI","role":"assistant",
 "blockCount":2,"blockTypes":["thinking","tool_use"],
 "blockStates":["complete","complete"],"messageState":"complete","hasUsage":true}
```

Probing the log for distinctive strings that *are* in the export:

| String | in thread log | in export |
| --- | --- | --- |
| `sub-agent reported` | 0 | 1 |
| `11 words` (the sub-agent's answer) | 0 | 2 |
| `Filesystem tool exercise` (the title) | 0 | 1 |
| `No such file or directory` (a tool result) | 0 | 1 |
| `walk through a small set of tasks` (the prompt) | 0 | 1 |

Searching the whole log for any usage field
(`inputTokens|outputTokens|cacheReadInputTokens|totalInputTokens|usage`)
returns **zero hits**.

The one exception: `onToolLease` lines carry the executor's tool **inputs**
(`{type:"tool_lease", toolCallId, toolName, args, messageId}`), so
`apply_patch` patch text does appear. Inputs only — never results, never
assistant text, never usage.

**No other local file holds content.** `~/.local/share/amp/` contains exactly
`session.json` (UI state), `secrets.json` (API key), `device-id.json`
(`installationID`), `history.jsonl` (prompt-history lines `{text, cwd}` — the
raw prompt strings only, no responses). No SQLite store, no thread directory.
See [directory-layout.md](directory-layout.md).

**`amp threads export` returns the whole thread.** Pretty-printed JSON,
1005 lines / 66 KB for the feature-elicit thread, containing all 24 messages
with full text, thinking, tool inputs, tool results, and per-message usage.
See [events.md](events.md).

**It is a server fetch, not a local read.** The sibling `amp threads raw`
failed with a server-side authorization error, which only a network call can
produce:

```
Error: Failed to export raw thread: Raw thread request failed (403):
{"error":"You do not have permission to access this resource"}
```

### Consequence for the crate

`toolpath-amp` **cannot be a pure filesystem reader** like every other
provider. It must either shell out to `amp threads export` or speak the
server API directly. This is the single biggest architectural difference from
`toolpath-claude` / `-codex` / `-copilot`, and it has knock-on effects:
offline derivation is impossible, `p list amp` needs the network, and the
`AmpConvo::new()`-infallible-on-missing-dirs convention in PLAN.md piece 01
needs rethinking (there is no directory to miss).

---

## Q2 — Tokens

> **What usage counters exist, and which kind-v1.1.0 pattern do they fit?**

**Amp reports genuine, non-cumulative, per-assistant-message usage — the
cleanest of the three legal patterns — available identically in both the
export document and the `--stream-json` stream.**
`[observed, 0.0.1785170481-ga5b614]`

No escalation needed.

### The counters

Per assistant message, in `amp threads export`:

```json
"usage": {
  "model": "gpt-5.6-sol",
  "timestamp": "2026-07-27T15:55:28.713Z",
  "inputTokens": 0,
  "outputTokens": 35,
  "maxInputTokens": 272000,
  "totalInputTokens": 16760,
  "cacheReadInputTokens": 0,
  "cacheCreationInputTokens": 16760
}
```

### Why "per-message", not "cumulative"

1. **`outputTokens` is not monotonic.** Across the install-session thread it
   runs `35 → 13 → 7`; a cumulative counter cannot decrease. Across the
   feature-elicit thread it runs
   `117, 142, 112, 119, 112, 128, 149, 139, 143, 150, 111, 115` — noise around
   a per-message mean, not a running total.
2. **Continuing a thread does not accumulate.** The trivial thread's first
   turn spent `output_tokens: 5`; the continued second turn also spent
   `5`, not `10`.
3. **`totalInputTokens` is a derived per-message sum, not a running counter.**
   Verified on **all 17 usage objects across all three threads**:

   ```
   totalInputTokens == inputTokens + cacheReadInputTokens + cacheCreationInputTokens
   ```

   It rises across a thread only because the prompt grows — that is prompt
   size, not accumulation.

### Two fields that must never be summed

- **`maxInputTokens` (272000)** is the model's **context-window capacity**,
  not a spend. It is constant across every message.
- **`totalInputTokens`** is the sum of the three input-side counters already
  being recorded; storing it as well double-counts.

### Sources that are *not* token sources

- `amp usage` → account credit balance only
  (`Individual credits: $4.39 remaining`).
- `amp threads usage <id>` → **a dollar figure only** (`$0.32`) plus a link to
  the web page. No token counts.
- The local thread log → zero usage fields (see Q1).

### Classification

**Clean per-message.** One Amp message carries exactly one `usage` object; it
is neither a repeated message total (Claude's per-content-block streaming
snapshot) nor a cumulative counter (Codex's `total_token_usage`). Therefore:

- Map one Amp message → one `Turn`; put the usage in **`Turn.token_usage`**.
  `Σ token_usage` over the path is the session total, by construction.
- **No `group_id` is needed** and **no `attributed_token_usage` should be
  emitted** — Amp reports per *message*, not per content block, so there is no
  finer-grained spend to attribute and inventing one would be fabrication.
- **No `breakdowns`.** Amp does not itemize reasoning tokens anywhere
  observed, despite emitting thinking blocks. Omit the field rather than
  guess.

Field mapping into `toolpath_convo::TokenUsage`:

| Amp | toolpath | Note |
| --- | --- | --- |
| `inputTokens` | `input_tokens` | Uncached prompt tokens. `0` here is a real zero (everything was cached), not a placeholder. |
| `outputTokens` | `output_tokens` | |
| `cacheReadInputTokens` | `cache_read_tokens` | |
| `cacheCreationInputTokens` | `cache_write_tokens` | |
| `totalInputTokens` | *(dropped)* | Derived; would double-count. |
| `maxInputTokens` | *(dropped)* | Capacity, not spend. |
| `model` | `Turn.model` | Not a usage field. |

Guard still applies: if every counter in a `usage` object is zero, decode to
`None` rather than stamping zeros.

---

## Q3 — Isolation

> **Which environment variables relocate dataDir/cache?**

**There is no `AMP_*` data-directory override, but `HOME` and the three
XDG variables each relocate Amp's state independently, with XDG taking
precedence.** `[observed, 0.0.1785170481-ga5b614]`

### Documented `AMP_*` variables `[official]`

From `amp --help`'s own `Environment variables:` section:

| Variable | Effect |
| --- | --- |
| `AMP_API_KEY` | Access token for Amp |
| `AMP_URL` | Amp service URL (default `https://ampcode.com/`) |
| `AMP_LOG_LEVEL` | Log level (also `--log-level`) |
| `AMP_LOG_FILE` | Log file location (also `--log-file`) |
| `AMP_REMOTE_CONTROL_TERMINAL` | Enable/disable terminal access from ampcode.com |
| `AMP_SETTINGS_FILE` | Settings file path (also `--settings-file`) |

**None of these relocates the data or cache directory.** `AMP_SETTINGS_FILE`
and `AMP_LOG_FILE` move individual files only.

### The probe

Running `threads list` under a scrubbed environment
(`env -i PATH=… HOME=<tmp> TERM=dumb`) created a complete fresh state tree:

```
<ISO>/.config/amp/
<ISO>/.local/share/amp/{secrets.json,device-id.json}
<ISO>/.cache/amp/logs/cli.log
```

Adding `XDG_DATA_HOME` / `XDG_CACHE_HOME` / `XDG_CONFIG_HOME` pointing at three
separate directories moved everything there instead — the isolated `HOME`
tree gained **no new files**, and each XDG root received its own `amp/`
subtree. So the precedence is **XDG first, `HOME` fallback**.

Recipe for the isolated-home story pieces 03/05 need:

```bash
env HOME="$tmp" \
    XDG_DATA_HOME="$tmp/data" XDG_CACHE_HOME="$tmp/cache" XDG_CONFIG_HOME="$tmp/config" \
    AMP_API_KEY="$key" \
    amp threads list
```

### ⚠️ The gotcha that must go in `verify-amp-live.sh`

An isolated home with **no** `secrets.json` does not fail — it
**auto-launches the browser CLI-login flow**, and if the operator's browser is
already signed in to ampcode.com, the flow **completes unattended and mints a
new access token** into the isolated directory. This happened twice during
this recon; both scratch tokens were deleted immediately.

Any automated isolated-home script **must** supply `AMP_API_KEY` so the login
flow is never reached.

### A second hazard worth knowing

`amp --help` documents `-x, --execute` as *"Enabled automatically when
redirecting stdout."* Every non-TTY invocation is therefore an execute-mode
invocation. A mistyped subcommand under output capture does not print usage —
it tries to start a **billable thread** (it errors out asking for a message,
but the failure mode is one typo away from spending credits). Always pass an
explicit subcommand.

---

## Q4 — Envelope

> **What is the `--stream-json` line format?**

**Four line types — `system/init`, `user`, `assistant`, `result/success` — in
a deliberately Claude-Code-compatible envelope keyed by `session_id`, which is
the Amp thread id.** `[observed, 0.0.1785170481-ga5b614]`

`amp --help` states this outright `[official]`:

> `--stream-json` — When used with `--execute`, output in **Claude
> Code-compatible stream JSON format** instead of plain text.
> `--stream-json-thinking` — Include thinking blocks in stream JSON output
> (**non-Claude Code extension**). Implies `--stream-json`.
> `--stream-json-input` — Read JSON Lines user messages from stdin. Requires
> both `--execute` and `--stream-json`.

Captured verbatim from `amp -x 'Reply with exactly: ok' --stream-json`:

```json
{"type":"system","subtype":"init","cwd":"/tmp/amp-trivial","session_id":"T-019fa4d8-…","tools":["apply_patch","…"],"mcp_servers":[],"agent_mode":"medium"}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: ok"}]},"parent_tool_use_id":null,"session_id":"T-019fa4d8-…"}
{"type":"assistant","message":{"type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":0,"cache_creation_input_tokens":16396,"cache_read_input_tokens":0,"output_tokens":5,"max_tokens":272000,"service_tier":"standard"}},"parent_tool_use_id":null,"session_id":"T-019fa4d8-…"}
{"type":"result","subtype":"success","duration_ms":4694,"is_error":false,"num_turns":1,"result":"ok","session_id":"T-019fa4d8-…"}
```

Full field catalogue in [events.md](events.md#the---stream-json-envelope).

### Stream vs export: what each has that the other lacks

| | export | stream |
| --- | --- | --- |
| Availability | any time, any thread | capture time only |
| Thinking blocks | yes | only with `--stream-json-thinking` |
| Tool result payload | structured object | the same JSON **stringified into a string** |
| Explicit error flag | no (`exitCode` inside the payload) | `is_error` on the `tool_result` block |
| Usage casing | camelCase + `model`/`timestamp`/`totalInputTokens` | snake_case + `service_tier` |
| Tool inventory / mcp servers | no | yes (`system/init`) |
| Wall-clock + turn count | no | yes (`result`) |

The **usage figures are identical**: all 12 assistant messages of the
feature-elicit capture matched field-for-field between the two encodings.

### Structural correspondence

The feature-elicit run produced **26 stream lines** and **24 export messages**:
`24 = 26 − system/init − result`, and the 12 `user` + 12 `assistant` stream
lines map 1:1 onto the export's 24 messages in order.

---

## Deviations from the plan's protocol

- **`--stream-json-thinking` was tested by *continuing* the trivial thread**
  rather than opening a third one, to conserve credits. Result: no thinking
  block was emitted, consistent with the export showing `thinking: ""` for
  that message.
- **The feature-elicit prompt ran verbatim, unmodified**, as a single `-x`
  argument. No adaptation was needed. The only checklist item it cannot
  satisfy on Amp is "2+ user turns": execute mode sends exactly one.
- **`amp threads raw` was attempted and failed** (403). Recorded rather than
  retried.
- **The canonical fixture is `convo.json`, not `convo.jsonl`.** The plan
  named a `.jsonl` file; Q1 makes the pretty-printed export document the
  canonical artifact, so `test-fixtures/amp/convo.json` holds it and the teed
  stream is a sidecar at `test-fixtures/amp/stream.jsonl`. DoD equivalent:
  `jq -e '.messages | length' test-fixtures/amp/convo.json`.

## Cost

Three threads, $0.41 total ($4.80 → $4.39): trivial thread + its continuation
≈ $0.09, feature-elicit $0.32. Exports, listings and `usage` calls are free.
