# Known gaps, sourcing, and the verification checklist

This folder began **without first-hand samples** and has since been partly
verified against **one session captured at `copilotVersion` 1.0.67**. This page
consolidates what that sample resolved, what's still open, the full source list,
and the checklist for the next (feature-rich) capture.

## Resolved by the 1.0.67 capture ✓

- **Line envelope**: `{type, data, id, timestamp, parentId}` — payload is under
  `data` (not inline); events form an `id`/`parentId` tree.
- **cwd + git**: under `session.start`'s `data.context`
  (`cwd`/`gitRoot`/`repository`/`branch`/`headCommit`), **not** top-level.
- **CLI version**: `copilotVersion` (top-level `version` is an int schema ver).
- **Tool correlation**: `tool.execution_start`/`complete` share **`toolCallId`**;
  tool name is `toolName`, args `arguments`.
- **Tool result content**: `data.result.content` (an object), *not* a top-level
  string — this was a real bug in the first cut, now fixed.
- **Reasoning + tokens**: `assistant.message` carries `reasoningText` (→ thinking)
  and per-message `outputTokens` (summed for the session total).
- **New types seen**: `system.message` (the ~56 KB system prompt) and a
  `session.model_change` (`{newModel}`) emitted right after start.
- **`workspace.yaml`**: flat YAML, fields observed (see
  [session-state.md](session-state.md)); `command-history-state` is a **file**.
- **Resume loader contract** (9 requirements, verbatim rejections) — fully
  mapped and verified live at 1.0.67–1.0.68, incl. a 5817-event sub-agent
  session: [writing-compatible.md](writing-compatible.md).
- **TUI rendering contract** for tool rows and diffs — the `toolRequests`
  mirror drives row dispatch; diffs need a single header + ≥1 hunk; `+N −M`
  recomputed from the diff: [file-fidelity.md](file-fidelity.md).
- **Native tool vocabulary** (`bash`/`view`/`edit`/`create` arg shapes):
  [events.md](events.md#native-tool-vocabulary-observed-10671068).
- **`session.shutdown` real shape** (`tokenDetails.{…}.tokenCount`, model-keyed
  `modelMetrics`, `codeChanges`) — observed at 1.0.68 via a feature-elicit
  capture; the parser was corrected (the reverse-eng `usage.inputTokens` shape
  was wrong). `tokenDetails.output` = Σ per-message `outputTokens` (verified).
- **Sub-agent storage** — resolved: `subagent.*` are thin markers sharing the
  `task` tool call's `toolCallId`; prompt/result live on the tool call and the
  sub-agent's own turns are **not** in the parent stream.
- **Real fixture** — a full feature-elicit session (shell, create/edit/view,
  glob+grep, errored read, sub-agent, reasoning, tokens, shutdown) captured at
  1.0.68 lives at `test-fixtures/copilot/convo.jsonl` + the crate's
  `tests/fixtures/real-session.jsonl`, and drives the cross-harness matrix and
  `real_fixture_roundtrip.rs` (forward invariants, projection fidelity,
  wire-level serde losslessness).

## Still open

1. **`checkpoints/` + `rewind-snapshots/` on-disk format** — full copies? git
   object store? patch series? `[unverified]`. (File-write fidelity itself is
   **resolved**: native `edit`/`create` embed a git-style diff inline in
   `result.detailedContent` — see [file-fidelity.md](file-fidelity.md) — so
   snapshot reconstruction is only relevant for rewind, not derivation.)
2. **Compaction token semantics** — `session.compaction_*` still unobserved;
   apply the "never stamp a cumulative counter" rule defensively when it shows up.
3. **Sub-agent transcript location** — the sub-agent's own turns aren't in the
   parent `events.jsonl`; whether they land in a sibling session dir is unknown
   (`DelegatedWork.turns` stays empty).
4. **`skill.invoked` / `hook.*` / `abort` `data` shapes** — not seen. `[reverse-eng]`.
5. **`session-store.db` exact table/column names** (single-source paraphrase).
   `[reverse-eng, Medium]` — see [session-store-db.md](session-store-db.md).
6. **`parentId` tree** — the provider derives turns sequentially and doesn't yet
   use the tree; confirm it's always linear for coding sessions.
7. **Legacy migration** (`history-session-state/` → `session-state/`). `[unverified]`.
8. **XDG support.** No evidence of `XDG_CONFIG_HOME`; likely absent `[unverified]`.

## Verification methodology

Two reproducible techniques ground this folder's `[observed]` claims beyond
static file inspection; reuse them for future verification work:

1. **Live loader loop** — project a doc into an *isolated* `COPILOT_HOME`
   (copy `~/.copilot/config.json` for auth; create a minimal
   `session-store.db` `sessions` table) and run
   `copilot --resume <id> -p "reply ok"`. The loader validates one field per
   event line, so each run either advances to the next `Session file is
   corrupted (line N: …)` rejection or loads. This is how the
   [writing-compatible.md](writing-compatible.md) contract was mapped.
2. **pty TUI capture** — the interactive renderer can't be observed with
   `-p` alone: spawn `copilot --resume` on a pseudo-tty, answer its terminal
   queries (CPR `ESC[6n`, kitty `ESC[?u`, OSC 10/11 colors), auto-accept the
   folder-trust prompt, send `ctrl+o` ("toggle all timeline") to expand tool
   bodies, and diff the captured ANSI against a native session's capture.
   This is how the rendering contract in
   [file-fidelity.md](file-fidelity.md) was found (hunkless-diff fallback,
   the `toolRequests`-mirror dispatch). Cross-check against the app bundle
   (`node_modules/@github/copilot-darwin-arm64/app.js` — minified but
   greppable: the diff heuristics, hunk regexes, and timeline mapping are
   all recoverable).

## Verification checklist

The original verify-once-we-have-samples pass is complete (envelope, event
types, field sets, tool correlation, inline diffs, `workspace.yaml`,
per-message + shutdown token semantics — all `[observed]`, and the fixtures +
tests below hold the line). Remaining boxes, tied to the open questions above:

- [ ] Inspect `checkpoints/` + `rewind-snapshots/`: record the on-disk format
      and the checkpoint→event mapping (open question #1).
- [ ] Find where a sub-agent's own transcript lands (open question #3).
- [ ] Capture a session exercising skills / hooks / abort / compaction and
      record their `data` shapes (open questions #2, #4).
- [ ] Open `session-store.db` read-only; dump the **real** schema
      (`.schema`); correct [session-store-db.md](session-store-db.md) (#5).
- [ ] Re-run `scripts/verify-copilot-live.sh` + refresh the elicit fixture
      (`docs/agents/feature-elicit.md`) after upstream Copilot releases.

## Other Copilot variants (why they're out of scope here)

- **Cloud "Copilot coding agent" (github.com)** — runs in a GitHub
  Actions-powered cloud environment; **no local session store**. Session logs
  live on github.com (PR timeline → "View session") and are reachable via the
  Agents tab / API. Derive provenance from the **PR** via the existing `github`
  provider, not from disk. `[official]`
  ([about coding agent](https://docs.github.com/copilot/concepts/agents/coding-agent/about-coding-agent)).
- **Copilot Chat in VS Code** — *does* have a local per-workspace store
  (`…/Code/User/workspaceStorage/<hash>/chatSessions/*.json`, mirrored in that
  workspace's `state.vscdb`). Structurally close to our `cursor` provider; a
  separate `copilot-vscode` reference + crate would be the right home, not this
  folder. `[reverse-eng, Medium]`
  ([community discussion](https://github.com/orgs/community/discussions/69740)).
- **Legacy `gh copilot` extension** — stateless suggest/explain; suggested
  commands go to the **shell's** history, not a session store. Nothing
  conversational to derive. `[official]`
  ([gh-copilot repo](https://github.com/github/gh-copilot)).

## Source list

| What it backs | Source | Kind |
|---|---|---|
| `~/.copilot` layout, files, `COPILOT_HOME`, `session-store.db` is SQLite, `events.jsonl` exists | [config-dir reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference) | `[official]` |
| `--continue`/`--resume`/`--session-id`/`/session` commands | [command reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference) | `[official]` |
| "session data" concept (event log) | [chronicle concept](https://docs.github.com/en/copilot/concepts/agents/copilot-cli/chronicle) | `[official]` |
| `events.jsonl` event-type list + per-event fields (v1.0.54) | [copilot-cli #3551](https://github.com/github/copilot-cli/issues/3551) | `[reverse-eng]` |
| `events.jsonl` is line-delimited (`JSON.parse` per line) | [copilot-cli #2012](https://github.com/github/copilot-cli/issues/2012) | `[reverse-eng]` |
| `session-store.db` 6-table schema; `workspace.yaml` fields | [jonmagic write-up](https://jonmagic.com/posts/github-copilot-session-search-and-resume-cli/) | `[reverse-eng]` |
| Version anchors (preview/GA/npm) | [changelog: preview](https://github.blog/changelog/2025-09-25-github-copilot-cli-is-now-in-public-preview/), [changelog: GA](https://github.blog/changelog/2026-02-25-github-copilot-cli-is-now-generally-available/), [npm](https://registry.npmjs.org/@github/copilot/latest) | `[official]` |
