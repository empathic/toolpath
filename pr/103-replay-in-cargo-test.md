# test(webmock): replay checked by cargo test, one test per agent (design + UNRUN tests)

Status: draft · renumbered on landing (author label pr/101) · Base: pr/102 · Source: Bobby 2026-10-21 "make the replay row green"; NEXT items 10, 11 · tests not run in this piece; first run, and green after fixes, in pr/107 (`just replay-agents`)

## Why this is UNRUN
Every replay starts a loopback server. This seat's sandbox forbids `bind` (the same EPERM as the 31 port-bind test failures), so no replay test can be run here, red or green.
The file compiles and is clippy-clean (`cargo clippy -p toolpath-webmock --tests -D warnings`); its tests "pass" here only by skipping, and say so (`SKIPPED <agent>: <why>`). Nothing is claimed for replay.
To verify: run where a port binds (needs `sandbox.network.allowLocalBinding: true`, or scratch-main-82) with cells and the CLIs present.

## Design
- `crates/toolpath-webmock/tests/replay_agents.rs`: one test per agent (copilot, claude, pi, opencode, gemini), each calls `run::run` with `RunMode::Replay`, the committed cassettes and the probe command copied from `cassettes/<agent>/provenance.json`, then asserts rc 0, `miss == 0`, and that something was served (exact or loose).
- Cells: `<WEBMOCK_CELLS>/<agent>/{home,project,sid}`, built by the resume matrix (`path p derive` the golden, `path resume --harness <agent>`). Making that a library step (so no env var is needed) is the next piece; it needs a `path` binary and the same bind permission to verify.
- Never silent green: a missing cell or CLI prints `SKIPPED`; `replay_zz_coverage` fails when `WEBMOCK_REQUIRE=copilot,claude,...` names an agent that did not run. CI sets it; a developer without the CLIs does not.
- Per-agent expectations (from cassettes/README.md): copilot EXACT (10 requests, the GitHub user cassette), claude LOOSE (date-changed reminder, one tool schema), pi/opencode/gemini run in place (`in_place = true`; their stores are keyed by absolute project path; gemini's cassette has the request only, no complete response, so its test may legitimately end non-zero: UNKNOWN until run).
- gemini: the CLI is not installed on this machine; that cell stays UNKNOWN.
- The CLI under test must be the pinned one (pr/92 `agents.lock`); until that exists the CLI on PATH is used and the test should print its version.

## Codex (NEXT 10), plan only
ChatGPT-auth codex refuses a plain-http backend. First probe, no spend: an API-key codex with `OPENAI_BASE_URL` or `model_providers.<id>.base_url = "http://127.0.0.1:<port>/v1"` in a temp `CODEX_HOME`. If it takes http, replay like the others. Only if it refuses: a throwaway-CA TLS listener trusted through `SSL_CERT_FILE` in the test environment only. The probe needs a bound port, so it is not done here.

## Cursor (NEXT 11), plan only
cursor-agent talks HTTPS (likely HTTP/2 + Connect/protobuf) to Cursor's backend. Needs the proxy/base-URL override found, a record on the free plan (no spend), raw frames stored in the cassette. Not started: needs network and a bound port.

## Not run
Everything above that runs a server or a CLI. Compile and clippy only.
