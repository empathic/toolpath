# `docs/agents/adding-a-projector.md` — still-true / superseded delta

Produced 2026-07-27 during the Amp planning session (Phase-1 subagent 6,
verified against `main` @ `192d726`). Feeds piece 06's refresh of the doc.
The doc predates the `toolpath-cli` → `path-cli` rename and the entire
Copilot era; Ben flagged it as out of date — this table is the claim-by-claim
verdict.

| # | Claim / section (doc line) | Verdict | Current truth |
|---|---|---|---|
| 1 | Title: "Adding `path p export <harness>`" (`:1`) | Still true, incomplete | `p export` exists (`crates/path-cli/src/cmd_p.rs:35`); retitle for adding a *harness* (share + resume + export). |
| 2 | "Distilled from Gemini; should generalize to Codex, opencode, Pi" (`:5`) | Superseded (historical) | All of those plus Cursor and Copilot shipped; the template is now `toolpath-copilot`. |
| 3 | Mental model `Provider::to_view → View → derive_path`; projector = `extract_conversation → project` (`:13-22`) | Still true (conceptually) | Correct pipeline, but `to_view` is a free fn per crate, not a trait method (`crates/toolpath-copilot/src/provider.rs:138`). |
| 4 | IR canonicalizes `ToolCategory`, names stay verbatim (`:24-27`) | Still true | `crates/toolpath-convo/src/lib.rs:206`, `:226-235`. |
| 5 | Prereq: `to_view` populates `ToolInvocation.category` (`:33`) | Still true | `lib.rs:235`. |
| 6 | Prereq: provider data in `Turn.extra["<harness>"]` (`:35-37`) | **SUPERSEDED** | `Turn` has no `extra` field (removed in `0452f61`); see `crates/toolpath-gemini/src/project.rs:136-141`. Projectors use typed IR fields. |
| 7 | Prereq: format doc at `docs/agents/formats/<harness>.md` (`:38`) | Superseded for preview harnesses | Directory treatment (`docs/agents/formats/copilot-cli/`, 9 files); `formats/README.md:59-62`. |
| 8 | `native_name(...) -> Option<&'static str>` (`:44-75`) | Partly true | Copilot's returns bare `&'static str` (total): `crates/toolpath-copilot/src/provider.rs:92`. Option-form survives in gemini/claude/codex/opencode/pi. |
| 9 | `impl ConversationProjector { type Output; fn project }` (`:77-95`) | Still true | `crates/toolpath-convo/src/project.rs:33-39`. |
| 10 | MUST-do "drop foreign-namespace extras… `split_gemini_extras`" (`:99-103`) | **SUPERSEDED** | Mechanism removed with `Turn.extra`; `split_gemini_extras` no longer exists. |
| 11 | MUST-do "remap tool names via category + `native_name`" (`:104-106`) | Still true | Enforced by `real_fixture_roundtrip.rs` + the matrix. |
| 12 | MUST-do "synthesize UI fields… extras carry originals" (`:107-114`) | Half superseded | Synthesis still required (`synthesize_description` `crates/toolpath-gemini/src/project.rs:415`); the extras half is dead. |
| 13 | Library/CLI parity for session resolution (`:116-123`) | Still true | `crates/toolpath-gemini/src/paths.rs:325`; copilot analogue honors `COPILOT_HOME`. |
| 14 | "In `crates/toolpath-cli/src/cmd_export.rs`" (`:127`) | **SUPERSEDED (path)** | `crates/path-cli/src/cmd_export.rs:29`; `toolpath-cli` is a 2-line shim. |
| 15 | Three-mode design `--project`/`--output`/stdout (`:145-159`) | Still true | `cmd_export.rs:120-140`; also add the `build_<h>_session`/`project_<h>` split (`:372`/`:397`) — `path resume` calls `project_<h>` directly (`cmd_resume.rs:466`). |
| 16 | "Mirror Claude's variant exactly" (`:145`) | Superseded | Mirror **Copilot's** (newest both-directions preview harness, writes an index row). |
| 17 | "Document format at `formats/<harness>.md`" (`:161-176`) | Superseded (shape) | Content bullets still apply, but as a directory incl. `writing-compatible.md` + `known-gaps-and-sourcing.md`. |
| 18 | Three test layers (`:179-198`) | Still true, now five | Add (4) the cross-harness matrix row (`crates/path-cli/tests/cross_harness_matrix.rs:19`) and (5) a real-capture fixture test (`real_fixture_roundtrip.rs`). |
| 19 | §7A full-pipeline check via `p import` (`:210-228`) | Still true | Commands valid on current CLI. |
| 20 | §7B CLI-accepts + probing question (`:230-243`) | Still true — now the DoD standard, automated | `scripts/verify-copilot-live.sh:45-60` (isolated home, rejection grep, probe question). |
| 21 | §7C diff-vs-real-session + Python analyzer (`:245-310`) | Partly superseded | Intent survives; practice is the live loader loop + pty TUI capture (`copilot-cli/known-gaps-and-sourcing.md:67-89`) and wire value-identity tests. |
| 22 | Pitfall: filename conventions load-bearing (`:314`) | Still true | Copilot analogue: the `session-store.db` row (`cmd_export.rs:480-497`). |
| 23 | Pitfall: identifier resolution ≠ filesystem layout (`:318`) | Still true | Cross-linked from `copilot-cli/resume-and-sessions.md:5`. |
| 24 | Pitfall: multi-file formats need a thoughtful `--output` (`:322`) | Still true | Copilot writes three artifacts across two stores. |
| 25 | Pitfall: extras leak via `#[serde(flatten)]` (`:327-331`) | **SUPERSEDED** | No `Turn.extra` to leak. Replace with the real pitfall: never mutate existing harness state — fresh-id INSERT-only (`cmd_export.rs:382`). |
| 26 | Pitfall: map names not args across harnesses (`:332`) | Still true | Matrix-enforced. |
| 27 | Pitfall: UI decoration fields aren't cosmetic (`:338`) | Still true, sharper | Copilot: timeline dispatches on the `toolRequests` mirror; hunkless diffs render flat (`copilot-cli/file-fidelity.md:29+`). |
| 28 | Pitfall: the reader is the next surprise (`:343`) | Still true | — |
| 29 | Pitfall: don't trust commit comments about fallbacks (`:347`) | Still true | Generic hygiene. |
| 30 | "Concrete: Gemini reference" block (`:352-367`) | Superseded (2 entries + framing) | Fix the `toolpath-cli` path; add a Copilot reference block (`crates/toolpath-copilot/src/*`, `cmd_export.rs:372/397`, `cmd_resume.rs:466`, `cross_harness_matrix.rs`, `docs/agents/formats/copilot-cli/`, `scripts/verify-copilot-live.sh`). |

**Sections the refresh must add (absent today):** `path share` wiring and the
provider listing/picker/TSV contract; `path resume` wiring; `Harness` /
`Harness::ALL` / `HarnessBundle` / `ArtifactType` registration and the
cache-id-prefix rule; `ExecStrategy`/`RecordingExec` as the testing seam; the
writer-contract methodology (isolated home, one-rejection-at-a-time loop,
verbatim errors → `writing-compatible.md`); the cross-harness matrix row and
the feature-elicit fixture pipeline; preview-labeling + version-stamping
obligations; the release/versioning checklist pointer.
