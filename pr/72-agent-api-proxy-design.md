# design: the agent API proxy (webmock `live` mode) -- record before code

Status: DESIGN ONLY, no code in this piece · Base: pr/71 · Source: Bobby's GO 2026-10-20 and ~/.lobby/ops/proposal-agent-api-proxy-2026-10-20.md
Replaces: the temp-home `op run` credential-hygiene piece (capture-live env-only). Absorbs: the webmock HTTPS-listener item.

## Problem
Live checks and re-recording need real provider keys. Today `capture-live` (crates/toolpath-goldens/src/goldens.rs) copies
~/.codex/auth.json and ~/.pi/agent/auth.json into a temp home and falls back to the Keychain item claude-code-oauth-token. A key in the
agent's own process or home can end up in a log, a recording, argv or a leaked temp dir. We want the agent process never to hold a
real key.

## Shape (one binary, three modes)
`toolpath-webmock` already is a loopback server with `replay` and `record` (src/lib.rs). Add `live`:
- replay: serve cassettes; a miss is 599. Unchanged.
- record: forward upstream, save request/response, secret headers stripped, paths masked. Unchanged, except the credential comes from the
  proxy (below), not from the agent.
- live: forward upstream, save nothing; write only the request log.
The agent gets a FAKE key and the base-URL switch it already has (run.rs `env(...)`: ANTHROPIC_BASE_URL, COPILOT_API_URL,
COPILOT_DEBUG_GITHUB_API_URL, GOOGLE_GEMINI_BASE_URL, plus the codex/pi/opencode config files run.rs writes). The proxy replaces the
fake auth header with the real one on the way out.

## Contract (what must hold; each line becomes a test)
1. Keys enter the process once, by stdin or an inherited fd, from `op read op://toolpath-ci/<agent>/credential`; never argv, never an
   env var of the agent, never a file in the temp home. Test: `ps -ww` while a run is in flight shows no key; the agent's `/proc`-less
   equivalent (`ps eww` on macOS) shows only the fake key.
2. The real key is held in memory only, in a type with no Debug/Display/Serialize, zeroised on drop. No key in logs, cassettes, panic
   messages or error strings. tests/no_secrets.rs keeps scanning cassettes and additionally scans the proxy's request log and stderr.
3. Bind 127.0.0.1 only. Refuse any request whose Host/upstream is not a declared upstream of the agent (table in code, one entry per
   agent: api.anthropic.com, api.openai.com, chatgpt.com/backend-api for codex ChatGPT auth, api.githubcopilot.com, generativelanguage.googleapis.com, ...).
   Anything else is 403 and logged. No open-proxy behaviour, no CONNECT to arbitrary hosts.
4. The proxy accepts only the fake key for that run (random per run). A request with another key is 401, so another local process
   cannot spend the real key through the port.
5. Caps per agent per run and per day: requests, tokens (from the usage the response reports), dollars (from a price table in code,
   pinned, unknown model = counted at the top price). Exceeding a cap returns 429 with a body naming the cap and ends the run.
   The day counter lives in a file under the user's state dir holding counts only (no key).
6. First upstream 429 (or `anthropic-ratelimit-*` status exhausted) stops the run: no retry, no second attempt. The agent sees the 429.
7. Every call is logged: time, agent, method, path, status, bytes, model, token usage, rate-limit headers. Never bodies unless record mode,
   never the Authorization header.
8. TLS: for agents that refuse http (Codex under ChatGPT auth), a per-run throwaway CA and leaf cert generated at start, kept in the
   cell's temp dir, trusted ONLY via the cell's env (SSL_CERT_FILE / NODE_EXTRA_CA_CERTS / the agent's own variable); never the system
   keychain or any global store. Deleted at the end of the run (including on failure). Tests: the files are gone after the run; nothing is
   written outside the temp home; the keychain is untouched (compare `security` listing is out of scope in the sandbox, so the test asserts
   the code path never calls it and that no path outside the temp home is written, via a write-guard on the run's file API).
9. Fail closed: a missing item, an `op` failure, or a missing declared upstream stops with a NAMED reason (`skip: <agent> credential
   op://toolpath-ci/<agent>/credential not available`). Never fall back to a dotfile or the Keychain.
10. Replay needs none of this: no key, no network, no `op`. CI replay stays as it is.

## Prior art (checked; links in docs/webmock-prior-art.md)
- toolpath-webmock itself: record/replay, normalized request key, SSE chunk replay, secret-header stripping. Keep. This design adds the
  forward-with-injection path, which record already has (it forwards upstream) minus credential injection.
- pathbase-llm-proxy (empathic/pathbase crates/pathbase-llm-proxy, ~1600 lines, axum): transparent reverse proxy that parses usage
  from SSE and JSON and reads the `anthropic-ratelimit-*` family by prefix (src/ratelimit.rs, with a key-space guard). Its rule 1 is "never
  degrade the request path", which conflicts with caps and stopping on 429. Reuse the header-family parsing and the usage parsing (src/parse.rs),
  not the telemetry export. Decision needed on HOW (see Open 1).
- mitmproxy reverse mode / proxay / Hoverfly: can proxy and record but do not inject a secret from stdin, enforce a cap, or ship as
  one static binary alongside our cassette key; rejected in docs/webmock-prior-art.md for the same reasons.
- 1Password: `op run` injects env vars into a child; the proxy needs the value in its own process, so it uses `op read` over a pipe.
  Shared read limit (1,000/day, fleet-wide): one read per proxy start, never per request.

## Entry points
- `path d webmock run <agent> live|record|replay -- <agent args>` (exists for record/replay; gains `live`).
- `just live-check <agent>`: starts the proxy under the developer's own `op` sign-in, runs the agent cell, writes the committed
  resume-results file `baseline map` reads. Local or scheduled only; never per PR.
- CI later: same commands with the service-account token. Out of scope here.

## Limits (stated up front)
- An agent signed in with a personal account may refuse a base-URL override (Codex with a ChatGPT login). The consultant's pick: re-record codex
  and pi under API keys and test whether API-key codex accepts plain http; if so the TLS part (8) is optional for codex and is built only
  for what still needs it.
- Cursor uses its own protobuf API. First version: pass-through with auth injection and the request log only, no recording.
- While it runs the proxy holds every key it was given. Contract 3 and 4 bound that; they are not a sandbox.

## Open (needs a decision before code)
1. Reuse of pathbase-llm-proxy: depend on it as a crate (different workspace, axum vs hyper), extract a small shared crate for the
   header-family and usage parsers, or port the two files? Recommendation: port the two parsers into toolpath-webmock with their
   tests (about 400 lines), cite the source, and offer the shared crate later. Avoids a cross-repo dependency.
2. Price table source for the dollar cap (pinned in code vs a committed JSON). Recommendation: committed JSON next to the cassettes, tested for staleness by date.
3. Default caps (suggest per run: 50 requests, 200k tokens, 1 dollar; per day: 5 dollars per agent). Bobby sets the numbers.
4. Does the day counter need locking across concurrent runs? Recommendation: yes, flock on the counts file.

## Build order (after this record is accepted)
1. `live` mode + fake-key check + upstream allowlist + request log + contract tests 1-4, 6, 7, 9 (no TLS, no caps).
2. caps + day counter (5).
3. per-run TLS CA (8), only where an agent needs it, plus the cleanup tests.
4. `just live-check` + resume-results file; `baseline map` reads it.
Each step its own pr/NN with its tests; the hold on capture-live env-only stays: it is dropped, not built.

## Not run
This is a record; no code, no tests, no gates beyond reading the source named above.

## Update 2026-10-20 (Bobby): the keys exist, and two of them have no provider-side cap
All four live-check keys are in 1Password vault toolpath-ci, field `credential`: claude (Anthropic workspace toolpath-ci, 10 USD/month cap),
codex and pi (OpenAI org EMPATHIC, Default project, restricted to Responses, chat and list models, 30 days; both returned 200 on
/v1/responses), copilot (GitHub PAT, Copilot Requests read-only, overage budget 0). The OpenAI org allows no project budget, so for codex
and pi the proxy's caps (contract 5) and the stop at the first 429 (contract 6) are the ONLY spend limit: they are mandatory for those two,
not optional, and their defaults must be set before any live run (Open 3 is no longer deferrable). The proxy refuses to start a live run for
an agent with no cap configured. Live calls run only on Bobby's go per run.
