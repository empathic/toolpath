# test(webmock): pathbase cassettes on a provisional pin, self-built replay cells, pi/opencode replay fixes, a throwaway-CA TLS listener

Status: draft · Base: pr/115 · Source: toolpath-author batch 2026-10-23 (8 commits, patch: pr/patches/116-webmock-pathbase-replay-cells-tls.patch)

## Why
- pathbase's cassette was recorded against a server build that no longer matches pull/486 (streamed uploads). It is re-recorded from a local pathbase build, including the streamed-upload routes.
- Replay depended on cells prepared by hand. It now builds each cell itself with the `path` binary under test (`WEBMOCK_PATH_BIN`), so replay runs the same way on any machine.
- pi 0.84 refuses a credential with no `type` before it sends any request, and opencode needs its session db in the cell. Both are fixed in the replay setup.
- codex refuses a plain-http base URL. `serve_tls` gives webmock a per-run TLS listener with a throwaway CA (rcgen, pinned `=0.14.10`, dev-dependency only). The codex wiring is NOT done in this batch.

## What changes
- `crates/toolpath-webmock/cassettes/pathbase/*`: re-recorded; `provenance.json` names the build, the scratch Postgres 16.15 database and the OpenAPI sha256. **PROVISIONAL pin 5b02098** (ruling pending, Bobby).
- `crates/toolpath-webmock/src/run.rs`: cells are built with `WEBMOCK_PATH_BIN`; the pi credential always says `type: oauth`; opencode's db is created in the cell (loopback-only config).
- `crates/toolpath-webmock/src/tls.rs` + `tests/tls.rs`: `serve_tls`.
- README pathbase row names the pin and the streamed-upload cassettes.
- Landing fix (scratch-main-82): `.gitleaks.toml` allows `openapi_sha256` (a spec hash that gitleaks' generic-api-key rule read as a key), with its reason beside it.

## Landing notes
- Cherry-picked onto wip 32eb1a4b. One conflict, `run.rs` (pi credential type): wip already set `type` only when it was absent; the author's version sets it always. The author's version was taken; for a fresh cell both behave the same.

## Verification (scratch-main-82, nix develop, wip tree + this batch)
- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets --features path-cli/develop,toolpath/verify -- -D warnings`: clean.
- `cargo test --workspace --no-fail-fast --features path-cli/develop,toolpath/verify`: 110 test binaries ok, 2,281 passed, 0 failed.
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace … --no-deps`: clean.
- `scripts/quality_gates.sh secrets shellcheck`: 2/2 after the allowlist entry; `no_secrets` test: 2 passed.
- `qualifier review`: 238 checked, 0 drifted.
- `just baseline`: ok (1 passed), no snapshot changes.
- NOT run: a live codex replay over TLS (wiring not done), pinned 1.94.
