# docs(webmock): prior art — why our own record/replay, and what to borrow

Status: planned (2026-10-19, rehearsal feedback, Alex: "Why build our own record/replay instead of an existing
crate?") · Base: pr/13 (webmock) once merged · Independent, docs only

## Why
`toolpath-webmock` was built without a written comparison against existing record/replay tools. This piece is that
comparison, written after the fact and honest about the answer.

## What changes
- `docs/webmock-prior-art.md` (new): the five needs of the resume cells (base-URL target for an unmodified agent
  binary, SSE replay with framing, JSON-RPC by method with the live id, a normalized body key, no secrets in
  cassettes). Ten tools are scored on each: rvcr, httpmock, wiremock-rs, mockito, vcr-cassette, vcrpy and
  pytest-recording, proxay, mitmproxy, Hoverfly and WireMock (JVM). Then a verdict per tool and a recommendation.

## Answer
An existing tool would have done most of it. mitmproxy (reverse mode plus an addon of about a hundred lines) covers
all five needs. proxay covers base URL, stream bytes and redaction out of the box, but matches by similarity (the
closest recording, not a loud miss) and has no JSON-RPC id rewrite. That gap is the Copilot MCP hang that cost
152 s per cell. No Rust crate fits: rvcr is client middleware, wiremock-rs and mockito have no record mode, and
httpmock 0.8 records but lacks the key normalization and secret defaults. Recommendation: keep ours (about 1,150
lines, in `cargo test`, no extra runtime). Borrow proxay's mimic mode and per-test tape switch, and VCR's record-mode
vocabulary and `before_record` hooks. Consider the VCR cassette format. Keep mitmproxy as the escape hatch for an
agent without a base-URL setting.

## Verification
- Each tool's claims come from its docs or source, read 2026-10-19 (links in the doc). proxay from `src/cli.ts`,
  `src/sender.ts` (buffers the response) and `src/similarity.ts`. WireMock's SSE from the 4.0 beta's
  `SseAcceptanceTest` (`openSseChannel` rejects proxying).
- Our side: `crates/toolpath-webmock/src/lib.rs` and `cassettes/README.md`.
- Docs only: no cargo, no `site/` change.

## Paths
```
docs/webmock-prior-art.md
```
