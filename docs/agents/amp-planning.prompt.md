<!-- Usage: start a FRESH Claude Code session at the toolpath repo root and
     prompt: "Read docs/agents/amp-planning.prompt.md and follow it."
     Authored 2026-07-27 (Roger IL Grande + Claude) for the Empathic onsite. -->

# Planning session: add Amp (ampcode.com) as a toolpath harness

You are running a **planning session** for a single-day build. Your job, in
order: (1) map, with parallel subagents, exactly how this repo's share/resume
machinery works and what a new harness must implement; (2) empirically
discover Amp's session format on this machine, working interactively with
Roger; (3) produce a written implementation plan of sequential milestones,
each with an explicit definition of done. **You do not implement the feature
in this session.**

The requirements in this document were brainstormed with and approved by
Roger (with Empathic's Alex Kesling and Ben) on 2026-07-27. Treat this file
as the approved spec input: skip `superpowers:brainstorming`, go straight to
Phase 1, and invoke `superpowers:writing-plans` when you reach Phase 3. Do
not re-litigate requirements — but do ask Roger whenever reality contradicts
this document.

## Ground rules

- **Additive only.** Existing harnesses' behavior, the current CLI surface,
  and all existing tests must remain untouched by the eventual build. The
  plan must be reviewable as pure addition (new crate + registration points),
  the way `toolpath-copilot` was added.
- Phase 1 subagents are **read-only** — no writes anywhere.
- This session writes only: draft docs under `docs/`, fixtures under
  `test-fixtures/amp/`, and the plan document. No source-code changes, no
  commits and no pushes without Roger's explicit approval.
- Get Roger's go-ahead before every install, login, credential use, network
  write, or any write outside this repo (`~/.amp`, `~/.config/amp`, …).
  Never overwrite existing Amp user state — fresh identifiers only (the
  Copilot INSERT-only precedent).
- When blocked or surprised, ask Roger — this is an interactive onsite
  session, and Alex/Ben are reachable for Amp- and Pathbase-side questions.

## Decisions already made (do not reopen)

- Harness identifier **`amp`**: crate `toolpath-amp`, `--harness amp`,
  cache-id prefix `amp-`.
- Preview-provider status and conventions modeled on `toolpath-copilot`.
- The build is sequential milestones with a DoD gate between each (Phase 3).

## Goal ladder (priority order; DoD language from Alex Kesling)

| Level | Goal | Definition of done |
|---|---|---|
| 1 | Share an Amp session to Pathbase | "I can share from Amp a session to pathbase, and I can view at the very least the top-level beats of the conversation with attribution for what tokens were spent." |
| 2 | Resume **in Amp** from an Amp-shared toolpath | The projected session loads in the real `amp` CLI at a recorded version and "gives back a suitable fidelity conversation" — the model answers a probing question about the prior session correctly. |
| 3 | Resume **in Claude Code** from an Amp-shared toolpath | Same probing-question standard inside `claude -r`. |
| 4 (deepest stretch) | Resume **in Amp** from a Claude-Code-shared toolpath | Same standard: a real CC session, projected into Amp, resumes with usable context. |

## Environment facts (verified the morning of 2026-07-27)

- **Amp is not installed on this machine** — no `amp` binary, no `~/.amp`,
  `~/.config/amp`, or Application Support state. Roger can install and log in
  today; that is Phase 2 step 1.
- **Unconfirmed**: usage credits/quota; existing historical Amp threads to
  test against; a reachable Pathbase deployment + login for level 1's live
  check. The plan must include cheap environment checks for each, with
  fallbacks (Pathbase mock-server tests + `path p render md` demonstrate
  "beats + token attribution" locally; `scripts/test-pathbase-live.sh <url>`
  when a live instance is available).
- **Known Amp facts** from https://ampcode.com/manual (fetched today):
  threads are server-hosted at ampcode.com with `T-<uuid>` ids and URLs like
  `https://ampcode.com/threads/T-…`; `amp threads continue --execute '…'
  --stream-json` exists; visibility levels are private / workspace / group /
  unlisted; settings live at `~/.config/amp/settings.json(c)` plus project
  `.amp/settings.json(c)`; MCP OAuth tokens under `~/.amp/oauth/`;
  instruction files are `AGENTS.md` (legacy `AGENT.md`); an Enterprise
  OpenAPI is mentioned at `/api/v2/openapi.json`; per-thread cost appears
  only in the web UI sidebar. The manual documents **no local thread storage
  format, no per-message token fields, and no thread API** — that gap is
  exactly what Phase 2 exists to close.

## What the repo already gives you

CLAUDE.md is loaded; these are the anchors that matter here. Phase 1 must
verify them in code rather than trusting this list.

- **IR + shared derivation** — `toolpath-convo`: `ConversationView` →
  `derive_path`, the reverse `extract_conversation`, `ConversationProjector`,
  `Turn.extra["<harness>"]` namespacing, `ToolCategory` canonicalization
  (names preserved verbatim; remapping is the projector's job).
- **The template: newest both-directions preview harness** —
  `toolpath-copilot`: reader/provider, `CopilotProjector`, resume writing
  `~/.copilot/session-state/<id>/` plus an INSERT-only `session-store.db`
  row, a reverse-engineered writer contract documented with verbatim loader
  rejections in `docs/agents/formats/copilot-cli/writing-compatible.md`, the
  "Verified in copilot 1.0.67" stamping convention, a real captured fixture
  driving round-trip tests, and a row in the cross-harness conformance matrix
  (`crates/path-cli/tests/cross_harness_matrix.rs`).
- **Fidelity gold standard for derivation** — `toolpath-codex` (real diffs on
  every file artifact; per-turn token deltas by differencing cumulative
  counters).
- **CLI seams** — `crates/path-cli/src/artifact.rs` (`ArtifactType`),
  `crates/path-cli/src/harness.rs` (`Harness`, `HarnessBundle`),
  `cmd_resume::ExecStrategy` (mockable exec), the fzf/skim pickers
  (`skim_picker.rs`), `p list … --format tsv`, cache-id prefixes in
  `cmd_import`.
- **Token accounting rules (kind v1.1.0)** — CLAUDE.md's "Token accounting"
  and "breakdowns" bullets are normative. Never stamp cumulative counters,
  repeated message totals, or zero-filled placeholders onto steps.
  `token_usage` = message-group total (group's final step; field-wise max
  where line order isn't trusted); `attributed_token_usage` only where the
  source genuinely reports per-step spend; `breakdowns` are informational,
  never summed, `Σ(inner) ≤ parent`.
- **Capture instrument** — `docs/agents/feature-elicit.prompt.txt`: the
  scripted task list exercising every tool category (including a sub-agent
  dispatch at step 9). Running it verbatim inside a new harness is how this
  repo generates reference fixtures; it produced the Copilot one.
- **Versioning/release checklist** — CLAUDE.md items 1–11, `scripts/release.sh`
  tiers, `site/_data/crates.json`. The plan's milestones must carry these.

### Warning: `docs/agents/adding-a-projector.md` is out of date (per Ben)

Read it for the mental model, which is still sound: `Provider::to_view` →
View → `derive_path`; projector = `extract_conversation` → `project`;
`native_name(category, args)` tool-name remapping; and its "Live end-to-end
verification" section (full-pipeline check, CLI-accepts check with a probing
question, diff-against-a-real-session check) — that verification method is
the DoD standard for goal levels 2–4. Do **not** trust its mechanics: it
predates the `toolpath-cli` → `path-cli` rename (stale file paths, stale
top-level `path export` invocations) and the entire Copilot era
(`path share`/`path resume`, `Harness`/`ArtifactType`, `ExecStrategy`,
writer-contract methodology, the cross-harness matrix). Subagent 6 produces
the corrected delta; the plan should include refreshing that doc as a cheap
cross-cutting item.

## Phase 1 — Repo fan-out (parallel, read-only subagents)

Dispatch these six explorations in parallel. Each returns: entry points as
`file:line`, the contract a new harness must satisfy, gotchas/invariants, and
a concrete checklist fragment.

1. **Share pipeline** — trace `path share` end to end: harness probing,
   session aggregation and picker ranking, per-harness derivation entry,
   cache write, Pathbase upload and auth. Deliverable: the exact seams `amp`
   must plug into, including what a provider exposes for listing/metadata
   (`first_user_message`, project/cwd keying) and its TSV columns.
2. **Resume pipeline** — trace `path resume`: input resolution
   (URL / shorthand / file / cache id), agent-bearing-Path validation,
   harness picker with `meta.source` inference, projector invocation, on-disk
   write, exec via `ExecStrategy`. Deliverable: everything
   `resume --harness amp` will need, including exactly how Copilot's resume
   writes session state and INSERTs a fresh store row.
3. **IR + token contract** — `toolpath-convo`: field semantics of
   `ConversationView`/`Turn`/`ToolInvocation`/`DelegatedWork`, `extra`
   namespacing, `derive_path`/`extract_conversation`/`ConversationProjector`;
   the token rules with pointers to each provider's compliance choice
   (Claude group_id field-wise max; Codex cumulative differencing; Gemini and
   opencode reasoning-fold + breakdown; pi/opencode all-zero → `None`).
   Deliverable: the checklist a new provider must pass to stamp tokens
   honestly.
4. **Copilot case study** — enumerate **every** file the Copilot integration
   touched (crate internals, path-cli registration, tests, fixtures, docs,
   CLAUDE.md/README/site/`crates.json`/`release.sh` entries) and how the
   writer contract was discovered per
   `docs/agents/formats/copilot-cli/writing-compatible.md`. Deliverable: the
   additive-change inventory `toolpath-amp` will mirror.
5. **CLI + release wiring inventory** — every registration point for a new
   harness: `ArtifactType`, `Harness`/`HarnessBundle`, `p import/list/show`
   dispatch, picker preview commands, cache prefixes, `p export` targets,
   plus the versioning checklist artifacts. Deliverable: an ordered wiring
   checklist with file paths.
6. **Docs conventions + stale-doc delta** — how `docs/agents/formats/` is
   organized (single-file vs directory treatment, what the Copilot directory
   includes that a preview harness needs); read `adding-a-projector.md` and
   produce a "still true / superseded by (pointer)" table.

Synthesize the six reports into one **integration contract** (~a page). It
becomes an appendix of the plan. **Checkpoint: present it to Roger** and flag
anything that contradicts this document before starting Phase 2.

## Phase 2 — Amp empirical discovery (interactive; Roger drives logins)

Goal: close the format gap far enough that the level-1 milestones (M0–M2)
in Phase 3 have fully concrete DoDs. This is reconnaissance, not the
finished format reference (that is the build's M0). Keep captures few and
small — credits are unconfirmed; check Amp's pricing/free-tier terms before
burning sessions.

**Steps 1–5 are the required core — every one serves goal level 1. Steps
6–7 serve levels 2/4 only and are conditional**: run them only if the
Phase 2 timebox still has room once the dossier's level-1 sections are
solid; otherwise skip them and the plan folds them into M3's opening recon
sub-step (see Phase 3). Getting the build started on M0 sooner beats
polishing recon for milestones that come later.

Protocol (⚠ = get Roger's explicit go-ahead first):

1. ⚠ **Install + login.** Take the install command from the manual (npm-based
   per Amp's docs — verify the exact command there before running). Record
   `amp --version`; every format claim from here on is stamped against it.
   Roger performs the login. If installation proves impossible, stop and
   checkpoint: Phase 3 gets replanned with format-contingent milestones.
2. **Snapshot before/after.** Inventory `$HOME` dotfiles/dirs before the
   first run; diff after each step (`~/.amp`, `~/.config/amp`,
   `~/Library/Application Support`, caches, any SQLite files). Output: the
   complete list of paths Amp writes.
3. ⚠ **Baseline captures.** One trivial thread first (interactive or
   `--execute`), then run `docs/agents/feature-elicit.prompt.txt` **verbatim**
   inside one Amp session — it exercises every tool class including a
   sub-agent dispatch. Tee `--stream-json` output where applicable.
   Immediately archive raw artifacts into `test-fixtures/amp/` (sanitize
   tokens/credentials/PII before they enter the repo).
4. **Locate the session record.** Where does the full conversation actually
   live? Check in order: local files/DB from step 2; the `--stream-json`
   stream (capturable only at run time); the server (web thread page, any
   `/api/...` endpoints the CLI hits — inspect the installed npm bundle for
   endpoint paths and storage code, the same trick `toolpath-cursor` used to
   extract `TOOL_TABLE` from a JS bundle). Answer explicitly: **can a
   completed thread be fully reconstructed from this machine after the
   fact?** The answer picks the provider architecture: local file/DB reader
   (like copilot/cursor/opencode) vs API derive (like `toolpath-github`) vs
   record-at-capture-time (new pattern — flag loudly if so).
5. **Token visibility.** Hunt for per-message or per-thread token counts in
   stream-json events, local records, bundle-discovered APIs, and the web
   thread page. Map findings onto the kind v1.1.0 rules: what is honestly
   stampable as `token_usage` vs `attributed_token_usage` vs `breakdowns`?
   (Copilot precedent: per-message `output` + shutdown totals, no per-step
   attribution — that shape satisfied level-1-style DoD before.) **Level 1's
   "attribution for what tokens were spent" hangs on this — report exactly
   what attribution is achievable, and if the answer is "none", surface it
   to Roger/Alex immediately rather than at plan review.**
6. **Resume semantics (conditional).** `amp threads --help` and siblings: what identifies a
   thread for continuation (`T-<uuid>`, URL, "most recent")? Does
   `amp threads continue` work offline or logged-out — i.e., does it read
   local state or fetch from the server? What exactly does a resumed session
   replay?
7. ⚠ **Writer-contract feasibility probe (conditional; goal levels 2 and 4).**
   Non-destructively test whether locally fabricated thread state is
   honored: e.g., copy a captured thread's local record under a **fresh** id
   and see whether `amp threads continue <fresh-id>` accepts it — or
   establish that threads must originate server-side, in which case levels
   2/4 route through the API (find thread-create/append endpoints in the
   bundle). Never touch existing ids. If feasibility can't be established
   cheaply, don't sink the day into it — record it as the plan's top risk
   with the evidence gathered.

Deliverable: a **preliminary format dossier**, drafted at
`docs/agents/formats/amp/RECON.md`: every path written, envelope/shape
samples, token fields found, the architecture-fork answer, resume-semantics
and writer-contract findings if steps 6–7 ran (otherwise listed explicitly
as deferred recon), and open questions. **Checkpoint: review it with Roger (plus Alex/Ben if
available)** — especially the fork answer and anything endangering a goal
level — before writing the plan.

## Phase 3 — Write the milestone plan

Invoke `superpowers:writing-plans` and produce the plan under its
conventions. Requirements:

- **Sequential milestones, each carrying**: scope (files to create/touch,
  drawn from the Phase 1 inventory); tests to add (fixture-driven, matching
  the repo's per-crate style); an explicit **definition of done listing the
  verification commands a reviewer runs**; the "verified at amp X.Y.Z" stamp
  wherever a real-CLI check applies; risks and fallback. No milestone DoD
  may rest on an unverified format assumption — cite the dossier. Where
  Phase 2 deferred steps 6–7, the affected milestone opens with that recon
  and states how its outcome adjusts the DoD (see M3).
- **Expected ladder → milestone mapping** (adjust to Phase 2 findings):
  - **M0 — Format reference.** `docs/agents/formats/amp/` written from the
    captures; sanitized fixtures landed in `test-fixtures/amp/` and the
    crate's `tests/fixtures/`.
  - **M1 — `toolpath-amp` derive crate.** Reader → `ConversationView` →
    `toolpath_convo::derive_path`; tokens per the v1.1.0 rules; unit +
    integration + doc tests against the real fixture. DoD includes
    `path p import amp …` producing a `p validate`-clean Path whose
    `p render md` shows the conversation beats.
  - **M2 — CLI forward wiring + share (goal level 1).** `ArtifactType` and
    `Harness` variants, `p import/list/show amp`, picker + TSV,
    `path share --harness amp`. DoD: live Pathbase round-trip when an
    instance is reachable (`scripts/test-pathbase-live.sh`), otherwise
    mock-server tests plus rendered markdown demonstrating beats + token
    attribution; Alex accepts the rendered result against the level-1 DoD.
  - **M3 — `AmpProjector` + same-harness resume (goal level 2).** If
    Phase 2 skipped steps 6–7, M3 **opens with that deferred recon**
    (resume semantics + writer-contract feasibility, same ⚠ go-ahead and
    fresh-id rules), and the plan states how each outcome routes the rest
    of the milestone (local-state write vs API thread-create vs documented
    infeasibility with evidence). Then: `p export amp`,
    `path resume --harness amp`, fresh-id-only writes, and the start of a
    `writing-compatible.md` sibling recording the writer contract with
    verbatim rejections. DoD: an Amp-shared toolpath resumes in the real
    `amp` CLI and passes the probing-question check.
  - **M4 — Cross-harness into Claude Code (goal level 3).** Mostly existing
    machinery: `meta.source` inference, category mapping, fidelity pass.
    DoD: probing-question check inside `claude -r`.
  - **M5 (stretch) — Claude Code → Amp (goal level 4).** `native_name`
    remapping, foreign-extras dropping, a cross-harness matrix row, and a
    round-trip test. DoD: a real CC session resumes in `amp` and passes the
    probing-question check.
- **Cross-cutting section**: preview labeling exactly like Copilot's;
  versioning checklist items 1–11 (workspace members and
  `[workspace.dependencies]`, `crates.json`, `release.sh` tier-2 placement,
  CHANGELOG, README, CLAUDE.md, crate README wired via
  `#![doc = include_str!…]`); `cargo build/test/clippy --workspace` green at
  every milestone gate; refresh `adding-a-projector.md` from the subagent-6
  delta; share uploads carry the full derivation (no egress stripping),
  matching existing harnesses.
- **Risk register**, at minimum: token-attribution availability (level 1);
  server-side-thread writer contract (levels 2/4 — possibly still unprobed
  at plan time if steps 6–7 were deferred); format volatility across
  Amp versions (mitigate with the version stamp); credit budget; Pathbase
  availability.

**Gate: Roger (plus Alex/Ben) review the plan document.** Implementation
starts only after approval, in fresh session(s) executing the plan
(`superpowers:executing-plans`) — not in this one.

## Timebox

Phase 1 within ~1 hour (subagents run in parallel); Phase 2 within ~1.5–2
hours; plan written by early afternoon so most of the day remains for the
build. Under time pressure the cut order is fixed: Phase 2 steps 6–7 go
first (they are deferrable by design), then capture depth elsewhere — never
the level-1 core. If Phase 2 rabbit-holes, stop and descope with Roger —
e.g., accept a capture-time recording strategy for level 1 and defer deeper
reverse engineering to the milestone work.

## Appendix — external references and their limits

- https://ampcode.com/manual — Owner's Manual: thread model, CLI surface,
  settings paths, visibility. **Usage-level only** — no on-disk format, no
  token fields, no thread API. Its extractable facts are already summarized
  in "Environment facts" above.
- https://github.com/sourcegraph/amp-examples-and-guides — examples/guides
  (an `ampcode/…` org URL mirrors the same content):
  `guides/agent-file/README.md` (the AGENTS.md/AGENT.md spec — relevant to
  instruction files only, not session format),
  `guides/context-management/` (context engineering — background on token
  windows and subagent orchestration, not format truth), and
  `guides/cli/README.md` (CLI usage patterns worth skimming before Phase 2).
- Format truth comes from Phase 2 captures on this machine, stamped with
  `amp --version`. Treat all web material as orientation, never as evidence.
