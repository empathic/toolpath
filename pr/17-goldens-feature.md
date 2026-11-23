# feat(path-cli): `path goldens` behind a `goldens` feature, off by default

Status: planned (asked 2026-10-19 on the demo deck: "goldens only under a development flag") · Base: the goldens stack once merged

## Why
`path goldens` (capture, check, diff) is a development tool, but it ships in the release `path` binary: it was gated
only off emscripten, and release.yml builds `-p path-cli --bin path` with default features. This answers the open
qualifier concern on lib.rs ("goldens machinery lives inside path-cli, the shipped binary") the way the repo already
does experimental surface: `resume-remote` is a Cargo feature, off by default.

The goldens tests are not made skippable. They require the feature (`required-features`), and every gate passes
`--features path-cli/goldens`, so the per-PR gate (decided 2026-10-16) still runs them. A gate without the feature
is the silent-pass failure mode, which is why the flag is a build feature and not an environment variable.

## What changes
- `crates/path-cli/Cargo.toml`: `goldens = []` feature; `tests/goldens.rs` and `tests/goldens_producer.rs` declare
  `required-features = ["goldens"]`.
- `crates/path-cli/src/lib.rs`: `mod goldens`, the `Goldens` subcommand and its dispatch are
  `cfg(all(not(emscripten), feature = "goldens"))`; a unit test asserts the subcommand exists exactly when the
  feature is on.
- `scripts/quality_gates.sh`, `justfile`, `pr/cut.sh`, `pr/gate-piece.sh`: clippy, test and doc pass
  `--features path-cli/goldens`.
- `scripts/goldens.sh`: `cargo run -p path-cli --features goldens -- goldens ...`.
- No golden content changes: `goldens` and `goldens_producer` pass unchanged (the moved harness/producer pins are
  info rows for a dev build, not failures).

## Verification
- Default build: `path --help` lists no `goldens`; `cargo test -p path-cli --lib goldens_subcommand` passes (absent).
- With the feature: the same unit test passes (present); `cargo test -p path-cli --features goldens --test goldens
  --test goldens_producer` passes (1 + 5 tests, 2026-10-19).

## Paths
```
crates/path-cli/Cargo.toml
crates/path-cli/src/lib.rs
scripts/quality_gates.sh
scripts/goldens.sh
justfile
```
(`pr/cut.sh` and `pr/gate-piece.sh` change on wip only; PR branches never carry `pr/`.)
```
