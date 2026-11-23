# Cassettes

One directory per upstream, recorded 2026-10-15 by the Bun prototype of this crate (the spec it was ported from), one real call each, from the toolpath resume matrix's headless cells.

| upstream | what was recorded | replay |
|---|---|---|
| pathbase | `path share --anon` of a golden session against the local pathbase, plus five streamed-upload calls of pathbase #486 (POST graphs, open path x-ndjson, append, final batch + Head, GET download) re-recorded 2026-10-23 against pathbase main `d874b09` (includes #468 and #486) from a hand-driven reqwest client (not #305's output; cookie redacted); the pathbase cassettes follow the latest pathbase main (or the PR under test) and are re-recorded on each bump | exact |
| claude | one `claude -p -r` resume cell (Messages API, SSE) | loose from these committed cassettes (below) |
| codex | one `codex exec` resume cell (Responses API, SSE, https through a loopback TLS listener) | exact in the prototype |
| pi | one resume cell | exact in the prototype |
| opencode | one `opencode run` resume cell on the free model | exact in the prototype |
| gemini | one `gemini --resume -p` cell; the request body carries the restored history, the stream was cut by a 429 retry loop | request pinned; no complete response |
| copilot | one `copilot -p` resume cell (CAPI, plus GitHub's remote MCP at `/mcp/readonly`, JSON-RPC over SSE) | exact from these committed cassettes since 2026-10-19 (`path d webmock run`, below) |

The copilot miss was ours, not copilot's. Copilot runs two `tools/list` calls at once (ids 1 and 2). Both keyed exact, but the replayed answer carried the recorded id 1, so call 2 never resolved: copilot waited out its 60 s MCP timeout three times, sent `notifications/cancelled` and an id-3 retry, and all of those shared one loose key with `initialize` (an MCP call has no model and no user text), so they got the `initialize` answer. It then ran with no MCP tools: 19 tools instead of 25, and every `/v1/messages` call missed (rc 1, 152 s). Now a replayed JSON-RPC answer carries the live request's id (`rewrite_jsonrpc_id`) and a JSON-RPC body's method is part of its loose key. Same cell, pre-scrub cassettes, Rust server: before, rc 1 in 152 s, 6 misses, 19 tools; after, rc 0 in 1 s, every request exact, 25 tools, the recorded reply.

Secret headers are never stored (`is_secret_header`: the list plus any `*-key`, `*-token`, `*-secret`). The first gemini recording predates the `x-goog-api-key` entry and was scrubbed by hand before being committed; the key it carried is to be rotated. The same recordings kept `x-gemini-api-privileged-user-id` (the Google account behind the key) and, in the pi response chunks, the recording machine's scratch path inside the base64; both scrubbed, the header added to the secret list, and `tests/no_secrets.rs` now reads the base64 too.

## Provenance

`<upstream>/provenance.json` says where each directory's cassettes came from: the golden set (its input file and sha256, the session id in it), the resume-matrix cell, the toolpath commit the projection was made at (`projection.toolpath_rev`, with how that was established in `basis`), the probe command and its prompt, and the file list. `tests/provenance.rs` checks it against the tree: the sha256 is the sha256 of `goldens/<set>/input.jsonl` now, the session id is in that file, the file list is the directory.

Every cassette is the golden session plus the headless probe turn, by construction: `path p derive` turned the golden input into the Toolpath document, `path resume --harness <agent>` wrote it where the agent keeps sessions, and the agent was run headlessly on that session with one more prompt (the probe). The request body the proxy recorded is therefore the golden's conversation followed by the probe; nothing else was sent. So cassette and golden are the same session, and a cassette is traceable to its golden through the input sha256.

| cassettes | golden set (input sha256) | cell | toolpath rev |
|---|---|---|---|
| claude | codex (`472c7633…`) | `v2-cell-codex-to-claude` | `6aeff36` |
| codex | claude-session (`a667edf0…`) | `v2-cell-claude-session-to-codex` | `6aeff36` |
| pi | codex | `v2-cell-codex-to-pi` | `6aeff36` |
| copilot | codex | `v3-cell-codex-to-copilot` | `6aeff36` |
| opencode | codex | `gate-cell-codex-to-opencode` | `d93750a` (= `5ff0d26` on wip) |
| gemini | codex | `v4-cell-codex-to-gemini` | `0655ae9` |
| pathbase | codex | `src-codex` (derive only; `path share`) | re-recorded 2026-10-23 against a local pathbase build at `d874b09` (pathbase main, #468 + #486), the anon share POST and the five streamed-upload calls; see `provenance.json` `pathbase` |

## Gates

`tests/no_secrets.rs` fails on any credential shape or machine identity in the committed fixture surfaces (rules and allowlist: `/.gitleaks.toml`, shared with the `secrets` gate of `scripts/quality_gates.sh`). It lists every `file:line` and rule, and reads base64 runs. Run it: `cargo test -p toolpath-webmock --test no_secrets`.

The agents' request bodies name the recording machine's temporary HOME; it is replaced by `<scratch>` here. A run masks its own directories the same way before keying (`Config::masks`, `apply_masks`): the work directory becomes the placeholder the recording ran under (`run::placeholder`), and `--mask <live>=<placeholder>` adds others, such as the scratch root a cell was copied from. The loose key also drops `<system-reminder>` blocks from the last user message and collapses whitespace (`without_reminders`): Claude Code adds "The date has changed" when a session resumes on a later day, which no recording can match exactly.

## Running an agent against these cassettes

`path d webmock run <agent> replay …` (path-cli built with `--features develop`; it runs this crate's `webmock` binary, found beside `path`, on `PATH`, or at `$WEBMOCK_BIN`), or `webmock run …` directly:

```text
path d webmock run copilot replay --cell <cell> --mask <cell's old scratch root>=<scratch> \
  -- -p "<prompt>" --resume={sid} -s --no-color --no-custom-instructions --available-tools=
```

A cell is a directory with `home/`, `project/` and `sid`. The run copies it to a temporary work directory (`--in-place` runs it where it is: gemini, pi and opencode key their stores by the absolute project path), starts one server per upstream the agent calls (copilot: its model API and the GitHub API it reads the signed-in user from), sets the agent's switch and, in replay, a fake credential, runs the agent with an empty environment and a hermetic `HOME`/XDG, and prints how each request was served. A replay serves a copy of the cassettes, so misses never land in the repo. Record mode needs `--upstream` (and `--github-upstream` for copilot) and passes only the variables named with `--pass-env`.

Replayed from the committed cassettes, 2026-10-19, cells from the resume matrix:

| agent | result |
|---|---|
| copilot | rc 0, 1 s, 10 requests EXACT (with the GitHub user cassette below), the recorded reply |
| claude | rc 0, under 1 s, `/v1/messages` LOOSE: the live body differs from the recording by the date-changed reminder and one tool's schema, the recording's `<project-dir>` mask; the recorded reply |
| codex | not supported: ChatGPT-auth codex refuses a plain-http backend, and the Rust server has no TLS listener yet |
| pi, opencode, gemini | not run here: they run in place, and their cells belong to the resume matrix |

The copilot directory now also holds `GET /copilot_internal/user` from the same recording (`9418809b…`), with the signed-in identity (id, login, analytics id, assigned date, org lists) replaced by `webmock-replay` values and identifying response headers dropped (`provenance.json`, `github_user`).
