# ci(goldens): CI installs the pinned qualifier and the qualifier review fails closed there

Status: planned (asked 2026-10-19 on the goldens demo deck: "can we prove it?", gap "one step of the CI check") · Base: the goldens stack once merged

## Why
The goldens' qualifier review needs a `qualifier` binary. The GitHub runner has none, and both tests that run the
review (`crates/toolpath-goldens/tests/qualifier.rs`, `crates/path-cli/tests/goldens.rs`) skip it with a printed
note when the binary is missing. In CI that skip is a silent pass: the review the goldens claim to run never runs on
any PR.

## What changes
- `.github/workflows/deploy-site.yml`: before the quality gates, download qualifier 0.8.0's
  `qualifier-x86_64-unknown-linux-musl.tar.gz` from the empathic/qualifier v0.8.0 release, check it against its
  pinned sha256 (`f39f5252…6f1ec`, the release's own digest), put it on PATH and set `TOOLPATH_GOLDENS_QUALIFIER`.
- Both tests fail instead of skipping when the binary is missing and `CI` is set (anything but `false`/`0`; GitHub
  sets `CI=true`). A dev shell without qualifier still skips, as before.

## Verification (2026-10-19, local simulation of the CI env, qualifier removed from PATH)
- `CI=true`, no binary: `toolpath-goldens --test qualifier` FAILED ("qualifier review cannot run … CI is set");
  `path-cli --features goldens --test goldens` FAILED ("no qualifier binary and CI is set").
- `CI=true`, pinned binary via `TOOLPATH_GOLDENS_QUALIFIER`: both pass (1 + 1 tests).
- No `CI`, no binary: the qualifier test skips and passes, as before.
- The release asset's sha256 was checked by download (`shasum -a 256`) and the tarball holds the one `qualifier` binary.

## Paths
```
.github/workflows/deploy-site.yml
crates/toolpath-goldens/tests/qualifier.rs
crates/path-cli/tests/goldens.rs
```
