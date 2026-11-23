# fix(gemini): the writer states tokens.tool and tokens.total

Status: draft · Base: pr/114 · Source: controller-core's order (real gemini losses the format can hold)

## Why
After pr/114 gemini -> gemini still lost `tokens.tool` and `tokens.total`. Gemini states both on every message: `total` = input + output + thoughts + tool (cached is inside input; checked on the recording: 8702+36+329+0 = 9067, 9133+59+10+0 = 9202). The usage has no tool-use-prompt slot, so `tool` is stated as 0.

## What changes
`tokens_from_common` (toolpath-gemini/src/project.rs) sets `tool: Some(0)` and `total` when the usage has input tokens. The older test that pinned "total absent on the fallback path" now pins `tool 0, total 150`.

## Assumption (said, not hidden)
`tool = 0` is an assumption: a real Gemini message with tool-use prompt tokens would have a nonzero value the IR cannot carry. For gemini -> gemini of a message that had one, `total` would then be short by that amount. No IR slot, none invented.

## Verification (sandbox, cargo 1.97.1, offline)
- `tokens_from_common_states_tool_and_total` written with the change (the red step was not observed separately: the existing fallback test, which asserted `total` absent, failed on the change and was updated on purpose).
- shed@gemini: `tokens.tool`/`tokens.total` no longer listed as lost; every cross-agent -> gemini projection gains the two (snapshots regenerated; the writer/contract checks pass).
- toolpath-gemini, toolpath-goldens, `path-cli --features develop --test goldens`, fmt, clippy -D warnings (nix clippy 1.97.1), qualifier 238/0.
- NOT run: other crates' slices, pinned 1.94, gitleaks, nix, a live gemini resume.

## Not changed, and why
- `toolCalls[].agentId`: the id of the sub-agent a tool call started. The IR only has it when the sub-agent's own chat file is read (a delegation); the recording has none, so there is nothing to write it from. No slot invented.
- `directories[]` in the shed lists is GAINED (the pipeline passes the project path), not lost.
- `toolCalls[].resultDisplay.*` (~40 paths): the UI render of a tool result. No IR slot; a declaration is the honest outcome, to be decided with the owner of the kind.
