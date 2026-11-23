# fix(goldens): the same-format round trip is compared in the adapter's own shape (gemini 82 -> 45 lost, opencode 87 -> 16)

Status: draft · Base: pr/113 · Source: Bobby 2026-10-22 "gemini and opencode same-agent losses (82 / 87, the biggest left)"

## Finding
Most of those 82 and 87 were not losses. The comparison was between the RECORDED file and what the writer gives back, and for these two agents the shapes differ:
- gemini records a JSONL record stream (header line, one record per message, `$set`); the writer gives one chat document, so `content` is "lost" and `messages[].content` "gained", for every field;
- opencode's `opencode export` nests `info`/`parts`; the writer gives the rows (`data`, `time_created`...), so `info.id` is "lost" and `messages[].info...` becomes `messages[].data...`.
A path that only moved counted as lost.

## What changes
`Conversation::written_shape` (toolpath-goldens/src/pipeline.rs): for gemini and opencode the same-format comparison uses the source as the adapter read it and serialises it in the shape the writer gives back; `shed::report` takes it as `compared` (the transcript stats still describe the recorded file). Other agents: unchanged.

## Verification (sandbox, cargo 1.97.1, offline)
- Test first: `same_format_is_compared_in_the_adapters_own_shape` asserts `id`, `timestamp`, `content[].text`, `info.id`, `info.title` are not in the dropped list; red before (they were), green now.
- shed@gemini: lost 82 -> 45; shed@opencode: 87 -> 16; shed@opencode-forced follows. No other set, table or golden changed (path-cli's sets are claude/codex/copilot/pi, which already compare like with like).
- toolpath-goldens all tests, `path-cli --features develop --test goldens`, fmt, clippy -D warnings (nix clippy 1.97.1), qualifier 238/0 (3 anchors re-anchored).
- NOT run: other crates' slices (untouched), pinned 1.94, gitleaks, nix.

## What is really lost now (next pieces; a value the format can hold is a writer bug)
- gemini (45): `toolCalls[].resultDisplay.*` (about 40 paths: the rich UI render of a tool result: diffStat, fileDiff, matches, recentActivity, ANSI cells), `tokens.tool`/`tokens.total`, `toolCalls[].agentId`, `directories[]`. `tokens.total` and `tokens.tool` are derivable from the usage; `resultDisplay` has no IR slot.
- opencode (16): `state.metadata.*` of tool parts (count, description, exists, filediff.*, matches, preview, model...), and the session `summary_additions/deletions/files`.
