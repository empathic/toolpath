# docs: every public claim carries its evidence; kind v1.0.0 is read-only legacy

Status: ready · Base: main · Independent

Commits: `docs(claims): README support table has a "Verified (2026-10-15)" column…` · `docs(kinds): v1.1.0 is the only version producers write; v1.0.0 read-only legacy…` · `path-cli: tests that every producer writes the current kind; validate warns on v1.0.0` (61c923d | 5f82a35 | 4a3d18c).

## Why
An audit of 77 public claims (README, toolpath.net pages, format references, crate READMEs, CHANGELOG) against the goldens, the resume matrix and the tests found 25 proven, 43 partly, 3 unproven and 3 false. The false ones: "matrix in CI" (main has no test workflow), the site's "with everything the last one knew", and the RFC naming v1.0.0 as the only kind. Resume ✓ for pi and opencode had been published while both were broken.

## What changes
- README support table gains a "Verified (2026-10-15)" column: resume ✓ only where an agent answered from projected history (claude 2.1.285, codex 0.162.1, pi 0.84.3, copilot 1.0.61); gemini 0.47.0 and opencode 1.18.18 "rendered, no model turn" at audit time; cursor 3.24.12 "opens workspace, composer listed, body not opened". Lossless/verified wording scoped to the unit test or fixture that backs it; opencode's snapshot diffs marked implemented but untested; site index loses "any agent", "everything the last one knew", "full session", "resumes natively".
- Kinds: a Status column on the index; v1.1.0 is the only version producers write; v1.0.0 is read-only legacy, not supported for writing, with its erratum, on the index, landing, both version pages, README and RFC.
- path-cli: `every_producer_writes_the_current_kind` (seven fixtures, one derive each) and `no_source_spells_the_superseded_kind_outside_the_legacy_constant`; `path p validate` prints a `warning:` per v1.0.0 path and exits 0 (immutable URIs stay valid; archives carry it).

## Verification
fmt and clippy clean; `cargo test -p path-cli` 485+ green, `-p toolpath` 72. Audit with the per-claim evidence and the 11 undecidable items: `review/claims-audit.md`. Pathbase's own wording (welcome/about: "full history", "exact state", "any agent"; the app listing pi while its goldens pin claude and codex) is proposed there, not changed here.

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs,
a file touched by several PRs belongs to the earliest):
```
glob:"site/**"
README.md
RFC.md
crates/path-cli/src/cmd_validate.rs
crates/path-cli/src/kinds.rs
crates/path-cli/tests/cross_harness_matrix.rs
crates/toolpath-copilot/README.md
crates/toolpath/src/types.rs
docs/agents/formats/copilot-cli/writing-compatible.md
docs/agents/formats/cursor.md
docs/agents/formats/gemini.md
```
