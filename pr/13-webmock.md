# test(webmock): record/replay for every harness's model API and for path ↔ pathbase

Status: draft (crate skeleton; consumers next) · Base: pr/12-goldens-all-harnesses · Independent of the resume fixes

Commits: `test(webmock): …` (703be69).

## Why
Every layer of the chain is pinned except the network ones. A recorded request is also evidence (gemini's restored history is visible in its request body when no probe answer is). With cassettes the resume matrix and capture-live run with no upstream and no token spend, and an upload failure is attributable to the client, the wire or the server.

## What changes
`crates/toolpath-webmock` (publish = false): an in-process server on loopback, the shape of pathbase's `MockGitHub` (#500). `RECORD` forwards upstream and writes a cassette (request normalized: method, path and query, body with the agent's volatile fields dropped and dates masked; auth headers never stored; response with SSE chunk framing); `REPLAY` serves cassettes and answers 599 with a miss file on a miss. Cassettes and the recorder live in the crate, so every fixture regenerates from the repo (#315). Five unit tests.

## Verification
Bun prototype (the spec, `scratchpad/webmock/`) proofs: claude, codex, pi, opencode and `path share --anon` replay exactly with no upstream and bogus credentials; gemini's request pin confirms restored history; copilot records but cannot replay (its remote-MCP tool list is nondeterministic). Overrides verified from the binaries: `ANTHROPIC_BASE_URL`, codex `chatgpt_base_url` (https only: loopback TLS + `CODEX_CA_CERTIFICATE`), pi `models.json`, opencode provider `baseURL`, `GOOGLE_GEMINI_BASE_URL`, `COPILOT_API_URL`, `path --url`. Report: `review/webmock.md`.

## Next
Consumers (an offline cell per agent in `toolpath-goldens`; the pathbase cassette in path-cli's tests), TLS + zstd for codex in the crate, JSON-RPC keying.

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs,
a file touched by several PRs belongs to the earliest):
```
glob:"crates/toolpath-webmock/cassettes/pathbase/**"
glob:"crates/toolpath-webmock/src/**"
crates/toolpath-webmock/Cargo.toml
crates/toolpath-webmock/tests/roundtrip.rs
```
