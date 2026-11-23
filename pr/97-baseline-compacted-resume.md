# docs(baseline): a compacted session resumed live; claude loses all history through toolpath, opencode keeps full history

Status: draft · Base: pr/91 · Files: docs/baseline/compacted-resume.md

## Why
Bobby (deck, 2026-10-21): "we can run compact manually on a really small conversation - we can run test on a conversation after it is
compacted". #108's forced recordings are exactly that (the 10-step session, then `/compact`); four are in the baseline (pr/88), but no
live check had resumed one.

## What changes
- `docs/baseline/compacted-resume.md`: claude-compacted and opencode-compacted, each resumed live natively, through toolpath
  (`path p derive` → `path resume --harness <same agent>`) and as a fresh-session control.
  - claude (Claude Code 2.1.296, haiku): native answers both facts (one only in the compaction summary, one only in a preserved
    turn); through toolpath it answers NONE: all history lost. The projected file writes the `compact_boundary` after the summary,
    parented to an attachment, and drops `isCompactSummary` / `isVisibleInTranscriptOnly`; Claude Code loads only what follows the
    last boundary.
  - opencode (1.18.18, Zen free): correct through toolpath, but the projection drops the `compaction` part and `summary: true`, so all
    pre-compaction history is sent again (about 2,900 more tokens than native).
- #108's compaction IR is the fix for both; these two cells become its live acceptance test.

## Verification
Evidence: `~/.lobby/ops/goldens-session-2026-10-19/compacted-resume-2026-10-21.md` (Bobby's machine); 3 haiku calls, $0.035; opencode free.
Docs only; `qualifier review` 0 drifted; secrets gate pass.

## For the next PR
Re-run both cells (pre-approved: capped key / free tier) once #108 is in the base. codex and pi compacted cells need Bobby's OK (uncapped keys).
