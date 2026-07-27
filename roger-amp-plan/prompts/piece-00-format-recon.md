# Piece 00 — format-recon

You are a fresh Claude Code session building one piece of the Amp harness for toolpath. This prompt is self-contained; execute it end to end. **This piece runs the real `amp` CLI: before every command marked ⚠, state what you're about to run and get Roger's explicit go-ahead. Roger is present and interactive.**

## Context to load first

1. `CLAUDE.md` (repo conventions — binding).
2. `roger-amp-plan/PLAN.md` — Global Constraints, Evidence base, Piece 00.
3. `roger-amp-plan/BUILD_LOG_TEMPLATE.md` — you create `roger-amp-plan/BUILD_LOG.md` this piece.
4. `docs/agents/feature-elicit.prompt.txt` + `docs/agents/feature-elicit.md` (the capture instrument) and `docs/agents/formats/copilot-cli/README.md` (the docs shape + confidence-tag table you must mirror).

**Assumed done:** nothing builds yet. You are on branch `roger-amp-plan` (or a build branch off it). Amp is installed (`~/.amp/bin/amp`, Bun single-file exe) and logged in; one prior test thread exists. Verified evidence so far is PLAN.md §Evidence-base — treat everything else as unknown.

## Scope

Answer four questions with evidence, land fixtures, write the format docs. (1) **Reconstruction**: can a completed thread be fully rebuilt from this machine afterwards — from `~/.cache/amp/logs/threads/T-*.log`, a local store, or only the teed `--stream-json`? (2) **Tokens**: what usage counters exist (stream events / thread log / `session.json` / bundle strings / web sidebar) and which of the three kind-v1.1.0 patterns do they fit (per-message-repeated ⇒ group_id + field-wise max; cumulative ⇒ saturating deltas; clean per-message)? **If none: stop and escalate to Roger before writing docs.** (3) **Isolation**: which env vars relocate dataDir/cache (`XDG_DATA_HOME`? `AMP_*`?) — needed later for the writer loop. (4) **Envelope**: the `--stream-json` line format, captured verbatim.

Protocol: baseline `find`-snapshot of `~/.local/share/amp ~/.cache/amp ~/.config/amp` → ⚠ `amp --version` + `amp usage` + `amp threads list` (record all; the version stamps every claim) → ⚠ one trivial thread `amp -x 'Reply with exactly: ok' --stream-json | tee` → dir diff → ⚠ `docs/agents/feature-elicit.prompt.txt` **verbatim** in ONE session (TUI if `-x` can't drive it; tee streams; try `--stream-json-thinking` only on the trivial thread) → dir diff → sanitize (strip `$HOME`, username, tokens, private URLs) → `test-fixtures/amp/convo.jsonl` = the canonical artifact per Q1. Keep thread visibility private; ≤3 new threads total.

Then write `docs/agents/formats/amp/`: `README.md` (revision date, `Tracks:`/`Version anchors:`/`First-hand grounding:` block, the 5-tag confidence table `[observed]/[official]/[reverse-eng]/[inferred]/[unverified]`, scope-exclusions), `RECON.md` (one-sentence answers to Q1–Q4 + evidence), `directory-layout.md`, `events.md` (envelope + event catalogue + a closing "Mapping sketch to the toolpath IR" table → `Turn`/`ToolInvocation`/`DelegatedWork`/`TokenUsage`), `session-state.md`, `file-fidelity.md` (do tool events carry real diffs/file contents?), `resume-and-sessions.md`, `known-gaps-and-sourcing.md` (methodology + unchecked verification checklist). Tag every claim; register the folder in `docs/agents/formats/README.md`; add the amp row to `docs/agents/feature-elicit.md`.

### Out of scope

No Rust. No crate. No writes outside `docs/`, `test-fixtures/amp/`, `roger-amp-plan/`, and scratchpad. Never read `~/.local/share/amp/secrets.json`; never touch existing thread ids.

## Definition of done

`ls docs/agents/formats/amp/` shows all files; RECON.md answers Q1–Q4 explicitly; fixtures parse (`jq` or documented equivalent); version stamps greppable; `just ci` green (prettier). Create `roger-amp-plan/BUILD_LOG.md` from the template preamble + the Piece 00 entry (ADR: the fork answer and the token classification are the two decisions; open questions → anything endangering L1/L2). Commit `docs(amp): format recon dossier + fixtures`; tag `amp-m0`. **Do not start piece 01.**
