# Piece 05 — cc-to-amp (goal level 4, stretch)

You are a fresh Claude Code session building one piece of the Amp harness for toolpath. Self-contained; execute end to end. ⚠ live steps as marked. **If piece 03 ended on the infeasibility route, do not start — record the skip in BUILD_LOG and stop.**

## Context to load first

`CLAUDE.md`; `roger-amp-plan/PLAN.md` (Piece 05); `roger-amp-plan/BUILD_LOG.md`; `crates/path-cli/tests/cross_harness_matrix.rs` (`trait Harness:19`, `CopilotHarness:127-168`, vectors `:1024-1033` + `:1075-1085`, invariants `:483-700`); `crates/toolpath-amp/src/{provider,project}.rs`.

**Assumed done:** 00–04.

## Scope

(1) `native_name` completeness for foreign sources: every `ToolCategory` maps to a real amp-native tool name whose args render in amp's UI; per-category reclassification invariant test; arg-shape remap where amp keys off arg names. (2) `AmpHarness` matrix row registered in **both** vectors; all 2N+1 new cells green (`cargo test -p path-cli --test cross_harness_matrix`). (3) Foreign-source round-trip test in `crates/toolpath-amp/tests/roundtrip.rs`. (4) ⚠ Live: share a real Claude Code session from this repo (`path share --harness claude …` or an existing cache doc), `path resume <it> --harness amp -C <dir>`, probing question in real `amp`.

### Out of scope

Bookkeeping (06).

## Definition of done

Matrix green incl. amp; live CC→amp probing-question pass; gates green; BUILD_LOG entry. Tag `amp-m5`. Stop.
