# feat(toolpath): the RFC's signature algorithm, proven — JCS inputs, SHA-256 digests, ssh verification

Status: planned (asked 2026-10-19 on the demo deck: "we can prove these") · Base: main

## Why
RFC.md's "Signature Algorithm" and "Verification Algorithm" sections were prose only: no toolpath crate
canonicalized, digested or verified anything, and the examples' signatures were placeholders nobody checked.
JCS appeared only in path-cli behind `resume-remote` (content-addressed session ids).

## What changes
- `toolpath` gets a `verify` feature (off by default): `v1::sign::{jcs, digest, step_author_input,
  path_author_input, path_reviewer_input, verify_step_signature, verify_path_signatures, key_id}`.
  JCS is written in the crate (RFC 8785; ECMAScript numbers via `ryu-js`, not serde_json_canonicalizer, which would turn on serde_json `float_roundtrip` for the whole build); digests are SHA-256; `ssh` keys verify Ed25519 SSHSIG
  signatures in namespace `toolpath`; `gpg`/`sigstore` are refused as unsupported. Keys come from a caller
  resolver (the document carries fingerprints, not key material); the resolved key's SHA256 fingerprint
  must match.
- `tests/jcs.rs`: the six RFC 8785 reference vectors (vendored, Apache-2.0, `tests/jcs-vectors/SOURCE.md`),
  escape set, ES6 numbers, the RFC's canonicalization example byte for byte, what gets signed, signed inputs
  surviving a `.path.jsonl` round trip, and 2000 generated values (re-parse equal, idempotent, no
  whitespace, structure/key order equal to an independent writer).
- `tests/signatures.rs`: 14 tests with keys made from fixed seeds — signed step and path verify; meta is
  not signed; tampered change / step / head / order fail; missing scope, unknown signer, wrong scope's
  canonical form, wrong key, lying resolver, missing reviewer timestamp, gpg/sigstore, unsupported scopes.
- Gates (`scripts/quality_gates.sh`, `justfile`, `pr/cut.sh`, `pr/gate-piece.sh`) build with
  `--features path-cli/goldens,toolpath/verify`, so these tests run on every gate.
- Qualifier records on every rule and pseudo-code line of the section (RFC.md 395, 712–836), on
  `jsonl.rs`'s round-trip claim, and on `examples/path-02-local-session.path.jsonl`.

## For reviewers: serde_json `float_roundtrip`, workspace-wide
This piece turns on serde_json's `float_roundtrip` in the workspace `Cargo.toml`, which **changes how every
crate parses floats**. Without it serde_json's fast parser can land one ULP away from the nearest double, a
silent precision change in real sessions: Claude Code's `"costUSD": 0.020428699999999998` was read back as
`0.0204287`, and RFC 8785's own vector `333333333.33333329` canonicalized wrong. With it every number parses to
the nearest double, which RFC 8785 (and so every signature) assumes. Three snapshots change for exactly that
value (`ir@`, `document@`, `shed@claude-session`).

## Findings
- **RFC.md:719 is false** (blocker): RFC 8785 sorts keys by UTF-16 code units, not UTF-8 bytes. The
  implementation follows RFC 8785, as its own reference vector `weird.json` requires.
- A path author signature covers step ids, not step contents (concern on :759).
- `meta.actors[*].keys` has no key material (concern on :823); witness/ci/release have no canonical form
  (concern on :395); gpg and sigstore are not verified (concerns on :834, :836); the SSHSIG namespace is
  unspecified (noted on :835).
- `examples/path-02-local-session.path.jsonl` differed from its `.path.json` (no `base.ref`); 8813140 fixed the fixture and the pin now requires every committed pair to match.

## Verification
- `cargo test -p toolpath --features verify --test jcs --test signatures`: 9 + 14 pass.
- `qualifier review`: the 20 new records fresh.
- Full gates with `--features path-cli/goldens,toolpath/verify` (see the commit).

## Paths
```
crates/toolpath/Cargo.toml
crates/toolpath/src/lib.rs
crates/toolpath/src/sign.rs
crates/toolpath/src/.qual
crates/toolpath/tests/jcs.rs
crates/toolpath/tests/signatures.rs
crates/toolpath/tests/jcs-vectors/
Cargo.lock
.qual
examples/.qual
scripts/quality_gates.sh
justfile
```
(`pr/cut.sh` and `pr/gate-piece.sh` change on wip only.)
