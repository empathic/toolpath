# GitHub Copilot CLI on-disk format

> **Reference revision:** 2026-06-30
> **Tracks:** `@github/copilot` (the standalone agentic CLI, command `copilot`)
> **Version anchors:** public preview 2025-09-25, GA 2026-02-25, npm `1.0.66`;
> reverse-engineered event details observed at `1.0.54`.
>
> When you change anything in this directory, bump the revision date here.

This folder documents the on-disk session format of the **standalone agentic
GitHub Copilot CLI** — the npm package [`@github/copilot`](https://registry.npmjs.org/@github/copilot/latest)
(command `copilot`), GitHub's terminal coding agent and its answer to Claude
Code / Codex CLI / Gemini CLI. It is **not** about:

- the older `gh copilot` gh-extension (stateless suggest/explain — no session store);
- the cloud "Copilot coding agent" on github.com (runs server-side; no local
  session file — derive that from the PR via the existing `github` provider);
- Copilot Chat inside VS Code (a VS Code `state.vscdb` store, structurally closer
  to our `cursor` provider).

See [known-gaps-and-sourcing.md](known-gaps-and-sourcing.md) for the one-paragraph
rationale on each of those.

## ⚠️ Sourcing posture — read this first

Unlike every other doc under `docs/agents/formats/`, this reference is **not built
on first-hand session samples.** At the time of writing we had no Copilot CLI
install and no captured `~/.copilot/session-state/` directory to inspect. This
folder is compiled from:

- **Official GitHub documentation** — the config-directory and command
  references and the "session data" concept page. High confidence; this is the
  documented surface.
- **Community reverse-engineering** — chiefly copilot-cli issue
  [#3551](https://github.com/github/copilot-cli/issues/3551) (an enumeration of
  `events.jsonl` event types at v1.0.54) and the
  [jonmagic write-up](https://jonmagic.com/posts/github-copilot-session-search-and-resume-cli/)
  (the `session-store.db` schema and `workspace.yaml`). Medium confidence,
  version-specific, and explicitly *not* an official schema.
- **Structural inference** — our own reasoning where no source spells something
  out. Labeled as such; treat as a hypothesis to verify against a real session.

Every non-trivial claim carries an inline tag:

| Tag | Meaning | Default confidence |
|---|---|---|
| `[official]` | Stated in GitHub's published docs (URL given). | High |
| `[reverse-eng]` | Observed by a community source at a named version. | Medium |
| `[inferred]` | Our structural reasoning; no direct source. | Low |
| `[unverified]` | Believed but unconfirmed; flagged for sample verification. | — |

This mirrors the `Observed` / `Expected` / `guess` convention the Claude Code
docs use — the honest twist being that we have **zero "Observed" entries yet.**
The first time anyone captures a real `events.jsonl`, this folder should be
re-graded against it and the tags upgraded (or corrected). The
[verification checklist](known-gaps-and-sourcing.md#verify-once-we-have-samples)
is the to-do list for that pass.

## How the docs are organized

1. **[directory-layout.md](directory-layout.md)** — the full `~/.copilot/`
   inventory: config/settings/MCP/permissions files, the `session-store.db`
   index, the `session-state/` history tree, and the `COPILOT_HOME` /
   `COPILOT_CACHE_HOME` overrides.
2. **[session-state.md](session-state.md)** — the per-session directory
   `session-state/<session-id>/`: `events.jsonl`, `workspace.yaml`,
   `checkpoints/`, and how sessions are keyed and named.
3. **[events.md](events.md)** — the `events.jsonl` line envelope and the
   ~20 dotted-namespace event-type catalogue (`session.*`, `user.message`,
   `assistant.*`, `tool.execution_*`, `subagent.*`, `skill.*`, `hook.*`,
   `abort`) with per-event fields where known.
4. **[session-store-db.md](session-store-db.md)** — the cross-session SQLite
   index: its six tables and how it relates to `events.jsonl`.
5. **[file-fidelity.md](file-fidelity.md)** — how file edits are captured
   (tool-call args + checkpoint snapshots, **not** inline diffs), the contrast
   with Codex's `patch_apply_end`, and what that means for a future toolpath
   `raw` perspective.
6. **[resume-and-sessions.md](resume-and-sessions.md)** — the CLI flags and
   slash commands that list, resume, and manage sessions; the surface a future
   projector / `path resume` integration must match.
7. **[known-gaps-and-sourcing.md](known-gaps-and-sourcing.md)** — the
   consolidated open questions, the full source list, and the
   "verify-once-we-have-samples" checklist.

## Conventions

- **Field names** are shown as they appear on disk. Event `type` strings use a
  dotted namespace (`tool.execution_complete`); we have not confirmed the case
  convention of the *payload* keys (see [events.md](events.md)).
- **Versions in parentheses** (e.g. "v1.0.54") are what a source observed, not
  what GitHub tagged a format change at.
- **Keep headings anchor-stable** — cross-links use GitHub auto-anchors.

## Maintenance

This is the single place Copilot CLI format knowledge should accumulate. When a
future `toolpath-copilot` derive crate learns something the hard way — a real
event line, a payload key, a fidelity quirk — record it here in the same change
and **upgrade the confidence tag** from `[inferred]`/`[unverified]` to something
sample-grounded.
