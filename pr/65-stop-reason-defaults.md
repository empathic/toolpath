# fix(opencode, pi): a turn that ended in tool calls is not written as `stop`; the defaults are declared

Status: draft · Base: pr/64-delegations-from-tool-calls · Code + recorded snapshots (opencode and pi columns, shed of four sources) + the contract test

## Why
Asked from the deck's "gains" slide: where does `stop` come from on a turn that called a tool, and is it documented.
- opencode `project.rs` wrote the step-finish `reason` as `"stop"` for any turn that carried no stop reason, tool calls or not.
- pi `project.rs` `parse_stop_reason` fell back to `Stop` the same way.
Both are wrong for a turn that ended in tool calls (opencode itself writes `tool-calls`, pi writes `toolUse`), and neither
default was declared anywhere. Bug on the write side; the default was not documented.

## What changes
- opencode: no stop reason and tool calls -> `tool-calls`; no stop reason and none -> `stop`. A turn's own stop reason still wins.
- pi: no stop reason and tool calls -> `toolUse`; none -> `stop`. `parse_stop_reason` takes `has_tool_calls`.
- Declared: new `toolpath_opencode::project::SYNTHESIZED` (`providerID`=anthropic, `modelID`=unknown, `version`=0.0.0-projected;
  the finish reason is turn-dependent, so it is in the doc comment, not the table). pi's doc comment states its default the
  same way; a `stopReason`=stop table entry was tried and dropped because no recording has a text-only turn without a reason,
  so the test could not see it written.
  `declared_synthesized_values_are_written` now covers opencode, and accepts pretty-printed JSON (opencode's session text).
- Snapshots re-recorded (INSTA_UPDATE=always, reviewed): the opencode and pi projections of codex, copilot, cursor and gemini
  (31 step-finish `stop` -> `tool-calls` for opencode; the pi message lines gain `"stopReason":"toolUse"`) and the shed of
  those four sources. claude, opencode and pi sources are unchanged (their turns carry a stop reason).

## Open: output goldens need the dev shell
`path d baseline check` reports `codex -> pi` and `copilot -> pi` output differs (`goldens/codex/to-pi.jsonl`,
`goldens/copilot/to-pi.jsonl`: `"stopReason":"stop"` -> `"toolUse"`, intended), on top of pr/64's copilot -> codex pin. Same
remedy for both pieces: `path d baseline capture --all` in `nix develop`, review (expected: those files, the roundtrip pin and
the environment lines), commit.

## Not changed
- The `parse_stop_reason` fallback still maps an unknown string to the default; only the default changed.
- Nothing run unsandboxed: gitleaks, qualifier gate, `just baseline` (the manifest sets may pin shed/round-trip rows for these
  writers: check), the pinned toolchain.

## Note
`projection@codex@pi.snap`'s `input_file` header moves to `test-fixtures/codex/convo.jsonl` (from the stale `goldens/codex/input.jsonl`)
because insta rewrites the header from what the test passes now; the body change is only `stopReason`.
