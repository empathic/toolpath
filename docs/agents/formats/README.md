# Agent session formats

This directory holds our working reference for the on-disk formats emitted by
coding agents whose sessions we derive `toolpath` documents from. These are the
documents we would like external consumers (other toolpath crates, workshop,
etc.) to be able to trust without having to reverse-engineer the format
themselves from a sampled `~/.claude/projects/…` directory.

The goal is **practitioner-grade reference**: exactly what fields appear, what
they mean, where the format has quirks or bugs, and how our own code copes with
them. Not a spec — we don't own any of these formats. But close enough that a
new contributor can add a derivation or a projector without a week of cargo-
culting.

## Contents

- **[`claude-code/`](claude-code/README.md)** — Claude Code
  (`~/.claude/projects/…` JSONL). Split into focused docs covering
  directory layout, the JSONL line envelope, entry types, the `message`
  object and content parts, tool invocation lifecycle, session chains
  and compaction, peripheral files, writing-compatible JSONL, known
  issues, a line-by-line walkthrough, and a version-keyed format
  changelog. Each revision of the reference carries a date stamp at
  the top of the subdirectory's README.
- **[`codex.md`](codex.md)** — Codex CLI rollout files under
  `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Single-file reference
  covering the date-bucketed session format and the `patch_apply_end`
  events that drive file-change fidelity.
- **[`cursor.md`](cursor.md)** — Cursor's split storage: the JSONL
  agent transcript at `~/.cursor/projects/<slug>/agent-transcripts/`
  and the rich SQLite bubble store at
  `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`,
  with content-addressed file blobs and the `cursor-commits`
  checkpoint directory.
- **[`gemini.md`](gemini.md)** — Gemini CLI chats under
  `~/.gemini/tmp/<project>/chats/`, including the main-file + sibling
  sub-agent UUID directory layout.
- **[`openclaw/`](openclaw/README.md)** — OpenClaw, a multi-channel
  local-first assistant. JSONL transcripts under
  `~/.openclaw/agents/<id>/sessions/`, with a tree/visible-leaf entry
  model and channel/peer-keyed sessions. Split into focused docs covering
  directory layout, the version-3 envelope, the ten entry types, messages
  and content blocks, tools and file-ops, token usage, lineage, the
  channel/actor identity model, known issues, a walkthrough, and a format
  changelog. Sourced from upstream code only — no first-hand sample yet.
- **[`opencode.md`](opencode.md)** — opencode's SQLite database
  (`~/.local/share/opencode/opencode.db`), its 12 typed message-part
  variants, and the sibling bare-git snapshot repo used for file diffs.
- **[`pi.md`](pi.md)** — Pi (pi.dev) terminal coding agent sessions
  under `~/.pi/agent/sessions/--<encoded-cwd>--/`, JSONL with a
  session header line plus tree-shaped entries.

The Claude Code reference is the most detailed because it's the
longest-standing provider and has the most moving parts (JSONL
envelope variants, session chaining, compaction, sidechains, and the
loader's own undocumented strictness on what it will accept). OpenClaw
also gets a folder — its tree/visible-leaf entry model and
multi-channel actor identity have enough moving parts to warrant the
breakout. The rest sit in single files because their formats are
either simpler or sufficiently covered there.

## Conventions used in these docs

- **"In the wild"** = observed in real JSONL files on disk, not just in types
  we've defined.
- **Field tables** show the name as it appears in JSON (so `parentUuid`, not
  `parent_uuid`), its shape, and whether it's optional. "Optional" means we've
  seen entries without it; "required" means we've never seen an entry missing
  it (not that the format promises it'll always be there).
- **Citations** point either to files under this repo (`crates/<name>/src/…`)
  or to external sources (marked with URLs). Repo citations dominate — we
  trust our own parsers and tests more than we trust blog posts.
- **Version numbers** when quoted (e.g. "Claude Code 2.1.90") are what we've
  seen in sample data, not what Anthropic has officially tagged a format
  change at.

## Maintenance

When `toolpath-claude` (or its siblings) learns about a new field, entry type,
or edge case, update the corresponding doc here in the same change. The point
of this directory is to be the single place where format knowledge
accumulates; if the knowledge only lives in code comments or commit messages,
it effectively doesn't exist.
