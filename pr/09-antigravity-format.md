# docs(formats): Antigravity CLI format reference (derive-first adapter proposal)

Status: opened #364 (draft, 2026-10-16) — was: draft · Base: main · Independent

Commits: `docs(formats): antigravity …` (baca3b5).

## Why
Google has moved individual Gemini accounts to Antigravity; `agy` is on this machine (1.1.13 in the home profile; 1.3.6 released 2026-10-15). It is not Gemini CLI re-skinned: a closed Go binary on the Windsurf "Cascade" engine with its own store, so `toolpath-gemini` does not apply and a reference is the first step toward an adapter.

## What changes
`docs/agents/formats/antigravity.md` (+ README entry): the store under `~/.gemini/antigravity-cli/conversations/<uuid>.db` (SQLite with protobuf blobs; `<uuid>.pb` on older builds), `conversation_summaries.db`, `cache/last_conversations.json`, observed by running `agy -p` in a throwaway HOME; the on-disk schema recovered from the binary's embedded descriptors (`gemini_coder.Conversation/Trajectory/Step`, `CortexStepMetadata`, `UserInput`, `PlannerResponse`, `ModelUsageStats`, `ChatToolCall`); headless (`-p --output-format json`, `--input-format stream-json`, `--conversation <uuid>`).

## Recommendation
New adapter, derive-first (rusqlite + prost over the recovered proto subset). For a matrix column now: headless-only `--harness antigravity` feeding the transcript through `--input-format stream-json`, no inception into a closed binary's store. Keep `toolpath-gemini` for enterprise accounts and archives. Blocked on Bobby's `agy` sign-in for a real conversation sample.

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs,
a file touched by several PRs belongs to the earliest):
```
docs/agents/formats/README.md
docs/agents/formats/antigravity.md
```
