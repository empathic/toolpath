# test(webmock): no_secrets gate over the committed fixture surfaces; cassette provenance

Status: opened #370 (draft, 2026-10-16) — was: ready · Base: pr/13-webmock · Independent of the goldens and resume PRs

Commits (signed, on wip after the squash): `fix(webmock): x-gemini-api-privileged-user-id is a secret header…`, `test(webmock): no_secrets gate…`, `docs(webmock): cassette provenance…`, `fix(quality-gates): the secrets gate scans . from the repo root…`, `fix(webmock): needless_borrow at up_headers_for_client`.

## Why
A real Gemini API key reached two scratch cassettes because `x-goog-api-key` was not on the redaction list, and a grep by hand caught it. Bobby asked for tooling that makes committing a credential or a private identity impossible, and for every webmock cassette to be traceable to the exact golden session it was recorded from. The gate also found what a plaintext grep cannot: the recording machine's scratch path inside the base64 response chunks of the pi cassette, and the Google account id behind the key (`x-gemini-api-privileged-user-id`) in both gemini cassettes. Both are scrubbed in these commits.

## What changes
`.gitleaks.toml` at the repo root is the one place for rules and allowlist (each entry commented with its reason): `sk-ant-`, Google (`AIza`, `AQ.`), GitHub, OpenAI, Slack, AWS, private-key blocks, JWT, bearer tokens, a non-redacted credential header in JSON, and the machine-identity shapes (the agent scratch-dir prefix, the encoded-home slug and the user-at-domain address repo-wide; `/Users/<name>` on the fixture surfaces; the toml spells them out, this file deliberately does not). Two consumers read it:

- `crates/toolpath-webmock/tests/no_secrets.rs`: parses the toml, walks the fixture surfaces (`cassettes/`, `toolpath-goldens/snapshots/`, `goldens/`, `test-fixtures/`, `crates/*/tests/fixtures/`), **decodes base64 runs of 64+ chars and scans the decoded text**, and fails listing `path:line rule-id <first 8 chars>… (N chars)`, never the whole value. A second test fires a runtime-assembled sample through every rule, plain and base64, and asserts `<redacted>` / `Bearer $TOKEN` pass.
- `scripts/quality_gates.sh` gate `secrets`: `gitleaks dir . --config .gitleaks.toml` from the repo root (so the path allowlist applies; with an absolute root every path came back absolute and `target/` produced 1888 findings), skipped with a message when the binary is absent. `pkgs.gitleaks` is in the flake dev shell.

`crates/toolpath-webmock/tests/provenance.rs` checks each `cassettes/<agent>/provenance.json` (golden set, `input_sha256`, session id, cell, toolpath rev, file list) against the tree: the input's sha, the session id in the input, the directory's file list. `src/lib.rs`: `x-gemini-api-privileged-user-id` added to `SECRET_HEADERS`; `needless_borrow` fix at `up_headers_for_client` (clippy `-D warnings` under the 1.94 pin was red on wip before it).

By the earliest-wins rule some of this series' files ride in earlier PRs or stay held: `Cargo.lock` in 02, `crates/toolpath-webmock/{Cargo.toml,src/lib.rs}` and `cassettes/pathbase/provenance.json` in 13, the other six `provenance.json`, the two scrubbed gemini cassettes, the scrubbed pi cassette, `cassettes/README.md`, `tests/secret_headers.rs` and `flake.nix` in HOLD with the cassette directories they belong to. This PR is the toml, the two tests and the gate script; it stacks on 13 for the dev-dependencies.

## Verification
`cargo test -p toolpath-webmock --test no_secrets` 2/2 (1.5 s over 16 MB), `--test provenance` 1/1, `--test secret_headers` 2/2; `cargo clippy -p toolpath-webmock --all-targets -- -D warnings` clean; `scripts/quality_gates.sh shellcheck secrets` PASS with `target/` present; gitleaks over the whole tree: no leaks (24 MB). Negative check: three planted fakes under `goldens/shed/` failed the test with four listed lines; file removed. Concern documented, not scrubbed: `test-fixtures/cursor/convo.json` carries two Cursor local-store encryption keys (on `main` since the fixture landed; pinned by ~20 cursor insta headers, so scrubbing is a re-pin).

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs,
a file touched by several PRs belongs to the earliest):
```
.gitleaks.toml
crates/toolpath-webmock/tests/no_secrets.rs
crates/toolpath-webmock/tests/provenance.rs
scripts/quality_gates.sh
```
