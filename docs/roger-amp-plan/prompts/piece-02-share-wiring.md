# Piece 02 — share-wiring (goal level 1)

You are a fresh Claude Code session building one piece of the Amp harness for toolpath. Self-contained; execute end to end. One ⚠ step (live Pathbase upload) — get Roger's go-ahead there.

## Context to load first

`CLAUDE.md`; `roger-amp-plan/PLAN.md` (Piece 02); `roger-amp-plan/BUILD_LOG.md`; `crates/path-cli/src/cmd_share.rs` (`collect_copilot:351-392`, `harness_status_copilot:719-730`, tests `:1047-1095` — your templates), `cmd_list.rs:695-768`, `cmd_show.rs:46-55` (the hidden `--project` shim is mandatory — share's preview template `"{exe} show --ansi {1} --project {2} --session {3}"`), `tests/integration.rs:429-542`.

**Assumed done:** pieces 00–01 (`AmpConvo`, `SessionMetadata`, cache ids, `p import amp` work; tags `amp-m0`, `amp-m1`).

## Scope

Full forward CLI: `ListSource::Amp` + `run_amp` (json/tsv/pretty; TSV `id·last_activity·line_count·cwd·first_user_message` via `sanitize_tsv`), `ShowSource::Amp` with hidden project shim + `derive_one` arm, share aggregation (`collect_amp` want-block, `is_not_found_amp` suppression, `warning: amp aggregation failed:` otherwise, `harness_status_amp` + status line, `derive_session` arm). Write the two gather unit tests + `amp_only_bundle`/`write_amp_session` helpers first (failing), then wire to green; add the 6 integration tests + `amp_home_fixture` (honoring the piece-00 env override so no real state is touched). ⚠ Then the live L1 check: `share --harness amp --session <captured-id> --anon` (+ `scripts/test-pathbase-live.sh <url>`); if Pathbase is unreachable today, run the mock-server suite and produce the rendered-markdown demo instead.

### Out of scope

Projector/resume (03); release bookkeeping (06).

## Definition of done

`p list amp --format tsv` shows the captured session; share prints a Pathbase URL whose page shows the beats + the token attribution level the dossier promised; workspace gates + `just ci` green; BUILD_LOG entry includes the URL (or mock evidence) — **this is the artifact Alex reviews against the L1 DoD sentence**. Tag `amp-m2`. Stop.
