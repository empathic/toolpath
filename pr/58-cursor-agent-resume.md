# feat(cursor): `path resume --harness cursor-agent` writes the Cursor CLI's chat store and resumes it

Status: draft · Base: pr/38-cursor-resume-history (it shares that piece's `model_messages`; independent of it otherwise)

## Why
`path resume` could target Cursor's IDE but not its CLI. `cursor-agent` keeps chats in its own SQLite store, which nothing
in toolpath wrote. A spike (2026-10-19) read the store's layout out of the CLI bundle (nixpkgs `cursor-cli`
0-unstable-2026-08-11) and wrote one from scratch. `cursor-agent ls` then listed it, run in a temp HOME with no login.

## What changes
- toolpath-cursor:
  - New `proto` module: varint and length-delimited encode, plus a field reader. Both Cursor writers use it.
  - `project::model_messages` is split out of the IDE's `model_history`. It builds the AI SDK `ModelMessage` history
    once. The IDE wraps it as `conversationState`, and its bytes are unchanged (no IDE snapshot moved because of this).
  - New `agent_store` module:
    - `AgentStoreProjector` builds an `AgentStore`: sha256-keyed blobs, `meta['0']` as hex JSON, and a
      `ConversationStateStructure` root. Root field 1 lists the ModelMessage blobs. Field 8 lists the rendered turns:
      `UserMessage{text, message_id}`, then thinking and assistant steps.
    - `AgentStore::write` and `read` handle `store.db` (WAL, `user_version` 1). `root`, `turns`, `model_messages` and
      `to_view` decode it.
    - `config_dir_from_env` resolves `CURSOR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cursor`, else `~/.cursor`. `store_path`
      gives `<config>/chats/<md5(cwd)>/<agentId>/store.db`.
    - The blob encryption key is random per chat (two v4 UUIDs). Deps: `md5` (already in the lock) and `uuid`.
- path-cli:
  - New `Harness::CursorAgent` (`--harness cursor-agent`). `Harness::artifact_type()` now returns `Option`, because
    cursor-agent is a resume target only. `share --harness cursor-agent` errors and says so.
  - `projection/cursor_agent.rs` writes the store for `-C`. If a chat with the same id already exists, it is left alone
    and resumed. `argv_for` gives `cursor-agent --resume <id>`.
  - `tests/support` `ScopedHome` now also pins `$XDG_CONFIG_HOME` and clears `$CURSOR_CONFIG_DIR`. This dev box sets
    `XDG_CONFIG_HOME`, so without this a test would have written into the real `~/.config/cursor`.
  - `help@resume` and `help@share` list `cursor-agent`.
- toolpath-goldens:
  - `Agent::CursorAgent` and `Agent::TARGETS`. `Agent::ALL` stays the seven recorded harnesses, because `observed.rs`
    needs a recording per agent.
  - Pipeline: the cursor-agent projection is the store as JSON (`{"text"}` for UTF-8 blobs, `{"hex"}` for protobuf).
    The key is pinned to `PINNED_BLOB_KEY` so the projection is the same on every run. The round trip reads it back
    through `AgentStore::to_view` and the shared derive.
  - New snapshots: `projection@<set>@cursor-agent` and `round_trip@<set>@cursor-agent` for all nine sets, plus
    `writer@cursor-agent`. In the existing `shed@*`, `validate@*` and `validate_transforms@*`, only the `cursor-agent`
    keys were added. They were grafted in by key, so none of the IDE changes the other agent is re-recording came along.
  - `fixtures/contract/cursor-agent.json` declares the writer's 33 drops. Two are marked as defects: the reader does not
    rebuild file mutations from the tool-call args it keeps. `tests/contract.rs` and `transforms_schema.rs` now iterate
    over `TARGETS`.
- Docs:
  - `docs/agents/formats/cursor.md` has a new section: "The Cursor CLI's chat store (`cursor-agent`)".
  - `README.md` has a new harness row.
  - `CHANGELOG.md` has a new entry. It also fixes the old line that said the store is always under `~/.cursor/chats`.
    It is under `CURSOR_CONFIG_DIR` / XDG.
- `.qual`:
  - New concern at `agent_store.rs:28`: the live check is pending.
  - Re-anchored with `--supersedes`: three `pipeline.rs` records (one re-anchors 01b41a4's own copy) and the 17 README
    records that the new row shifted.

## Verification
- Unit tests, toolpath-cursor (`agent_store`):
  - `root_blob_decodes_to_the_history_and_the_turns` decodes the root blob by hand, field by field.
  - `the_cli_and_the_ide_list_the_same_history`, `meta_is_what_the_cli_writes`, `store_db_round_trips`,
    `the_view_reads_back`, `the_json_form_round_trips`.
  - `store_path_hashes_the_cwd_like_the_cli` checks against an md5 vector.
  - `proto` has three tests.
- path-cli tests:
  - `projection::cursor_agent` writes a store, reads it back, and checks that a second resume leaves it alone.
  - `harness::tests` checks that every name parses back and that cursor-agent has no artifact type.
  - Integration test `file_input_explicit_cursor_agent_projects_and_records_exec` reads the store from the pinned
    `$XDG_CONFIG_HOME/cursor`.
- No-model check, 2026-10-19:
  - `path resume <codeword doc> --harness cursor-agent -C <tmp>/proj` ran in a temp HOME with `HOME`,
    `XDG_CONFIG_HOME`, `CURSOR_CONFIG_DIR`, `CURSOR_DATA_DIR`, `AGENT_CLI_CREDENTIAL_STORE=memory` and
    `NO_OPEN_BROWSER=1` all set. The exec went to a fake `cursor-agent`.
  - It printed `Exported cursor-agent chat 5b0f3c1e-7d4a-4f2b-9c61-0e8a2d7b4c19 (2 history messages, 1 turns)`.
  - The real `cursor-agent ls` (cursor-cli 0-unstable-2026-08-11) then ran from the project directory under a
    160x40 pty, sent no keys, and was killed after 15 s. It listed:
    `→ Remember this codeword for later: PELICAN-4417. Just acknowl   Today 08:18 PM`
  - The CLI wrote its `meta.json` cache next to the store. Real `~/.cursor/chats` and `~/.config/cursor/chats` still
    do not exist.
- Gates, on the rebased tree (01b41a4 + this piece; Rust 1.94.1, the pin is 1.94.0):
  - `cargo fmt --check`: ok.
  - `clippy --workspace --all-targets --features path-cli/develop,toolpath/verify -D warnings`: ok.
  - `cargo doc -D warnings`: ok.
  - `qualifier review`: 0 drifted, 0 missing.
  - `cargo test --workspace` (same features): the failures are only `toolpath-goldens` `set_*` (9) on
    `projection@<set>@cursor` and `shed@cursor`, plus `writer_cursor`. These are the Cursor IDE snapshots that 7a93457
    changed. Their re-record is `handoff/cursor-snapshots` (pr/38) and is not in this piece. Every cursor-agent snapshot
    passes. Note that insta stops a `set_*` test at its first failing snapshot, so the per-set cursor-agent snapshots
    were checked with `INSTA_FORCE_PASS` until that lands.
  - gitleaks 8.30.1 with `.gitleaks.toml`: 3 findings, all in files this piece does not touch (`observed.rs:101`,
    `webmock/src/run.rs:179,194`). The base has the same 3.
- Not verified: that `cursor-agent --resume <id>` sends the history to the model. Also unknown: whether the CLI needs
  root fields 3–39 or `UserMessage` fields beyond `text` / `message_id`. That needs the live check below.

## Live check (for Bobby; needs `CURSOR_API_KEY` in your own shell; makes model calls; not run)
Run it from a checkout of this piece. It writes only under a fresh `mktemp -d`. Two parts: the resume cell must quote
the codeword, and the negative control (a random chat id) must not.

```bash
set -eu
: "${CURSOR_API_KEY:?export CURSOR_API_KEY in your own shell first}"
cargo build -q -p path-cli
PATHBIN="$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)/debug/path"
T=$(mktemp -d); mkdir -p "$T/home" "$T/proj" "$T/fake"
cat > "$T/codeword.json" <<'EOF'
{"graph":{"id":"path-codeword-check"},"paths":[{"meta":{"source":"claude-code","kind":"https://toolpath.net/kinds/agent-coding-session/v1.1.0","title":"codeword check"},"path":{"id":"path-codeword-check","head":"s2"},"steps":[
{"step":{"id":"s1","actor":"human:user","timestamp":"2026-10-19T12:00:00Z"},"change":{"claude-code://5b0f3c1e-7d4a-4f2b-9c61-0e8a2d7b4c19":{"structural":{"type":"conversation.append","role":"user","text":"Remember this codeword for later: PELICAN-4417. Just acknowledge."}}}},
{"step":{"id":"s2","parents":["s1"],"actor":"agent:claude-opus-4-7","timestamp":"2026-10-19T12:00:05Z"},"change":{"claude-code://5b0f3c1e-7d4a-4f2b-9c61-0e8a2d7b4c19":{"structural":{"type":"conversation.append","role":"assistant","text":"Acknowledged. I will remember the codeword."}}}}]}]}
EOF
SANDBOX=(HOME="$T/home" XDG_CONFIG_HOME="$T/home/.config" CURSOR_CONFIG_DIR="$T/home/cfg"
  CURSOR_DATA_DIR="$T/home/data" TOOLPATH_CONFIG_DIR="$T/home/.toolpath"
  AGENT_CLI_CREDENTIAL_STORE=memory NO_OPEN_BROWSER=1)
# 1. Write the store. path resume execs `cursor-agent --resume <id>`; a fake on PATH records the id.
printf '#!/bin/sh\necho "$2" > "%s/id"\n' "$T" > "$T/fake/cursor-agent"; chmod +x "$T/fake/cursor-agent"
env "${SANDBOX[@]}" PATH="$T/fake:$PATH" "$PATHBIN" resume "$T/codeword.json" --harness cursor-agent -C "$T/proj"
ID=$(cat "$T/id"); echo "chat id: $ID"
cd "$T/proj"
Q='What codeword did I ask you to remember earlier in this chat? Reply with the codeword only, or NONE.'
# 2. Resume cell: the projected chat.
env "${SANDBOX[@]}" CURSOR_API_KEY="$CURSOR_API_KEY" cursor-agent -p --mode ask --trust --resume "$ID" "$Q" \
  | tee "$T/resume.out" || true
# 3. Negative control: a random chat id, same question.
NEG=$(uuidgen | tr 'A-Z' 'a-z')
env "${SANDBOX[@]}" CURSOR_API_KEY="$CURSOR_API_KEY" cursor-agent -p --mode ask --trust --resume "$NEG" "$Q" \
  > "$T/neg.out" 2>&1 || true; cat "$T/neg.out"
grep -q 'PELICAN-4417' "$T/resume.out" && echo "RESUME: PASS" || echo "RESUME: FAIL (history not seen)"
grep -q 'PELICAN-4417' "$T/neg.out" && echo "CONTROL: FAIL (a random id knew it)" || echo "CONTROL: PASS"
echo "evidence: $T"
```
If RESUME fails while CONTROL passes, the store loads but the model is not sent field 1. The next thing to try is the
root's other fields (a native chat's root, read back with `AgentStore::read` on a `--print` chat from the same sandbox,
shows which).

## Paths
```
crates/toolpath-cursor/src/proto.rs
crates/toolpath-cursor/src/agent_store.rs
crates/toolpath-cursor/src/lib.rs
crates/toolpath-cursor/src/project.rs
crates/toolpath-cursor/Cargo.toml
crates/toolpath-cursor/src/.qual
crates/path-cli/src/harness.rs
crates/path-cli/src/cmd_resume.rs
crates/path-cli/src/cmd_share.rs
crates/path-cli/src/projection/mod.rs
crates/path-cli/src/projection/cursor_agent.rs
crates/path-cli/tests/resume.rs
crates/path-cli/tests/support/mod.rs
crates/path-cli/tests/snapshots/help@resume.snap
crates/path-cli/tests/snapshots/help@share.snap
crates/toolpath-goldens/src/model.rs
crates/toolpath-goldens/src/pipeline.rs
crates/toolpath-goldens/src/.qual
crates/toolpath-goldens/tests/observed.rs
crates/toolpath-goldens/tests/writers.rs
crates/toolpath-goldens/tests/transforms_schema.rs
crates/toolpath-goldens/tests/contract.rs
crates/toolpath-goldens/fixtures/contract/cursor-agent.json
glob:"crates/toolpath-goldens/snapshots/*@cursor-agent.snap"
docs/agents/formats/cursor.md
README.md
.qual
CHANGELOG.md
Cargo.lock
```
Shared files, assigned per hunk: the `cursor-agent` keys in `crates/toolpath-goldens/snapshots/{shed,validate,validate_transforms}@*.snap`,
the toolpath-cursor `md5`/`uuid` lines of `Cargo.lock`, the README row, and the CHANGELOG entry and line-996 fix.
