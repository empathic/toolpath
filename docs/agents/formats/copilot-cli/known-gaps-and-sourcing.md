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

## Still open (not exercised by that session)

1. **File-change fidelity** — the sample only ran `bash`/`view` (no file writes),
   so whether edits embed content/diffs or rely on `checkpoints/`/`rewind-snapshots/`
   remains `[unverified]` — see [file-fidelity.md](file-fidelity.md).
2. **`checkpoints/` + `rewind-snapshots/` on-disk format** — full copies? git
   object store? patch series? `[unverified]`.
3. **Token accounting completeness** — the sample had no `session.shutdown`
   (open session); is `outputTokens` per-message final? does `shutdown`/compaction
   double-count? Apply the "never stamp a cumulative counter" rule defensively.
4. **Sub-agent storage** — `subagent.*` did not occur; are its turns inline or in
   a separate stream? Decides `DelegatedWork.turns`. `[unverified]`.
5. **`skill.invoked` / `hook.*` / `abort` `data` shapes** — not seen. `[reverse-eng]`.
6. **`session-store.db` exact table/column names** (single-source paraphrase).
   `[reverse-eng, Medium]` — see [session-store-db.md](session-store-db.md).
7. **`parentId` tree** — the provider derives turns sequentially and doesn't yet
   use the tree; confirm it's always linear for coding sessions.
8. **Legacy migration** (`history-session-state/` → `session-state/`). `[unverified]`.
9. **XDG support.** No evidence of `XDG_CONFIG_HOME`; likely absent `[unverified]`.

## Verify once we have samples

Run this the first time a real `~/.copilot/session-state/<id>/` exists. Capture
it via the feature-elicit flow (`docs/agents/feature-elicit.md`) once `copilot`
is on `$PATH` and authenticated; commit a sanitized fixture under
`test-fixtures/copilot/`.

- [ ] Dump a few raw `events.jsonl` lines verbatim. Confirm the **envelope**:
      inline vs. `data`-nested payload, top-level `timestamp`, payload-key casing.
      Update [events.md](events.md) and upgrade `[inferred]` → observed.
- [ ] Enumerate the **actual `type` strings** present; diff against the
      [events.md](events.md) catalogue. Note any new/renamed/missing types and
      the version they were seen at.
- [ ] For each event type, record the **real field set**; fill the "not
      reported" gaps in the catalogue.
- [ ] Determine how a `tool.execution_complete` **correlates to its
      `tool.execution_start`** — is there an id, or only implicit ordering? The
      provider pairs by id when present and falls back to positional pairing
      otherwise (see [events.md](events.md)); confirm which is real and tighten
      if an id field exists under a name we don't yet check.
- [ ] Inspect a `tool.execution_complete` for a file edit: **is the diff/new
      content inline?** Resolve open question #2 and rewrite
      [file-fidelity.md](file-fidelity.md) accordingly.
- [ ] Inspect `checkpoints/`: record the on-disk format and the
      checkpoint→event mapping (open question #3).
- [ ] Capture a sub-agent session; determine where `subagent.*` turns live
      (open question #5).
- [ ] Read `workspace.yaml` and confirm field names/presence.
- [ ] Open `session-store.db` read-only; dump the **real** schema
      (`.schema`); correct [session-store-db.md](session-store-db.md).
- [ ] Trace token accounting across a multi-turn session; determine whether
      `session.shutdown` / `session.compaction_complete` counts are cumulative,
      and document the per-step attribution rule.
- [ ] Re-grade every confidence tag in this folder against the sample.

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
