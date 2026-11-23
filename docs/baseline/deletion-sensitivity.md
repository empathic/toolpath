# Deletion sensitivity: Claude Code, 2026-10-21

Question: of the 36 key paths the claude → toolpath → claude round trip drops (`shed@claude-session.snap`, `same_format.dropped`, toolpath `wip/bdelanghe` @ b9f3f78a), which does Claude Code actually need to resume a session?

Approved by Bobby on the deck ("lets run this … I can ok this").

## Method

- **Recording:** `goldens/claude-session/input.jsonl` (session `08f8ca74-…`, Claude Code 2.1.285, haiku-4.5, 29 lines). The recording has two thinking blocks and two tool calls.
- **Variants:** for each cluster below, that cluster's exact paths were deleted with `jq` (`del(.a.b[]?.c)` per path, leaf level), and every path was checked absent afterwards. `ALL` deletes all 36.
- **Run:** the mutated NATIVE file was placed in a temp HOME (`.claude/projects/<cwd>/<sid>.jsonl`) and resumed in the real Claude Code 2.1.296 (pinned nixpkgs) with `claude -p --resume <sid> --model claude-haiku-4-5 --tools ""`. One history-only question, the same as the claude-session cross cell: *FILES | LINE* (the files `ls` printed | the first line of notes.txt). Expected: `notes.txt,other.txt | first line of the notes`.
- **Why native rather than through toolpath:** toolpath's projection already drops all 36 paths. Deleting them before `path p derive` would give byte-identical projections, so it couldn't show anything. Mutating the native file isolates what Claude Code itself depends on.
- **Extra runs:**
  - `BASE`: nothing deleted.
  - `control`: a fresh session with no history.
  - `tp`: the unmodified recording through the real toolpath path (`path p derive claude` → `path resume --harness claude`, path 0.30.0 from the home generation at 133cf46b), then resumed. The `tp` probe needed one retry because my driver read the session id from the wrong flag (`-r` rather than `--resume`). That failure made no model call.
- **Keys:** read through the read-only 1Password service account (`op run`), passed in the env only, with the env scrubbed to that one variable. Temp homes only.

## Results

"Context tokens" = input + cache_read + cache_creation, as Claude Code reported it. Shared system-prompt tokens are the same in every run (the control, with no history, is 6,530). Answers are compared with whitespace ignored. The model wrote either `notes.txt,other.txt` or `notes.txt, other.txt` at random, including in unmutated runs.

| cluster | paths deleted | verdict | answer | context tokens (Δ vs BASE) | cost |
|---|---|---|---|---:|---:|
| BASE (nothing) | none | loaded | `notes.txt,other.txt \| first line of the notes` | 9,901 | $0.0336 |
| C1 thinking signature | `message.content[].signature` | **AFFECTS BEHAVIOUR** | correct | 9,770 (**−131**) | $0.0262 |
| C2 usage counters | `message.usage.{cache_creation.ephemeral_1h/5m_input_tokens, fallback_credit, inference_geo, iterations[].{cache_creation.ephemeral_1h/5m_input_tokens, cache_creation_input_tokens, cache_read_input_tokens, input_tokens, output_tokens, type}, output_tokens_details.thinking_tokens, server_tool_use.web_fetch/web_search_requests, speed}` (15) | IGNORED | correct | 9,907 (+6) | $0.0260 |
| C3 empty envelope | `message.{container, context_management, diagnostics, stop_details, stop_sequence}` (all null) | IGNORED | correct | 9,906 (+5) | $0.0260 |
| C4 message type | `message.type` | IGNORED | correct | 9,906 (+5) | $0.0261 |
| C5 tool caller | `message.content[].caller.type` | IGNORED | correct | 9,906 (+5) | $0.0262 |
| C6 request / prompt ids | `apiBlockIndex, requestId, promptId, promptSource` | IGNORED | correct | 9,908 (+7) | $0.0260 |
| C7 turn position | `turnOrigin, turnPosition.promptIndex, turnPosition.turnIndex, perTurnEffort` | IGNORED | correct | 9,906 (+5) | $0.0261 |
| C8 permission mode | `permissionMode` | IGNORED | correct | 9,906 (+5) | $0.0261 |
| C9 raw tool inputs | `wireToolInputs.<toolu_…>.{command, description, file_path, limit}` | IGNORED | correct | 9,908 (+7) | $0.0262 |
| ALL 36 | all of the above | **AFFECTS BEHAVIOUR** (from C1) | correct | 9,765 (−136) | $0.0260 |
| control (fresh, no history) | — | NONE, as expected | `NONE` | 6,530 | $0.0089 |
| tp (toolpath round trip, unmodified) | (projection) | loaded | correct | 6,856 | $0.0298 |

**Total cost: $0.3333** (13 calls, haiku). No API errors and no 429.

## What it shows

- **No dropped cluster is REQUIRED.** Every variant resumed and answered from the history correctly.
- **Only the thinking signature changes behaviour.** Without it, the resumed request carries about 131 fewer tokens, the size of the session's two thinking blocks. In other words, Claude Code silently leaves unsigned thinking out of the context it sends; there was no API error. Losing the signature means the model loses its earlier reasoning on resume. This probe didn't depend on that reasoning, but a longer task could. That supports the fix already dispatched (an Anthropic-tagged signature, restored for claude → claude).
- **The other 8 clusters (35 paths) made no difference.** Their +5 to +7 tokens fall within the noise between runs.
- **Unexplained observation (not verified):** the `tp` run's history costs about 326 tokens above the no-history control, while the native file's history costs about 3,371. Context tokens include the shared system prompt, so `tp` has about 3,000 fewer history tokens than the native resume, far more than the 36 dropped fields account for (the `ALL` run loses only 136). The cause isn't traced, but the twelve native `attachment` lines (environment, session_context, skill_listing, deferred_tools_delta, …) are the likely candidates. The answer was still correct. **Follow-up:** trace which projected content Claude Code doesn't send.

## Limits

- One recording and one probe. A probe that depended on the earlier reasoning would test C1 directly; it wasn't run here.
- Each run was done once. Model noise was judged from the runs agreeing with each other, not from repeats.
- The driver is `ds.sh` (in the scratchpad). Per-run raw outputs are summarised above; the scratch copies are deleted.
