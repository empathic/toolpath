# fix(convo): every source reader builds delegations from delegation-type tool calls

Status: draft · Base: pr/63-opencode-reads-are-not-writes · Code + recorded snapshots (cursor, gemini, opencode rows and their columns)

## Why
Asked from the deck's "gains" slide. The target-side readers (claude `extract_delegations`, gemini, pi) turned a
delegation-category tool call into a turn delegation; the others did not, so the same dispatch read differently by harness:
- cursor: `task_v2` is categorised as delegation (provider.rs tool_category) but the turn's `delegations` was always empty.
- opencode: only `subtask` parts produced delegations; the `task` tool call (the usual dispatch) produced none.
- gemini: `tool_category` had no `invoke_agent`, the name Gemini uses for a sub-agent dispatch (the recording has one,
  `invoke_agent_1778178575008_0`), so it was an uncategorised tool call and no delegation.
- codex: `spawn_agent` is categorised as delegation; no delegation was built. (No codex recording dispatches one, so
  no codex snapshot changes.)
Bug on the read side, not documented as intended.

## What changes
- `toolpath-convo`: `ToolInvocation::as_delegation()` is the one function: for a call whose category is Delegation it
  returns the delegation (agent id = call id, `prompt` input, result when the turn has one); `None` otherwise. Unit test.
- Readers use it: claude (replaces its own copy), gemini (replaces `tool_invocation_to_delegation`, and now adds
  `invoke_agent` to `tool_category`), cursor, opencode (added beside the `subtask` parts), codex (built at finish, after
  tool outputs are attached so the result is present).
- Snapshots re-recorded (INSTA_UPDATE=always, reviewed): 47 files. Every diff is one of: a `delegations` entry now present in
  `ir`/`document` for cursor, gemini and opencode; gemini's `invoke_agent` call now carrying `category: delegation` and
  projecting as a delegation to every target; the lost-key lists of round trips shrinking or the counts changing as a result
  (`key_paths`, delegation keys now present in the source side).

- `examples/graph-01-release.json` regenerated with `UPDATE_EXAMPLES=1` (its gemini step now has the delegation and
  `category: delegation` instead of `null`).

## Open: one manifest pin needs the dev shell
`path d baseline check` (the path-cli `goldens` test) now reports one problem: `copilot -> codex -> IR: round trip gains 1 key
path it did not before: paths[].steps[].change.<artifact>.structural.delegations[].result`. This is the intended gain (codex now
reads back the `spawn_agent` call the writer made for copilot's delegation), but the pin is `goldens/roundtrip/copilot-codex.json`
and the manifest lists its sha, and `capture` also rewrites the environment pins (Cargo.lock hash, nix store hash), which are only
right in the nix dev shell. So the pin is NOT refreshed here: run `path d baseline capture --all` in `nix develop`, review the diff
(expected: that one roundtrip file plus the environment lines) and commit it with this piece.

## Not changed
- pi keeps its own extraction: its prompt falls back through `input` and `instructions` and then the whole argument
  object, and its result is attached later. Moving it onto `as_delegation` would change pi's prompt text; left for a decision.
- copilot builds delegations from its own sub-agent events; untouched.
- The sub-agent's own turns (`turns`) stay empty for these readers; only gemini fills them from its separate sub-agent files.
- Nothing run unsandboxed: gitleaks, qualifier gate, `just baseline`, the pinned toolchain.
