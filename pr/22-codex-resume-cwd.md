# test(goldens): retire claude-to-codex-caller-cwd; the resume directory is the codex cwd by design

Status: planned (defect sweep 2026-10-19: "projecting a claude session to codex writes the source cwd as `/`") · Base: the goldens stack once merged (pr/17)

## Why
`goldens/known-defect/claude-to-codex-caller-cwd.tsv` and `known_defects@claude.snap` pinned "claude -> codex writes
`/` where the source session's cwd was `…/toolpath-elicit.nu6ZWB/claude`" as a defect, with the source cwd as the
value a fix would write. Checked against what a real user run does and what codex needs, it is not a defect:

- **What a real run writes.** `path resume --harness codex -C <dir>` (`cmd_resume::project_into_harness` ->
  `projection::codex::project_codex`) canonicalizes `<dir>` and hands it to `CodexProjector::with_cwd`;
  `path export codex --project <dir>` does the same, and `-o` without `--project` uses the shell's cwd. That value is
  every `session_meta.payload.cwd`, every `turn_context.payload.cwd` and the `cwd` of the `state_5.sqlite` thread
  row. The source session's cwd is only the projector's fallback when no cwd is given. Unchanged since the projector
  landed; the `--project` help says so ("the session_meta line carries this path as `cwd`, so `codex resume` invoked
  from the same directory will resolve it").
- **What codex needs.** `codex resume`'s picker lists only sessions whose cwd is the directory it runs in
  (`codex resume --help`, codex 0.162: "`--all` Show all sessions (disables cwd filtering …)"). A rollout carrying the
  source cwd would be missing from the picker in the directory the user resumed in, and the source cwd is usually a
  path that does not exist on the resuming machine (here a deleted macOS temp dir). Pi and gemini key their project
  on the same directory (pi's `--<cwd>--` session dir, gemini's `projectHash` and `directories`).
- **Why the goldens wrote `/`.** The hermetic harness runs every `p export` from `/` (`Goldens::hermetic`), and the
  in-process pipeline passes the same value (`toolpath_goldens::pipeline::CALLER_CWD`). `/` is the caller's
  directory there, i.e. the contract working as intended. The pin compared it to the source cwd, so a change that
  wrote the source cwd would have been judged FIXED by `judge_cwd` while breaking resume.

Neither the projector nor the harness setup is wrong; the pin's reference value was. Behaviour is unchanged.

## What changes
- `crates/toolpath-goldens`: the `claude-to-codex-caller-cwd` defect, `defects::caller_cwd`, its unit test and
  `Finding::CallerCwd` are removed; `CALLER_CWD`'s doc says what `/` stands for (same line count, so the other anchors
  in `pipeline.rs` stay fresh). `snapshots/known_defects@claude.snap` loses that one entry (accepted with
  `INSTA_UPDATE=always`); every other snapshot is byte-identical.
- `crates/path-cli/src/goldens.rs`: the same defect is removed from `DEFECTS` with `caller_cwd`, `judge_cwd` and the
  `SetRun.source_cwd` / `place()` plumbing that only fed it; `write_defects` now drops manifest `known_defects` entries
  whose defect is no longer in `DEFECTS` (before, a retired defect stayed in the manifest forever).
  `goldens/known-defect/claude-to-codex-caller-cwd.tsv` is deleted; `goldens/manifest.json` recaptured with
  `path goldens capture --all` (the entry gone, environment pins refreshed; no output sha moved).
- `crates/path-cli/src/projection/codex.rs`: `project_codex_writes_the_resume_directory_as_cwd_not_the_source_cwd`
  pins the contract: a source whose turns carry `/source/session/project` projected with `-C <tmp>/proj` writes the
  canonical `-C` dir in `session_meta` and every `turn_context`, never the source cwd.
- `docs/GOLDENS.md`, `goldens/DEMO.md`: the defect list drops it and says why.
- qualifier: the concerns on `toolpath-codex/src/project.rs` (3ed731d5) and on `CALLER_CWD` (90e64df3) are resolved
  `reason:invalid` with the explanation above; a `comment` on `CALLER_CWD` keeps it at the code; the defect-count
  concern on `defects.rs` is superseded (six -> five); the `--allow-all-tools` concern in `goldens.rs` is re-anchored
  (its line moved, text unchanged).

## Open
- `p export codex -o <file>` with no `--project` writes the shell's cwd. That is consistent with the other modes and
  with what `codex resume` needs when the file is then dropped into `~/.codex/sessions` from the same shell, but an
  export meant to be read elsewhere might prefer the source cwd. A product decision, not pinned either way here.

## Verification
`scripts/quality_gates.sh` with `--features path-cli/goldens`: fmt, clippy `--workspace --all-targets -D warnings`,
`RUSTFLAGS=-D warnings cargo test --workspace`, `RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps`,
shellcheck; `path goldens check` 0 problems before and after the recapture; `qualifier review` all fresh.

## Paths
```
crates/toolpath-goldens/src/defects.rs
crates/toolpath-goldens/src/model.rs
crates/toolpath-goldens/src/pipeline.rs
crates/toolpath-goldens/src/.qual
crates/toolpath-goldens/snapshots/known_defects@claude.snap
crates/path-cli/src/goldens.rs
crates/path-cli/src/projection/codex.rs
crates/path-cli/src/.qual
crates/toolpath-codex/src/.qual
goldens/known-defect/claude-to-codex-caller-cwd.tsv
goldens/manifest.json
goldens/DEMO.md
docs/GOLDENS.md
pr/22-codex-resume-cwd.md
```
