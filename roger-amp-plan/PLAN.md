# Amp Harness (`toolpath-amp`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan piece-by-piece. Steps use checkbox (`- [ ]`) syntax for tracking. Each piece also exists as a self-contained build prompt under `prompts/` — the intended execution mode is one fresh session per piece.

**Goal:** Add Amp (ampcode.com) as a both-directions preview harness — share an Amp session to Pathbase with conversation beats + honest token attribution (L1), resume Amp-shared toolpaths in Amp (L2) and Claude Code (L3), and stretch: resume Claude-Code-shared toolpaths in Amp (L4).

**Architecture:** New crate `toolpath-amp` (reader → `ConversationView` → shared `toolpath_convo::derive_path`; reverse `AmpProjector`), registered at the same seams `toolpath-copilot` used in `path-cli`. Format truth comes only from captures on this machine stamped with the recorded amp version; piece 00 closes the discovery gap the planning session deferred.

**Tech Stack:** Rust 1.94.0 / edition 2024 workspace; deps mirror `toolpath-copilot` (`toolpath`, `toolpath-convo`, `anyhow`, `chrono`, `serde`, `serde_json`, `similar`, `thiserror`; dev `tempfile`; `rusqlite` only if piece 00 finds a SQLite store).

## Layout

- `PLAN.md` — this file (the master plan). Build sessions read `CLAUDE.md` → this → `BUILD_LOG.md` before working.
- `BUILD_LOG_TEMPLATE.md` — entry format for the append-only `BUILD_LOG.md` (created in this directory by piece 00).
- `prompts/piece-00…06-*.md` — one self-contained build prompt per piece; paste into a fresh Claude Code session at the repo root.
- `adding-a-projector-delta.md` — the 30-claim still-true/superseded table feeding piece 06's refresh of `docs/agents/adding-a-projector.md`.
- Provenance: planning driven by `docs/agents/amp-planning.prompt.md` (approved spec, Roger + Alex Kesling + Ben, 2026-07-27); the Phase-1 six-subagent repo fan-out and the read-only Amp recon happened in the planning session on 2026-07-27. First deliverable = goal level 1, achieved at tag `amp-m2` (pieces 00+01+02).

## Global Constraints

- **Additive only.** Existing harnesses' behavior, CLI surface, and all existing tests stay untouched. Sanctioned exceptions (called out in pieces): appending `copilot, amp` to the stale hint string `crates/path-cli/src/cmd_resume.rs:350`, and widening exhaustive registration seams (`Harness::ALL [7]→[8]`, test-only `ArtifactType ALL [8]→[9]`, `HarnessBundle`, dispatch matches).
- **Fresh identifiers only.** Resume/export mint a fresh id before projection (`view.id = Uuid::new_v4()` pattern, `cmd_export.rs:382-386`); index registration is INSERT-only; never create a store the harness owns (`if !db_path.exists() → warn, Ok(false)`); never touch existing Amp thread ids.
- **⚠ protocol.** Steps marked ⚠ run the real `amp` CLI (network to ampcode.com, credits, amp-owned state writes) or fabricate state: stop and get Roger's explicit in-session go-ahead first. Captures stay visibility-private; sanitize tokens/credentials/PII before anything enters the repo.
- **Version stamping.** Every format claim carries `[observed, <amp version>]`. Current recorded version: `0.0.1785164324-gd1fcef` (build 2026-07-27T15:05:20Z) — re-record with `amp --version` at piece 00 and stamp against what's actually seen.
- **Token honesty (kind v1.1.0).** `token_usage` = message-group total on the group's final turn (field-wise max when line order untrusted); `attributed_token_usage` only where the source genuinely reports per-step spend; `breakdowns` informational, never summed, Σ(inner) ≤ parent, omitted when empty; all-zero wire counters decode to `None`; never stamp cumulative snapshots, repeated totals, or zero placeholders; no fabricated splits.
- **Gates at every piece:** `cargo build --workspace` && `cargo test --workspace` && `cargo clippy --workspace -- -D warnings`, plus `just ci` (fmt, clippy, shellcheck on new scripts, prettier on site/markdown). Site changes: `cd site && pnpm run build` stays green.
- **Git:** build branch `roger/amp-harness` off `roger-amp-plan` (or off `main` once this plan branch merges); Conventional Commits; tag `amp-m<N>` at each piece's DoD-green commit; no pushes/PR until Roger says so. Commit messages end with the repo's Claude co-author trailer.
- **IR baseline:** `main` at `192d726`. Ben's unmerged `items-ir`/`compaction` branches are NOT targets; an explicit reconciliation follow-up is budgeted in the risk register (copilot precedent: 5 files / 129 insertions).

## Evidence base (2026-07-27, read-only recon; the only verified format facts)

Verified `[observed, 0.0.1785164324-gd1fcef]`:

- Binary: `~/.amp/bin/amp` (symlinked from `~/.local/bin/amp`), **Bun single-file executable** (`argv: ["bun", "/$bunfs/root/amp-darwin-arm64"]`), arm64, 71.7MB — embedded JS is `strings`-mineable.
- State dirs: `~/.config/amp/` (user settings; currently empty), dataDir `~/.local/share/amp/` = `session.json` (UI/app state: `lastThreadId`, `lastThreadByTerminal`, `agentMode`, `launchCount`…), `secrets.json` (API key post-login; 141 B; never read/committed), `device-id.json` (`installationID`), `history.jsonl` (prompt history lines `{"text","cwd"}` — not thread content). Workspace settings path = `<workspaceRoot>/.amp/settings.json`. Logs: `~/.cache/amp/logs/cli.log` (structured JSONL) + `~/.cache/amp/logs/threads/T-<id>.log` (per-thread JSONL).
- Thread model: **server-authoritative**, live JSON-RPC/websocket "thread-client" (executor handshake, `client_mark_message_read`, `requiresOrderedDelivery`, transport connected/disconnected; observer events `onMessageAdded`, `onThreadTitle`, `onToolProgress`, `onAgentState`, `onThreadSettings`). Ids: threads `T-<uuidv7-ish>`, messages `M-<base62>`; `agentMode: "medium"`. One real test thread exists from Roger's install session (106 KB thread log).
- Orientation-only (manual/guides, NOT evidence): `amp threads new|continue [id]|list|fork|share|compact`, `amp -x`, `--stream-json`, `--stream-json-thinking`, `amp usage`, visibility levels.

**Unverified — piece 00 must close (no later DoD may assume them):** stream-json envelope shape; whether the per-thread cache log embeds full message bodies (⇒ after-the-fact local reconstruction) or only transport metadata; any per-message/per-thread token fields anywhere; `threads list/continue` local-vs-server semantics; env overrides for dataDir/cache (isolated-home story); server API endpoints.

## Piece table

| Piece | Tag | Goal-ladder | Delivers |
|---|---|---|---|
| 00 format-recon | `amp-m0` | gates all | ⚠ captures, architecture-fork answer, token answer, `docs/agents/formats/amp/`, sanitized fixtures, BUILD_LOG created |
| 01 derive-crate | `amp-m1` | → L1 | `crates/toolpath-amp` forward path + minimal `p import amp`; `p validate`-clean; `p render md` beats |
| 02 share-wiring | `amp-m2` | **L1** | full forward CLI (list/show/picker/TSV/share); Pathbase round-trip; Alex accepts rendered beats+tokens |
| 03 projector-resume | `amp-m3` | **L2** | opens with ⚠ resume/writer recon; `AmpProjector`, `p export amp`, `path resume --harness amp`, `writing-compatible.md`, `verify-amp-live.sh` |
| 04 resume-into-cc | `amp-m4` | **L3** | Amp-shared toolpath resumes in `claude -r`; probing-question pass |
| 05 cc-to-amp | `amp-m5` | **L4** (stretch) | CC session → Amp; `native_name` completeness; cross-harness matrix row; round-trip test |
| 06 docs-release | `amp-m6` | — | versioning checklist 1–11, CLAUDE.md/README/site/CHANGELOG/release.sh, `adding-a-projector.md` refresh, stale-doc fixes |

Sequential; each piece's DoD gate must be green (and BUILD_LOG entry written) before the next starts. L4 explicitly droppable; 06 can land right after 04 if 05 is cut.

---

## Piece 00 — format-recon (tag `amp-m0`)

**Files:** Create `docs/agents/formats/amp/README.md`, `RECON.md`, then `directory-layout.md`, `events.md` (envelope + event catalogue + "Mapping sketch to the toolpath IR" table), `session-state.md`, `file-fidelity.md`, `resume-and-sessions.md`, `known-gaps-and-sourcing.md`; `test-fixtures/amp/convo.jsonl` (+ `.json` sidecars as found); `roger-amp-plan/BUILD_LOG.md`. Register folder in `docs/agents/formats/README.md`; add the amp row to `docs/agents/feature-elicit.md`'s harness table.

**Interfaces — Produces:** the dossier every later piece cites: (Q1) architecture fork answer — local-log reconstruction / server API / capture-time-only; (Q2) token classification into exactly one of the three legal patterns (or "none" ⇒ escalate); (Q3) isolated-home/env-override story; (Q4) `--stream-json` envelope reference; fixture files.

**Steps:**

- [ ] ⚠ Confirm go-ahead; `amp --version`; `amp usage`; `amp threads list` (expect the one install-session thread); record workspace/visibility default. Re-stamp the version everywhere below.
- [ ] Baseline dir snapshot (`find ~/.local/share/amp ~/.cache/amp ~/.config/amp -type f -exec ls -la {} +` into scratchpad).
- [ ] ⚠ Trivial capture: `amp -x "Reply with exactly: ok" --stream-json | tee <scratchpad>/trivial-stream.jsonl`; diff dirs; save the new `T-*.log`.
- [ ] ⚠ Feature-elicit capture: run `docs/agents/feature-elicit.prompt.txt` **verbatim** in one Amp session (interactive TUI if `-x` can't drive it; tee `--stream-json` where applicable; also try `--stream-json-thinking` on the trivial thread only). Diff dirs; archive stream + thread log + any new files. Team guidance (2026-07-27): this prompt is the standard instrument for exercising/testing a newly hooked-up agent — verbatim where possible; if Amp's surface forces changes, adapt minimally and record every deviation in RECON.md.
- [ ] Q1: compare the thread log + stream against the web thread page content; grep the log for full message bodies (`onMessageAdded` payloads). State explicitly whether a completed thread is fully reconstructable from this machine after the fact, and from which artifact.
- [ ] Q2: hunt token fields in stream events, thread log, `session.json`, bundle strings (`strings -n 8 ~/.amp/bin/amp | grep -iE 'tokens?|usage|cost'`), and the web sidebar. Classify per-message vs cumulative vs thread-only vs none. **If "none": stop, escalate to Roger/Alex before writing more docs.**
- [ ] Q3: test env overrides seen in cli.log/strings (`AMP_LOG_LEVEL`, `AMP_SETTINGS_FILE`, `AMP_API_KEY` are known; probe `XDG_DATA_HOME`/`XDG_CACHE_HOME` and any `AMP_*` dataDir override with `env XDG_DATA_HOME=<tmp> amp threads list`) — the isolated-home story pieces 03/05 need.
- [ ] Sanitize (replace `$HOME`, usernames, tokens, thread URLs if private) → `test-fixtures/amp/convo.jsonl` = the feature-elicit capture in whatever the canonical local artifact is per Q1.
- [ ] Write the docs listed above: README carries revision date + `Tracks:`/`Version anchors:`/`First-hand grounding:` block + the 5-tag confidence table (`[observed]/[official]/[reverse-eng]/[inferred]/[unverified]`); every claim tagged.
- [ ] Create BUILD_LOG.md (template preamble) + Piece 00 entry; commit `docs(amp): format recon dossier + fixtures`; tag `amp-m0`.

**Definition of done (reviewer runs):** `ls docs/agents/formats/amp/` shows the files; `jq -c 'limit(3;.[])' test-fixtures/amp/convo.jsonl >/dev/null` (or documented equivalent if not JSONL); `grep -rn 'observed, 0\.' docs/agents/formats/amp/ | head` shows version-stamped claims; RECON.md contains explicit single-sentence answers to Q1–Q4; `just ci` green (prettier covers the new markdown).

**Risks/fallback:** thread log turns out metadata-only and no API found ⇒ record-at-capture-time architecture: fixtures = teed `--stream-json`; L1 still achievable (share derives from the captured stream); flag loudly in RECON and to Roger. Credits: captures ≤ 3 threads.

## Piece 01 — derive-crate (tag `amp-m1`)

**Files:** Create `crates/toolpath-amp/{Cargo.toml,README.md,src/{lib,error,paths,reader,types,io,provider,derive}.rs,tests/{roundtrip.rs,real_fixture_roundtrip.rs},tests/fixtures/{sample-session.*,real-session.*}}`. Modify: root `Cargo.toml` (`members` + `[workspace.dependencies] toolpath-amp = { version = "0.1.0", path = "crates/toolpath-amp" }`), `crates/path-cli/Cargo.toml` (dep in the emscripten-appropriate target block(s) per piece 00's storage answer), `crates/path-cli/src/{artifact.rs,harness.rs,derive.rs,cmd_import.rs}` (minimal import path only).

**Interfaces — Produces (consumed by 02–05):** `toolpath_amp::{PROVIDER_ID = "amp", PRODUCER_NAME, AmpConvo, Session, SessionMetadata, ConvoError, PathResolver, AmpProjector (stub OK until 03), derive::{DeriveConfig, derive_path}, provider::{to_view, tool_category, native_name}}`. `AmpConvo::new()` infallible on missing dirs; `SessionMetadata {id, started_at, last_activity, cwd, version, first_user_message, line_count, dir_path}`; `ConvoError::{Io, NoHomeDirectory, AmpDirectoryNotFound}`; resolver honors the piece-00 env override, `with_amp_dir()` injection for tests; strict-mode reader flag threaded as a parameter.

**Steps:**

- [ ] Crate scaffold with `#![doc = include_str!("../README.md")]`; README opens with the `> ⚠️ Preview — schema reverse-engineered` blockquote + version anchor; description ends `(preview; schema reverse-engineered)`.
- [ ] `types.rs`/`reader.rs` from `events.md` (write the wire structs for exactly what the fixture shows; tolerant line-skip default + strict param; 15+5-style unit tests mirroring copilot's split).
- [ ] `provider.rs`: `tool_category` (exact-match arms first, substring fallback), `to_view` with linear `push_linked` parent chaining, tool results merged into originating turns, `files_changed` first-touch dedup, `view.base`/`producer`, `provider_id = Some("amp")`.
- [ ] Token layer per piece 00 Q2 classification — implement only the matching legal pattern; `is_usage_zero → None` guard; unit test per rule (group-final placement, no zero stamping, breakdown Σ ≤ parent if present).
- [ ] `derive.rs` thin wrapper (title `"Amp session: <8ch>"`); kind-conformance test validating a maximal derived path against `crates/path-cli/kinds/agent-coding-session/v1.1.0/schema.json` (mirror `toolpath-convo/src/derive.rs:1022`).
- [ ] `tests/real_fixture_roundtrip.rs` (forward counts vs source, valid single-Path graph, wire serde value-identity on every line; module doc stamps fixture provenance + amp version).
- [ ] Minimal CLI import: `ArtifactType::Amp` (+`"amp"`, test `ALL` 8→9, `path_keyed` stays false, `assert!(!ArtifactType::Amp.path_keyed())`), `Harness::Amp` (+`ALL` 7→8, both mappings, `HarnessBundle.amp`, `is_not_found_amp`), `derive.rs::derive_amp_session{,_with}`, `ImportSource::Amp {session, all}` + dispatch + `derive_amp` + `pick_amp` (preview `"{exe} show --ansi amp --session {1}"` — show lands in 02; picker preview may 404 until then, note in BUILD_LOG).
- [ ] Commit `feat(amp): toolpath-amp derive crate + p import amp (preview)`; tag `amp-m1`.

**Definition of done (reviewer runs):** `cargo test -p toolpath-amp` green; `cargo run -p path-cli -- p import amp --session <captured-id>` writes `~/.toolpath/documents/amp-path-amp-….json`; `cargo run -p path-cli -- p validate --input <that file>` passes; `cargo run -p path-cli -- p import amp --session <id> --no-cache --force | cargo run -p path-cli -- p render md --input - --detail full` shows the feature-elicit beats (user prompts, tool calls, sub-agent step, token totals where stamped); workspace gates green.

**Risks/fallback:** fixture reveals fields the wire structs missed ⇒ extend types + re-run value-identity test (it is the tripwire); if Q1 = capture-time-only, `AmpConvo` reads the archived capture dir instead of live amp state — same public surface.

## Piece 02 — share-wiring, L1 (tag `amp-m2`)

**Files:** Modify `crates/path-cli/src/{cmd_list.rs,cmd_show.rs,cmd_share.rs}` (+ `cmd_import.rs` picker polish): `ListSource::Amp` + `run_amp` (json `"source":"amp"` / tsv `id·last_activity·line_count·cwd·first_user_message` through `sanitize_tsv` / pretty, `// ── Amp (preview) ──` banner); `ShowSource::Amp { session, #[arg(long, hide=true)] project }` + `derive_one` arm; share: `collect_amp` gather block, `harness_status_amp` + `format_status_line("amp", …)`, `derive_session` arm. Tests: `cmd_share.rs` unit (`amp_only_bundle`, `write_amp_session`, gather-includes/filters ×2, status-unresolved loop), `tests/integration.rs` (`amp_home_fixture` + 6 CLI tests incl. cache-prefix assert `amp-path-amp-`).

**Steps:**

- [ ] TDD the share gather tests against a fixture home (clone `cmd_share.rs:1047-1094` shape), then wire `collect_amp`/status/dispatch until green.
- [ ] list/show arms + integration tests (clone `tests/integration.rs:429-542`).
- [ ] ⚠ Live L1 check: `cargo run -p path-cli -- share --harness amp --session <captured-id> --anon` against the reachable Pathbase (anon; Roger may `path auth login` for the authed variant); plus `scripts/test-pathbase-live.sh <url>` sandbox round-trip. Fallback when unreachable: the existing pathbase mock-server tests + the rendered markdown demo.
- [ ] Commit `feat(amp): p list/show amp + path share --harness amp (preview)`; tag `amp-m2`.

**Definition of done (reviewer runs):** `cargo run -p path-cli -- p list amp --format tsv` lists the captured session with `first_user_message`; `path share --harness amp --session <id> --anon` prints a Pathbase URL whose page shows the conversation beats; token attribution visible per piece 00's classification (session totals minimum, per-message/`attributed` if the source supports it); **Alex accepts the rendered result against the L1 DoD sentence**; workspace gates green.

**Risks/fallback:** L1 attribution ceiling is whatever Q2 found — if thread-total-only, the rendered demo shows `total_usage` and the limitation is stated in the dossier + surfaced to Alex before acceptance.

## Piece 03 — projector-resume, L2 (tag `amp-m3`)

**Opens with the deferred recon (same ⚠ + fresh-id rules):**

- [ ] ⚠ Resume semantics: `amp threads --help` + siblings; does `amp threads continue T-<id>` replay from local state or server (test logged-in; note offline behavior if cheap); what does the TUI show on resume?
- [ ] ⚠ Writer probe: fabricate under a **fresh** id only — per Q1 either copy a captured local record under a fresh `T-…`/new dir and try `amp threads continue <fresh>`, or (server-authoritative) locate thread-create/append endpoints in the bundle strings and probe once with a throwaway title. Outcome routes the rest: (a) local-state write ⇒ copilot-style projector writes; (b) API thread-create ⇒ projector output posts via the API (auth from Roger's logged-in CLI; never store creds in repo); (c) infeasible ⇒ document evidence in `writing-compatible.md`, mark L2/L4 blocked, ship `p export amp --output` only and stop the piece early with the BUILD_LOG entry saying so.

**Files:** Create `crates/toolpath-amp/src/project.rs` (+ re-exports), `scripts/verify-amp-live.sh`, `docs/agents/formats/amp/writing-compatible.md`. Modify `crates/path-cli/src/cmd_export.rs` (`ExportTarget::Amp` 3-mode; `build_amp_session` factored + fresh-UUID mint + `view.base.working_dir = cwd`; `project_amp`; `write_into_amp_project` warn-don't-fail + INSERT-only; **unit test `project_amp_returns_session_id_and_writes_artifact`** — close copilot's gap), `crates/path-cli/src/cmd_resume.rs` (`"amp"` source arm, `agent:amp` sniff, `argv_for` per recon — expected `["threads","continue",<id>]`, `project_into_harness` arm, fix `:350` hint to include `copilot, amp`). Tests: `tests/resume.rs` `file_input_explicit_amp_projects_and_records_exec` (ScopedHome + `ScopedPath::with_binary("amp")` + RecordingExec), `cmd_resume.rs` argv/sniff units.

**Steps:** projector (position-stable ids, preserved delegation ids, token re-expansion per wire shape, `native_name` reclassification invariant test) → export/resume wiring → ⚠ live loop: fabricate → real `amp` rejects → record **verbatim rejection** rows `[observed, <ver>]` → fix → repeat until load; verify on the trivial capture and the feature-elicit capture; then `scripts/verify-amp-live.sh` (isolated home per Q3, loader grep, probing question) — shellcheck-clean. Commit `feat(amp): AmpProjector + p export amp + path resume --harness amp (preview)`; tag `amp-m3`.

**Definition of done (reviewer runs):** `cargo run -p path-cli -- resume <amp-shared pathbase URL or cache id> --harness amp -C <dir>` lands in a real `amp` session that **answers a probing question about the prior session correctly** ("In one sentence, what was the most-used tool in this session?"); `writing-compatible.md` lists every observed rejection verbatim with the version stamp; `bash scripts/verify-amp-live.sh` passes; workspace gates green. If routed to (c): DoD is instead the documented infeasibility evidence + green export-to-file path — explicitly reviewed by Roger.

## Piece 04 — resume-into-cc, L3 (tag `amp-m4`)

**Files:** Modify only if gaps appear — the machinery exists: `infer_source_harness` already gained `"amp"` (03); claude projection is `cmd_export::project_claude`. Add `crates/path-cli/tests/resume.rs` case `file_input_amp_source_into_claude_projects_and_records_exec` (`make_convo_path("agent:amp", "amp://…")` + `args_explicit(…, Harness::Claude)`); fidelity pass on `ToolCategory` coverage for amp's native tool names so CC rendering is sensible (extend `tool_category` arms + unit tests as needed).

**Steps:**

- [ ] Test-first the recording-exec case; run; fix category/name mapping fallout.
- [ ] ⚠ Live: `path resume <amp-shared input> --harness claude -C <dir>` → real `claude -r` probing question.
- [ ] Commit `feat(amp): amp→claude cross-harness resume verified`; tag `amp-m4`.

**Definition of done:** probing-question pass inside real `claude -r` on the feature-elicit-derived doc; RecordingExec test green; workspace gates green.

## Piece 05 — cc-to-amp, L4 stretch (tag `amp-m5`)

**Files:** Modify `crates/toolpath-amp/src/provider.rs::native_name` (total mapping, disambiguate FileWrite by `old_string`/`edits`, FileSearch by `pattern`/`query`; invariant test per category), `crates/path-cli/tests/cross_harness_matrix.rs` (`struct AmpHarness` impl `{name, roundtrip, load_fixture, schema_validates}`; register in **both** vectors `:1024-1033` and `:1075-1085`), fixture already at `test-fixtures/amp/convo.jsonl` (byte-identical to crate `real-session`); `crates/toolpath-amp/tests/roundtrip.rs` foreign-source case.

**Steps:** matrix row green across all 2N+1 new cells (14 invariants) → ⚠ live: pick a real CC session from this repo, `path share --harness claude …` (or reuse an existing cache doc), `path resume <it> --harness amp` → probing question in real `amp`. Commit `feat(amp): CC→amp projection + cross-harness matrix row`; tag `amp-m5`.

**Definition of done:** `cargo test -p path-cli --test cross_harness_matrix` green including amp rows/columns; a real CC session resumes in `amp` and passes the probing question; workspace gates green. **Droppable without affecting 06.**

## Piece 06 — docs-release (tag `amp-m6`)

**Files:** Modify `CLAUDE.md` (crate tree, dep graph `(preview)`, preview prose block, cross-deps sentence, test-count line, provider bullet, "seven agent harnesses"→eight — and fix the stale `Turn.extra` bullet), `README.md` (crate list + TSV provider doc), `site/_data/crates.json` (6-key entry, role text with preview caveat), `site/pages/crates.md` (diagram + sentence), `scripts/release.sh` (comment block, `_all_crates`, tier-2b publish **and** wait_for_index loops), `CHANGELOG.md` (`## New provider: Amp (preview) — <date>` covering parse coverage, sourcing posture, wiring, loader contract w/ verbatim errors, verification evidence), `scripts/capture-elicit-fixtures.sh` (`ALL_HARNESSES` + `drive_amp()` + dispatch case), `docs/agents/adding-a-projector.md` (refresh from `roger-amp-plan/adding-a-projector-delta.md`: fix stale paths/signatures, remove `Turn.extra` teaching, add share/resume/`Harness`/`ExecStrategy`/matrix/writer-contract sections, re-point the reference block at copilot+amp), crate README/status flips **only if** 03 verified (all 5 hedge sites in lockstep, including the stderr banner).

**Definition of done (reviewer runs):** `grep -n amp Cargo.toml site/_data/crates.json scripts/release.sh CHANGELOG.md README.md CLAUDE.md | wc -l` shows every checklist item; `bash -n scripts/release.sh` + `shellcheck scripts/*.sh` clean; `cd site && pnpm run build` green; `just ci` green; versioning checklist items 1–11 each pointable in the diff (`toolpath-amp 0.1.0` in 3 places, `path-cli 0.16.0→0.17.0`). Tag `amp-m6`; PR only on Roger's word.

---

## Cross-cutting requirements

`docs/agents/feature-elicit.prompt.txt` is the standard exercise/test instrument for any newly hooked-up agent (team guidance, 2026-07-27): piece 00 uses it to generate the reference capture, and later pieces reuse feature-elicit-shaped sessions as the verification substrate (the piece-03/04/05 probing question — "most-used tool in this session" — keys off exactly this shape). Treat it as a starting point, not a straitjacket: adapt minimally where a harness can't be driven verbatim, and record deviations in the dossier.

Preview labeling exactly like copilot (clap doc comments `(preview)`, Cargo description, README blockquote, CLAUDE.md graph suffix, crates.json, runtime stderr banner until verified). Share uploads carry the full derivation — no egress stripping. No changes to `crates/path-cli/kinds/**` or `site/kinds/**` (kind v1.1.0 is sufficient; only accounting-rule changes would warrant v1.2.0). Emscripten placement decided by piece 00 storage answer (pure JSONL ⇒ both dep blocks like copilot; SQLite ⇒ native-only gating like opencode). Every piece appends its BUILD_LOG entry (ADR decisions, deviations, tests run, open questions) before its tag.

## Risk register

1. **Token attribution (L1).** Amp may expose only thread-level cost (manual shows cost only in the web sidebar). Mitigation: piece 00 Q2 escalation before any code; honest `total_usage`-only fallback presented to Alex as the L1 ceiling.
2. **Server-side writer contract (L2/L4).** Threads are server-authoritative; local fabrication may be ignored. Mitigation: piece 03's three-route fork with the documented-infeasibility exit; evidence-grade `writing-compatible.md` either way.
3. **Format volatility.** Amp versions are build-timestamped (`0.0.1785164324-…`) — expect churn. Mitigation: version stamps on every claim; re-record `amp --version` at each piece; matrix + value-identity tests as tripwires.
4. **items-IR collision.** Ben's branches rework `ConversationView`. Decision (Roger): build against `main`; budget a reconciliation commit (~5 files) when they merge — tracked as a named follow-up, not part of this plan.
5. **Credits/Pathbase.** Account funded; Pathbase reachable anon today. Authed pathstash check is optional (Roger login) — never a DoD blocker (mock-server + rendered-md fallback per spec).
