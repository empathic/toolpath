# docs: DEVELOPMENT.md and the changelog use `path d` and its just recipes

Status: planned (follow-up to pr/39, once `path d` landed in e8b4ea7) · Base: the goldens stack once merged · Docs only

## Why
pr/39 wrote the baseline loop against `cargo test -p toolpath-goldens` and `scripts/goldens.sh`, naming `path d
baseline` / `just baseline` as landing. They have landed (e8b4ea7, with `just webmock` and `just model`).

## What changes
- `DEVELOPMENT.md`: step 2 runs `just baseline` (with `diff`/`shed`/`list`/`capture --all`) beside the
  insta test; adding a harness records its cassette with `just webmock`; the object model regenerates with `just
  model`; a "Format notes" recipe points at `docs/agents/formats/observed/` and its test; the gate features are
  `path-cli/develop,toolpath/verify`.
- `CHANGELOG.md`: the path-cli line describes `path d` (baseline, webmock, model) behind `develop`, and the
  breaking rename `path goldens` → `path d baseline` (`goldens` an alias).

## Not removed, and why
- `scripts/goldens.sh` stays: it is in the goldens harness's pinned file set (`crates/path-cli/src/goldens.rs`
  `HARNESS_FILES` and the code-sha globs, `tests/goldens_producer.rs`), so deleting it changes every manifest
  entry's pins and needs a recapture; that belongs with the crate rename to "baseline", not in a docs piece.

## Verification
Docs only; every recipe named is in `justfile` at this tip.

## Paths
```
DEVELOPMENT.md
CHANGELOG.md
pr/55-development-uses-path-d.md
```
