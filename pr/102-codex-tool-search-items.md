# fix(codex): a codex -> codex round trip keeps the tool-search items (the tool definitions)

Status: draft · renumbered on landing (author label pr/100) · Base: pr/99 · Source: pr/99 recommendation 1, group 1 (tool definitions); controller-core's order

## Why
pr/99: codex-forced loses 48 `tools[]...` paths on every target, and codex -> codex loses the same ones. They are the `tool_search_output` response item (the tool definitions Codex found) and
its `tool_search_call` (`arguments.query/limit`, `execution`, `status`). The reader keeps both as `response_item.<kind>` events; the writer only read the `turn_context` event and wrote the rest from turns, so the items vanished.
Codex's format holds them: a writer bug.

## What changes
`codex_passthrough` (toolpath-codex/src/project.rs): for a view Codex recorded (`provider_id == "codex"`), every `response_item.<kind>` event goes back as a `response_item` line with its payload verbatim,
in event order, each ahead of the first turn whose timestamp is not earlier (the remainder at the end). Other harnesses' views get none (tested). Same rule as the turn settings (pr/100): what the target cannot hold is declared in the writer contract, not copied.

## Verification (sandbox, cargo 1.97.1, offline)
- Test first: `codex_tool_search_items_are_written_back` failed (no `tool_search_call` line), passes now; `other_harness_tool_search_items_are_not_copied` guards the other direction.
- shed@codex-forced: lost 99 -> 48 key paths (the 48 tool-definition paths and the `tool_search_call` fields are gone from the dropped list), `response_item/tool_search_*` no longer in the shed event types; no other snapshot or table changed (the cross-agent projections never got them).
- toolpath-codex, toolpath-goldens (incl. contract and twice-around), `path-cli --features develop --test goldens`, qualifier review, fmt, clippy -D warnings (workspace, all targets, nix-store clippy 1.97.1), workspace tests (only the known sandbox port-bind failures).
- NOT run: pinned 1.94, gitleaks, nix, a live codex resume of the written session.

## For the next PR
pr/99 groups 2-4: replacement_history + base_instructions; current_date, timezone, workspace_roots; rate_limits + token info + timings. Placement caveat: an item whose timestamp falls between two lines of one turn is written before the next turn, not inside it.
