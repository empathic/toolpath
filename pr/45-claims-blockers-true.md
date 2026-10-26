# docs(claims): the six statements recorded false on wip are true now; crates stop shipping .qual

Status: draft (2026-10-19, after pr/37: "fix the text so each becomes true, then resolve each blocker record to pass") · Base: pr/37 (claims as records) · Siblings: pr/14 / #369 owns RFC.md:303 (left as a blocker until #369 merges)

## Why
pr/37 left six claims recorded `blocker` (false on wip): the v1.1.0 kind page's `meta.source` list
missed copilot, and the README said gemini, opencode and cursor have no golden, that the gemini
reader misses 0.47 `.jsonl`, that the goldens cover four harnesses, and that no GitHub Actions
workflow runs the tests. The text was stale, not the code. Separately, the `.qual` files pr/37 and
earlier pieces placed inside published crate directories went into `cargo package`.

## What changes
- `site/kinds/agent-coding-session/v1.1.0/index.md`: `meta.source` lists `copilot` (what
  `toolpath-copilot` writes: `PROVIDER_ID`, provider.rs:19/:393, through toolpath-convo
  derive.rs:439; `document@copilot.snap` pins it). Table realigned by prettier.
- `README.md` harness table: gemini, opencode and cursor rows say what the baseline covers (a
  recording per harness, snapshots for read, write and round trip from and into each other
  harness; opencode also the `opencode-stable.db` read); gemini reads 0.47's
  `chats/session-*.jsonl` (pr/19); opencode's resume adds the headless Zen free-tier answer (pr/10).
  The paragraph under the table: recordings of all seven harnesses (nine sets) against the other
  six, and both checks run in `just ci`, which deploy-site.yml runs on every pull request. Line
  count unchanged, so no other README record moves.
- `docs/model/claims.json`: the four quotes that changed.
- `.qual`, `site/kinds/agent-coding-session/v1.1.0/.qual`: six `pass` records superseding the six
  blockers, each citing the test or snapshot; the cursor-resume concern on README.md:41 (from the
  Cursor history fix) re-anchored unchanged, since its line's text moved.
- 15 published crates' `Cargo.toml` (`path-cli`, `pathbase-client`, `toolpath`, and the
  claude, codex, convo, copilot, cursor, dot, gemini, git, github, md, opencode and pi crates):
  `exclude = ["**/.qual"]`.

## Verification
- `cargo test -p toolpath-model`: claims.rs `every_claim_has_a_live_record_at_its_text` ok,
  `committed_model_keeps_its_rules` ok.
- `qualifier review`: 230 annotations, 230 fresh, 0 drifted, 0 missing.
- `cargo test -p toolpath-goldens --test qualifier` ok; `prettier --check` over `site/` clean.
- `cargo package --list -p toolpath-claude`: `.qual` and `src/.qual` listed before, absent after.

## Status after
172 claims: 108 pinned, 64 partly, 0 false on wip. Still open: the `RFC.md:303` blocker on
main's text (resolves when #369 merges) and the `RFC.md:719` JCS key-order blocker (not a claim).

## Paths
```
README.md
site/kinds/agent-coding-session/v1.1.0/index.md
site/kinds/agent-coding-session/v1.1.0/.qual
.qual
docs/model/claims.json
crates/path-cli/Cargo.toml
crates/pathbase-client/Cargo.toml
crates/toolpath/Cargo.toml
crates/toolpath-claude/Cargo.toml
crates/toolpath-codex/Cargo.toml
crates/toolpath-convo/Cargo.toml
crates/toolpath-copilot/Cargo.toml
crates/toolpath-cursor/Cargo.toml
crates/toolpath-dot/Cargo.toml
crates/toolpath-gemini/Cargo.toml
crates/toolpath-git/Cargo.toml
crates/toolpath-github/Cargo.toml
crates/toolpath-md/Cargo.toml
crates/toolpath-opencode/Cargo.toml
crates/toolpath-pi/Cargo.toml
```
