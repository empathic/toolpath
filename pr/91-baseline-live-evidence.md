# docs(baseline): live evidence, file-backed: resume results (own session 7 of 7) and Claude deletion sensitivity

Status: draft · Base: pr/90 · Files: docs/baseline/resume-results.md, docs/baseline/deletion-sensitivity.md

## Why
The deck said "own session 7/7, cross-agent 28/28" with no file behind two own-session cells and most cross-agent cells. Every count the
baseline claims about live resume must point at an evidence file. Separately, Bobby asked which of the 36 fields a Claude → Claude round
trip drops actually matter on resume ("chaos": prove what breaks).

## What changes
- `docs/baseline/resume-results.md`: built from the evidence files only. Own session 7 of 7 loaded (claude live 2026-10-21 on the capped
  toolpath-ci workspace, claude-haiku-4-5; copilot on bobby-emp's Copilot Free, BYOK also passed as a fallback). Cross-agent: 5 file-backed
  cells, all loaded; the earlier 28/28 is withdrawn.
- `docs/baseline/deletion-sensitivity.md`: each dropped field group deleted from the native claude-session recording and resumed live in
  Claude Code 2.1.296 (13 haiku calls, $0.33). None of the 36 fields is required; only the thinking signature changes behaviour (unsigned
  thinking is silently omitted on resume). Open: the toolpath route carries about 326 history tokens vs about 3,371 natively, cause not traced.

## Verification
Evidence: `~/.lobby/ops/goldens-session-2026-10-19/there-and-back-*.md` and `deletion-sensitivity-claude-2026-10-21.md` (Bobby's machine);
docs only; `qualifier review` 0 drifted; secrets gate pass.

## For the next PR
The context gap (326 vs 3,371 tokens) and the thinking-signature fix are queued; both re-run these cells.
