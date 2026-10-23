# fix(claude): a projected Claude session keeps its attachments on the chain Claude walks when it resumes

Status: draft · Base: pr/109 · Source: deck v52 item 9 (context gap on Claude resume, ae1faded, docs/baseline/deletion-sensitivity.md)

## The gap, and its cause (found without a live call)
Resuming the native claude-session gave the model about 3,371 tokens of history over a fresh session; the same session through toolpath (`path p derive claude` -> `path resume --harness claude`) about 326. The suspect was the 12 `attachment` lines (environment, deferred tools, agent and skill listings, session context, date, ...).
Confirmed by reading the projected file: all 12 attachments were written, but on a SIDE BRANCH. Claude Code loads a session by following `parentUuid` from the last message. Natively the chain is `user -> 10 attachments -> assistant -> ... -> tool result -> 2 attachments -> assistant`; projected, the assistant's parent was the user directly and the attachments hung off the user/assistant as leaves the leaf never reaches.
Two causes: (1) the reader rewrites a turn's parent through attachments to the previous turn-bearing entry, and the document's turn-to-turn parent splices events out, so the attachment position is lost; (2) the attachments that followed a tool-result entry lost their anchor (the reader absorbs tool-result entries, so their uuid vanishes).

## What changes (toolpath-claude only)
- Reader (provider.rs): an attachment whose parent was an absorbed tool-result entry takes the turn that result belongs to as its parent (`absorbed_results`).
- Writer (project.rs): an event hanging off an assistant turn hangs off that turn's last tool-result entry (the existing `parent_rewrites`); `chain_attachments` then, for a view Claude recorded, puts each attachment chain between its root message and the messages that hung off the root (they now hang off the chain's last attachment), and writes the chain right after its root in the file (the parent-chain contract requires a parent before its child). Nothing else moves.
- snapshots: document@claude*, ir@claude* (event parents now name turns); no projection or cross-agent snapshot changed.

## Verification (sandbox, cargo 1.97.1, offline)
- Test first: `the_resumed_chain_reaches_the_same_attachments_as_the_native_one` (toolpath-claude/tests/resume_chain.rs) walks the chain from the leaf in the native golden and in its projection: native 12 attachments on the chain, projected 0 before the fix (red, with the two chains printed), 12 after, and the same user and assistant counts.
- toolpath-claude all tests, toolpath-goldens all tests (incl. the parent-chain contract that failed on the first attempt, which put the attachments after their children), `path-cli --features develop --test goldens`, clippy -D warnings (nix clippy 1.97.1), fmt, qualifier 238/0.
- NOT run: the live resume that measures the token gap closing (it needs the webmock record on both routes: a bound port, which this seat's sandbox forbids, and a key); so the claim is "the attachments are on the resume chain", not "the token count is now 3,371". scratchpad re-run of the two cells (pre-approved, capped) is the check. NOT run either: other slices, pinned 1.94, gitleaks, nix.

## Limits
Claude-recorded views only (`provider_id` starting with "claude"); a cross-harness view has no attachments. An attachment whose parent is neither a written message nor another attachment stays where it was.

Landing note (scratch-main-82, 2026-10-22): the fix changes the parent of three attachment steps in examples/path-02-local-session.path.json/.jsonl (each now points at its real parentUuid instead of the previous step); regenerated with scripts/gen-examples.sh and reviewed.
