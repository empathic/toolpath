# toolpath-webmock: prior art

Asked in rehearsal (2026-10-19): *why build our own record/replay instead of an existing crate?* This compares the
tools we should have looked at first, against what the resume cells need. Checked 2026-10-19 against each tool's
docs or source; the links are at the end.

## What the resume cells need
`toolpath-webmock` sits between an unmodified agent binary (Claude Code, Codex, pi, Copilot, opencode, Gemini CLI)
and its model API. It records one real call, then replays it with no network and fake credentials. Five things decide
whether another tool could do that job:

1. **Base-URL target.** The agent is a Node, Rust or Go binary that we do not patch. The mock has to be a server the
   agent reaches through the base-URL setting each agent already has, not a library inside our test process.
2. **Streams.** The answers are SSE (Messages API, Responses API, CAPI; MCP is JSON-RPC over SSE). The replay must
   serve the bytes with their `data:` framing intact. Ours also keeps the chunk boundaries and their timing.
3. **JSON-RPC.** Copilot's MCP calls all go to one path. They have to be told apart by `method`, and a replayed
   answer must carry the *live* request's `id`. Copilot sends two `tools/list` calls at once; with the recorded id
   the second never resolves (`cassettes/README.md`: rc 1 in 152 s before the fix, rc 0 in 1 s after).
4. **Normalized key.** Every request carries per-call ids and clocks (`turn_id`, `client_metadata`, `created_at`,
   ISO dates in strings). The key has to drop those and keep everything else, so that a changed conversation is a
   loud miss and never a near match.
5. **No secrets in cassettes.** Credential headers are never written. Ours drops a list and any `*-key`, `*-token`
   or `*-secret` header, so a new provider header is dropped by default. A test (`tests/no_secrets.rs`) scans the
   committed cassettes, base64 chunks included.

## The tools

| Tool | 1 Base URL | 2 Streams | 3 JSON-RPC | 4 Normalized key | 5 Secrets | Verdict |
|---|---|---|---|---|---|---|
| **rvcr** (Rust) | no: a `reqwest-middleware` inside the client | body recorded whole | no | custom matcher via `VCRReplaySearch` only for ordering; no body normalizer | `with_modify_request` / `with_modify_response` hooks | Wrong shape: it only sees our own reqwest calls, not an agent's. Fine for `pathbase-client` tests. |
| **httpmock** 0.8 (Rust) | yes: standalone server (binary, Docker); 0.8 adds record by forwarding | response buffered | no | matches recorded request fields; no "drop these JSON keys" | no documented header exclusion on record | Closest Rust crate. Missing 3, 4 and 5, and its YAML is not built for multi-MB SSE bodies. Would need forking. |
| **wiremock-rs** (Rust) | yes, as a server inside a Rust test | authored `ResponseTemplate` bodies, whole | custom `Match` impl | custom `Match` impl | n/a (no recording) | No record mode. A replay engine we would still have to feed. |
| **mockito** (Rust) | yes, as a server inside a Rust test | whole bodies, or a chunked writer you author | `Matcher::PartialJson` | `PartialJson` subset | n/a (no recording) | No record mode. Same as wiremock-rs. |
| **vcr-cassette** (Rust) | n/a: a serde model of Ruby VCR's cassette format, no runtime | — | — | — | — | A format, not a tool. Worth borrowing as a file format (rvcr uses it). |
| **vcrpy** / **pytest-recording** (Python) | no: patches Python HTTP libraries in-process | whole body | `register_matcher` | `register_matcher` + `before_record_request` | `filter_headers`, `before_record_*` | Right ideas (matchers, filters, record modes), wrong process. Cannot see a Node or Rust agent. |
| **proxay** (Node, Airtasker) | yes: `--mode record/replay/mimic --host <upstream>`, one tape per test via `POST /__proxay/tape` | response buffered (`Buffer.concat`) and served at once; bytes and SSE framing kept, chunk boundaries and timing not | no; replay sends the recorded id | best-effort *similarity* score (JSON deep-diff, `rewrite-before-diff`), or `--exact-request-matching` | `--redact-headers`, `--redact-body-fields` (values become `XXXX`) | **Nearly would have done.** Base URL, record/replay/mimic and redaction are all there. Two gaps: (a) similarity matching serves the *closest* recording when the conversation changed. For a baseline that has to fail loudly, that is the wrong default; exact mode helps but has no volatile-key list. (b) No JSON-RPC id rewrite, so it would hit the Copilot MCP hang above. Redaction is opt-in per header, not "anything named `*-key`". |
| **mitmproxy** (Python), `server_replay` + record (`-w`) | yes: `--mode reverse:https://api…` is a base-URL target; regular mode also covers an agent with no base-URL setting | flows stored whole; `stream_large_bodies` streams but then *does not store* | needs an addon | `server_replay_ignore_params` (query) and `…_ignore_payload_params` (form bodies only); JSON needs an addon | `modify_headers`, or an addon before save | **Would have done, with about a hundred lines of addon** (a custom hash, the JSON-RPC id rewrite, header scrub on save). The best fallback, and the only tool here that could intercept an agent with *no* base-URL override (CA trust permitting). The cost is a Python runtime and a binary flow format in the test path. |
| **Hoverfly** (Go) | yes: `hoverfly -webserver` is a base-URL target; capture/simulate modes | whole bodies; simulation schema has delays (`fixedDelay`, `logNormalDelay`), no chunk list or SSE | `jsonpath` / `jsonPartial` matchers by hand | matchers (`json`, `jsonPartial`, `jsonpath`, `regex`), edited after capture | captures only the headers you ask for; middleware can strip | Capable, but a capture becomes an exact-match simulation that you then hand-loosen. No stream framing beyond whole bodies. |
| **WireMock** (JVM) record mode | yes: proxy and record, `/__admin/recordings/start` | recorded bodies whole; SSE exists only as authored `openSseChannel()` stubs in the 4.0 beta, which reject proxying | `matchingJsonPath` by hand | JSON bodies recorded as `equalToJson` with `ignoreExtraElements`, `ignoreArrayOrder` (too loose for a baseline), or hand edits | `captureHeaders` opts *in* to request headers; we found no default credential scrub for what is stored | Heavyweight (a JVM in the nix shell). The default JSON match is loose in the wrong direction. |

## Verdict

Honest answer: **an existing tool would have done most of it.** mitmproxy in reverse mode plus a small addon covers
all five needs, and proxay covers 1, 2 (bytes, not chunks) and 5 out of the box. No Rust crate does, because the
Rust options either live inside the client (rvcr) or have no record mode (wiremock-rs, mockito). httpmock 0.8 is the
exception, and it lacks the key normalization and the secret defaults. Ours is about 1,150 lines of Rust over hyper.
What it has that the others lack without extra work:

- an **exact-or-loud-miss** key over a normalized body (a miss is a `599` naming the key, plus a `miss-*.json`
  file), rather than nearest-match or "ignore extra fields";
- the **JSON-RPC id rewrite** and a method-aware loose key. This is the bug that cost 152 s per Copilot cell, and no
  tool above does it by default;
- **secrets dropped by name pattern by default**, enforced by a test over the committed cassettes;
- **chunk-for-chunk replay with timing**. We have not shown that any of the seven agents depends on chunk
  boundaries rather than bytes, so this is a nice-to-have, not a reason;
- it builds in the same `cargo test` and nix shell, with no Python, Node or JVM.

**Recommendation: keep ours, and borrow.**
- From **proxay**: *mimic* mode (record on a miss, replay on a hit) for re-recording one cell. Also a per-test cassette
  switch over HTTP (`POST /__proxay/tape`) instead of one directory per process.
- From **vcrpy / VCR**: the record modes (`none`, `once`, `new_episodes`, `all`) as the vocabulary for
  `WEBMOCK_MODE`, and `before_record_*` hooks in place of a hard-coded scrub.
- From **vcr-cassette**: weigh whether a cassette should be a VCR-format file, so other tools can read it. Our
  per-chunk timing would have to live in an extension field.
- **mitmproxy** stays the escape hatch for an agent that turns out to have no base-URL setting, and the tool for
  looking at a live exchange by hand. Not a dependency of the tests.

Swap only if ours grows past "one loopback server and a key function". In that case, mitmproxy with an addon is the
target.

## Sources
- rvcr: https://github.com/ChorusOne/rvcr (README: reqwest-middleware, `with_modify_request`, `VCRReplaySearch`)
- httpmock: https://httpmock.rs/record-and-playback/recording/ (forwarding record, standalone server)
- wiremock-rs: https://docs.rs/wiremock · mockito: https://docs.rs/mockito
- vcr-cassette: https://docs.rs/vcr-cassette
- vcrpy: https://vcrpy.readthedocs.io/en/latest/advanced.html · pytest-recording: https://github.com/kiwicom/pytest-recording
- proxay: https://github.com/airtasker/proxay (README, `src/cli.ts`, `src/sender.ts`, `src/similarity.ts`)
- mitmproxy: https://docs.mitmproxy.org/stable/concepts/options/ (`server_replay*`, `stream_large_bodies`,
  `modify_headers`, `reverse:` mode)
- Hoverfly: https://docs.hoverfly.io/en/latest/pages/reference/simulationschema.html
- WireMock: https://wiremock.org/docs/record-playback/ and the 4.0 beta's `SseAcceptanceTest` (`openSseChannel`
  rejects proxying)
