# fix(webmock): replay JSON-RPC answers with the live id; key JSON-RPC calls by method

Status: planned (asked 2026-10-19 on the goldens demo deck: "can we dig into the copilot issue") · Base: the webmock piece (pr/13) once merged

## Why
The demo's replay table said copilot "miss · GitHub's remote MCP tool list changed (25 → 19)". It was our replay, not
copilot or GitHub. Copilot talks to GitHub's remote MCP server (`/mcp/readonly`, JSON-RPC over SSE) and runs two
`tools/list` calls at once (ids 1 and 2). Both keyed exact (`id` is already volatile), but the replayed answer carried
the recorded `"id":1`, so call 2 never resolved. Copilot waited out its 60 s MCP timeout, sent
`notifications/cancelled` and an id-3 retry; those keyed loose, and every MCP call shared one loose key (no model, no
user text), so they got the `initialize` answer. Copilot then ran with no MCP tools: its model request carried 19
tools instead of 25 and every `POST /v1/messages` missed.

## What changes
- `crates/toolpath-webmock/src/lib.rs`: on replay, when the live and recorded bodies are both JSON-RPC calls with an
  id, the answer's top-level `"id"` (a JSON body, or each SSE `data:` line) is rewritten to the live id
  (`rewrite_jsonrpc_id`, `jsonrpc_ids`); the rest of the answer is byte-identical, sent as one chunk. A JSON-RPC body's
  `method` is part of its loose key (`jsonrpc_method`); no other body's loose key changes.
- Tests: three MCP calls get three loose keys and an id-3 `tools/list` retry falls back to `tools/list`; ids do not
  change the exact key; a non-JSON-RPC loose key equals the old formula; the replayed answer carries the live id (SSE
  and JSON, nested `"id"`s untouched); across the 33 committed cassettes only the 4 JSON-RPC ones (copilot's 3, codex's
  `ps/mcp` initialize) change loose key. The stored `key`/`loose_key` fields came from the Bun prototype's hashing and
  never matched `keys()`; the store recomputes both at open, so no cassette is re-keyed.
- `cassettes/README.md`: copilot row and what the miss was; qualifier `pass` with the before/after run; a `concern`
  (declared-limit) that committed model cassettes carry `<scratch>` paths and so do not replay exactly.

## Verification
End to end, replay only (no upstream, bogus GitHub token), the copilot resume cell (`v3-cell-codex-to-copilot`,
copilot CLI 1.0.61) against the Rust `webmock` binary built before and after this change, same pre-scrub cassettes:
before rc 1 in 152 s, `tools/list` id 2 answered with id 1, 3 LOOSE `initialize` answers, 6 MISS on
`/v1/messages`, 19 tools sent; after rc 0 in 1 s, every request EXACT ("jsonrpc id 1 -> 2"), 25 tools, the recorded
reply ("You're going to walk through END"). With the committed (scrubbed) cassettes the MCP part replays the same, and
`/v1/messages` still misses only on the `<scratch>` path in the system prompt (the concern above).
Gates: fmt, clippy `--workspace --all-targets --features path-cli/goldens -D warnings`,
`RUSTFLAGS=-D warnings cargo test --workspace --features path-cli/goldens`, doc, shellcheck.

## Paths
```
crates/toolpath-webmock/src/lib.rs
crates/toolpath-webmock/src/.qual
crates/toolpath-webmock/cassettes/README.md
crates/toolpath-webmock/cassettes/.qual
```
