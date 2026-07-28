# Amp harness — plan review (for Roger + Alex + Ben)

*2026-07-27 · companion to `PLAN.md` (the executable version). This is the
5-minute read for deciding whether build sessions start.*

## What we're building

Teach toolpath to treat **Amp** (ampcode.com, Sourcegraph's CLI coding agent)
like the seven harnesses it already supports: a new `toolpath-amp` crate plus
`--harness amp` everywhere, shipped as a **preview** provider exactly the way
Copilot was — purely additive, nothing about existing harnesses changes.

The goal ladder, in Alex's words, in priority order:

1. **Share**: "I can share from Amp a session to pathbase, and I can view at
   the very least the top-level beats of the conversation with attribution
   for what tokens were spent."
2. **Resume in Amp** from an Amp-shared toolpath — the resumed model answers
   a probing question about the prior session correctly.
3. **Resume in Claude Code** from an Amp-shared toolpath — same standard.
4. *(stretch)* **Resume in Amp** from a Claude-Code-shared toolpath.

## What we know about Amp so far (verified on this machine today)

- Amp is installed and logged in; the CLI is a single compiled Bun executable
  whose embedded JS we can search for endpoints and storage code.
- Amp keeps very little on disk: settings (empty right now), an API key, a
  prompt-history file — and **per-thread logs** under `~/.cache/amp/logs/`.
- Threads are **server-first**: the CLI talks to ampcode.com over a live
  JSON-RPC connection while you work. The manual documents no local thread
  store and no thread API.

**The two make-or-break unknowns** (both resolved by the first build piece,
before any Rust is written):

- **Where can we read a finished conversation from?** The per-thread log
  might contain the full conversation (great: read it like we read Copilot's
  files), or only connection noise (then we capture `--stream-json` at run
  time, or find the server API in the bundle).
- **What token numbers does Amp expose, per message or at all?** Goal 1's
  "attribution for what tokens were spent" can only be as good as what Amp
  actually reports. If the answer is "nothing per-message", we surface that
  to Alex immediately and agree on the honest ceiling (session totals only)
  before continuing.

## How the build is structured

Seven sequential pieces, each run by a fresh agent session from a paste-ready
prompt (`prompts/`), each ending in a **definition of done a reviewer can
verify by running listed commands**, a git tag (`amp-m0`…`amp-m6`), and an
append-only entry in `BUILD_LOG.md` recording what was decided and why.
No piece starts until the previous piece's gate is green.

| Piece | What it delivers | Done means |
|---|---|---|
| 00 format-recon | Run Amp for real (small, private, ≤3 threads), capture a full-featured session using the repo's standard agent-exercise script (`feature-elicit` — per team guidance, the instrument for testing any newly hooked-up agent; adapt minimally if needed), answer the two unknowns, write the format docs + test fixtures | Dossier answers the storage + token questions with version-stamped evidence |
| 01 derive-crate | `toolpath-amp`: read a session → toolpath Path | `path p import amp` output validates and renders the conversation beats |
| 02 share-wiring | **Goal 1.** Amp appears in `path share`, list, show, picker | A real Pathbase URL showing beats + tokens; **Alex accepts it** |
| 03 projector-resume | **Goal 2.** Write sessions back into Amp; `path resume --harness amp` | A projected session resumes in real `amp` and passes the probing question |
| 04 resume-into-cc | **Goal 3.** Amp session opens in Claude Code | Probing question passes inside `claude -r` |
| 05 cc-to-amp *(stretch)* | **Goal 4** + the cross-harness test matrix row | A real CC session resumes in `amp`; matrix green |
| 06 docs-release | Bookkeeping: CLAUDE.md/README/site/CHANGELOG/release script, refresh the stale how-to doc | Release checklist items all present; site + CI green |

Piece 03 is the risky one: Amp's threads live server-side, so "write a
session Amp will accept" may mean local files (like Copilot), a server API,
or may prove infeasible. The piece starts with a cheap probe (fresh ids only,
never touching real threads) and has an explicit documented-infeasibility
exit — goals 1 and 3 survive even in the worst case.

## Safety rails (hold for every piece)

- Additive only; existing tests untouched; everything preview-labeled.
- Live `amp` runs, credits, and anything network-facing get Roger's explicit
  go-ahead at the moment of use; captures stay private and are sanitized
  before entering the repo.
- Never overwrite Amp user state: fresh ids only, insert-only, never create
  Amp's own stores.
- Every format claim is stamped with the `amp --version` it was observed on
  (Amp versions look build-timestamped, so drift is expected and tracked).
- Work happens on a branch; no pushes or PR until Roger says so.

## Decisions already made (confirm or veto now)

1. **Names**: harness id `amp`, crate `toolpath-amp`, cache prefix `amp-` —
   locked with Roger this morning.
2. **Build against current `main`**, not Ben's in-flight `items-ir` /
   `compaction` branches; we budget a small reconciliation pass when those
   merge (Copilot's equivalent was ~5 files). *Ben: does the merge timing
   make this sane?*
3. **Copilot conventions throughout** (preview labeling, verbatim-rejection
   writer-contract doc, verification script, fixture-driven tests).
4. One deliberate spec correction: the IR's `Turn.extra` mechanism was
   removed from the codebase in May; the plan builds on typed IR fields only.

## What reviewers should weigh in on

- **Alex**: the goal-1 acceptance moment is the piece-02 Pathbase page — and
  the token-attribution ceiling if piece 00 finds Amp reports less than
  per-message usage. Is "session totals only" acceptable as a floor?
- **Ben**: the piece-03 three-route fork (local write / API create /
  documented infeasibility) and the items-IR sequencing above.
- **Roger**: the ⚠ moments (live runs, credits, any API probing) and the
  branch/PR flow.

**Approve** ⇒ the first build session runs `prompts/piece-00-format-recon.md`
in a fresh session, with Roger present for the go-aheads. Everything after
that is gated piece by piece.
