# fix(cursor): a Cursor IDE round trip keeps the thinking text

Status: draft · renumbered on landing (author label pr/97; clashes with pr/97-baseline-compacted-resume) · Base: pr/96 · Source: deck v52 item 3

## Why
shed@cursor listed `bubbles[].thinking.text` and `bubbles[].thinking.signature` as dropped on cursor -> cursor. A real Cursor bubble records its thinking on its own `thinking: {text, signature}`
object with `allThinkingBlocks` empty beside it; the reader only looked at `allThinkingBlocks`, so the text never reached the IR and the writer could not give it back. The format holds it: a writer bug.

## What changes
- `Bubble.thinking: Option<BubbleThinking {text, signature}>` (types.rs), exported.
- Reader (provider.rs): the turn's thinking is `allThinkingBlocks` when non-empty, else `thinking.text`. A non-empty `thinking.signature` becomes the turn's agent-tagged `thinking_signature` (provider "cursor", hashed to the text).
- Writer (project.rs): writes the bubble's `thinking` object for any turn with thinking text (as real bubbles carry it; `allThinkingBlocks` is written as before). The signature goes back only when the turn's signature is Cursor's and still hashes to the text; otherwise it is "" (what real bubbles hold when nothing is signed).
- Snapshots, goldens/ (capture --all, shed) regenerated. Other harnesses' thinking now also lands in a cursor target's `thinking` object (gained, expected).

## Verification (sandbox, cargo 1.97.1, offline)
- Test first, both red before the fix (None vs "Two plus two is four."): `a_bubbles_thinking_object_is_the_turns_thinking`, `cursor_to_cursor_keeps_the_thinking_text_and_signature` (tests/projection_roundtrip.rs, fixture bubble with `thinking` and empty `allThinkingBlocks`). Green now.
- shed@cursor no longer lists `bubbles[].thinking.text` / `.signature`. `thinkingDurationMs` and `thinkingStyle` still drop: not touched here.
- toolpath-cursor, toolpath-goldens tests; `path-cli --features develop --test goldens` (baseline check); qualifier review 238 fresh; fmt; clippy -D warnings (workspace, all targets, nix-store clippy 1.97.1); workspace tests (only the known sandbox port-bind failures).
- NOT run: pinned 1.94 toolchain, gitleaks, nix, the Cursor GUI (reopening a converted chat is item 7, not mine).

## For the next PR
`thinkingDurationMs` / `thinkingStyle` on the bubble and `thinkingDurationMs` in the header grouping.
