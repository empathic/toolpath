# docs(formats): how each harness validates the session it resumes

Status: draft (2026-10-19, Bobby: "can we sniff how each agent does payload or type validation? extra keys, mutations and such") · Base: main · Independent, docs only

## Why
Every resume bug so far lived in how a harness's loader reads a file our projection wrote (pi's
parent chain, Copilot's errored result). The loaders' tolerance had never been written down, so the
writers and `toolpath-harness-mock` guessed it. This records it for all seven harnesses, sourced.

## What changes
- `docs/agents/formats/validation.md` (new): harness × behaviour table (extra keys, missing keys,
  wrong types, unknown entry types, malformed lines, order/parent links, file on load), every cell
  cited to `file:line` (codex, opencode, pi, gemini), a bundle line and byte offset (Copilot,
  Cursor), or a single-mutation Claude Code run against a local logging server (no model call).
  Then what was surprising, and the rules for our writers and the mock harness.
- `docs/agents/formats/README.md`, `docs/mock-harness.md`: link to it.

## Verification
- Claude Code 2.1.285: seven mutations of `test-fixtures/claude/convo.jsonl` resumed in a
  temporary `HOME` against a 400-only local server; message counts and request bodies read from its
  log (baseline 41 messages; a dropped `parentUuid` 22).
- Sources read at the pinned versions: codex `rust-v0.162.1`, opencode `v1.18.18`, pi 0.84.3 and
  gemini-cli 0.47.0 from the nix store, Copilot 1.0.61 `app.js`, Cursor 3.24.18 bundle (no launch,
  no write to its database).
- Docs only: no `site/` change, so no prettier run; no cargo.

## Paths
```
docs/agents/formats/validation.md
docs/agents/formats/README.md
docs/mock-harness.md
```
