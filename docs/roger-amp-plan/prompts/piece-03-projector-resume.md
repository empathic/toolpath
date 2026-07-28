# Piece 03 — projector-resume (goal level 2)

You are a fresh Claude Code session building one piece of the Amp harness for toolpath. Self-contained; execute end to end. **Opens with ⚠ recon** — Roger go-ahead before every live/fabrication step; fresh ids only; never create or mutate Amp-owned stores; never touch existing thread ids.

## Context to load first

`CLAUDE.md`; `roger-amp-plan/PLAN.md` (Piece 03 — the three-route fork is the decision tree); `roger-amp-plan/BUILD_LOG.md`; `docs/agents/formats/amp/{resume-and-sessions,RECON}.md`; templates: `crates/toolpath-copilot/src/project.rs`, `crates/path-cli/src/cmd_export.rs:372-568` (build/project/write/register — note `view.id = Uuid::new_v4()` at `:384` and the `if !db_path.exists()` guard at `:545`), `cmd_resume.rs` (`argv_for:403`, `project_into_harness:462`, stale hint `:350`), `tests/resume.rs:110-142`, `scripts/verify-copilot-live.sh`, `docs/agents/formats/copilot-cli/writing-compatible.md` (the rejection-table shape).

**Assumed done:** 00–02 (tags `amp-m0..m2`).

## Scope

(1) ⚠ Recon: resume semantics (`amp threads --help`; local vs server replay) + the writer-feasibility probe under a fresh id. Route per PLAN.md: local-write / API-create / documented-infeasibility. (2) `AmpProjector` (`ConversationProjector`; `native_name` total mapping + reclassification invariant test; position-stable ids; preserved delegation ids; token re-expansion inverse of the piece-01 pattern). (3) `cmd_export.rs`: `ExportTarget::Amp` 3-mode, `build_amp_session` (fresh UUID mint + cwd root), `project_amp`, `write_into_amp_project` (INSERT-only registration, warn-don't-fail, preview banner) **+ the `project_amp` unit test copilot never had**. (4) `cmd_resume.rs`: source arm, `agent:amp` sniff, `argv_for` per recon (expected `["threads","continue",<id>]`), `project_into_harness` arm, fix `:350` to include `copilot, amp`. (5) ⚠ The loop: fabricate → real `amp` rejects → record the **verbatim** rejection in `docs/agents/formats/amp/writing-compatible.md` `[observed, <ver>]` → fix → repeat until it loads; verify on the trivial AND feature-elicit captures. (6) `scripts/verify-amp-live.sh` (isolated home per dossier Q3; loader grep; probing question), shellcheck-clean. (7) `tests/resume.rs` RecordingExec case + argv/sniff units.

### Out of scope

Cross-harness into CC (04) / CC→amp + matrix (05) / bookkeeping (06).

## Definition of done

`path resume <amp-shared input> --harness amp -C <dir>` resumes in the real `amp` and answers "In one sentence, what was the most-used tool in this session?" correctly; `writing-compatible.md` has the verbatim rejection table + a `✅ Verified (amp <version>)` section with a reproduce recipe; `bash scripts/verify-amp-live.sh` passes; gates green. Infeasibility route: evidence documented + green export-to-file path + Roger reviews before the tag. BUILD_LOG entry (ADR: the route taken). Tag `amp-m3`. Stop.
