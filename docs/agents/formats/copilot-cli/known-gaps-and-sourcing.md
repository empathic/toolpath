# Known gaps, sourcing, and the verification checklist

This folder was written **without first-hand session samples** (no Copilot CLI
install, no captured `~/.copilot/session-state/`). It is the one reference under
`docs/agents/formats/` with zero "Observed" entries. This page consolidates what
we are unsure of, the full source list, and the checklist to run the first time
a real session is available.

## Open questions (ranked by impact on a derivation)

1. **The exact `events.jsonl` line envelope.** Whether the payload is inline or
   nested under `data`/`payload`, whether there's a top-level `timestamp`, and
   the casing of payload keys. The only concrete casing hint is
   `session.shutdown`'s `usage.inputTokens` (camelCase). `[unverified]` — see
   [events.md](events.md#line-envelope).
2. **File-change fidelity** — inline diff/content vs. checkpoint-derived. Decides
   whether a derived `Path` gets a `raw` perspective for free or needs snapshot
   reconstruction. `[unverified]` — see [file-fidelity.md](file-fidelity.md).
3. **The `checkpoints/` on-disk format** — full file copies? a git object store?
   a patch series? And whether a checkpoint maps back to an `events.jsonl`
   position. `[unverified]`.
4. **Per-event field completeness.** The catalogue in [events.md](events.md) is
   only as complete as issue #3551 + the jonmagic post; many events have "fields
   not reported." Token-accounting specifics (is `session.shutdown`'s
   `usage.inputTokens` cumulative? does `session.compaction_complete` double-count?)
   are unknown — apply the project's "never stamp a cumulative counter as a step
   total" rule defensively (see CLAUDE.md token-accounting notes).
5. **Sub-agent storage** — are `subagent.*` turns inline in the parent
   `events.jsonl`, or in a separate file (à la Claude), or folded in (à la
   Gemini)? Decides how `DelegatedWork.turns` is populated. `[unverified]`.
6. **`workspace.yaml` exact field set** and whether it's always present.
   `[reverse-eng, Medium]`.
7. **`session-store.db` exact table/column names** (single-source paraphrase).
   `[reverse-eng, Medium]` — see [session-store-db.md](session-store-db.md).
8. **Version drift.** Everything reverse-engineered is pinned to v1.0.54; GA is
   1.0.66+. The format is explicitly internal (#3551) and may have changed.
9. **Legacy migration trigger / version** (`history-session-state/` →
   `session-state/`, "~v0.0.342"). `[unverified]`.
10. **XDG support.** No evidence of `XDG_CONFIG_HOME` / `~/.config/github-copilot/`;
    likely absent but `[unverified]`.

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
