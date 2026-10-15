# docs(formats): format references for Factory Droid, Goose, Qwen Code, Cline CLI and Hermes Agent

Status: draft · Base: main · Independent

Commits: `docs(formats): …` (30e2c0a), 1,138 lines, five drafts.

## Why
A survey of 23 harnesses ranked these five as the next adapter candidates by on-disk format, resume support and adoption. Each draft is in the shape of the existing references (opencode.md, pi.md) so an adapter can be written from it; every claim is marked read (source cited) or inferred.

## What is in each
- **Droid 0.242.0** (release binary, sha256-verified; not in nixpkgs): `~/.factory/sessions/<cwd with / → ->/<uuid>.jsonl` + `<uuid>.settings.json` (session-level token totals) + a SQLite index; observed from a credential-less `droid exec`. Message shapes (Anthropic-style blocks) from the binary's zod validators.
- **Goose 1.49.0** (`nixpkgs#goose-cli`): full `sessions.db` DDL observed (schema 16). Lever: `goose session import` accepts Claude Code, Codex and Pi JSONL, so projection can reuse existing projectors instead of writing SQLite.
- **Qwen Code 0.23.4** (`nixpkgs#qwen-code`): `~/.qwen/projects/<sanitizeCwd>/chats/<uuid>.jsonl` (from source; the survey's path was wrong); full `ChatRecord` type.
- **Cline CLI 3.0.68** (`nixpkgs#cline`): `<id>.json` manifest, `<id>.messages.json`, `db/sessions.db` with an absolute `messages_path` a projection must upsert.
- **Hermes 0.19.0** (PyPI sdist; not in nixpkgs): `state.db` schema 22 from `SCHEMA_SQL`; FTS by triggers; two `tool_calls` shapes; resume by id, title or prefix.

## Verification
Each draft names the single authenticated run that would turn its inferred parts into observed ones. None has a real model-turn sample yet. Summary table: `review/harness-formats.md`.

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs,
a file touched by several PRs belongs to the earliest):
```
docs/agents/formats/cline.md
docs/agents/formats/droid.md
docs/agents/formats/goose.md
docs/agents/formats/hermes.md
docs/agents/formats/qwen-code.md
```
