# toolpath-openclaw Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `toolpath-openclaw` provider crate that derives toolpath `Path` documents from OpenClaw agent-session JSONL (forward) and projects/incepts a `Path` back into an OpenClaw on-disk session (reverse), wired into the `path` CLI like the other providers.

**Architecture:** Mirror `toolpath-pi` (the closest structural twin: v3 header + `id`/`parentId` entry tree + `toolCall` blocks + separate `toolResult` messages + dual ISO/epoch timestamps). The forward path is purely native→`ConversationView`; the provider-agnostic `toolpath_convo::derive_path` builds the step DAG and token grouping. OpenClaw's one novel axis — multi-channel human identity — is handled by a small additive change to `toolpath-convo` (`DeriveConfig.user_actor`) plus channel/peer metadata, because an OpenClaw session is single-peer so the human actor is session-level.

**Tech Stack:** Rust (edition 2024), `serde`/`serde_json`, `chrono`, `thiserror`, `anyhow`; deps `toolpath` + `toolpath-convo`. Test helper: `tempfile`.

## Global Constraints

- Rust edition 2024; workspace pinned to 1.94.0. Build clean under `cargo clippy --workspace -- -D warnings`.
- New crate version `0.1.0`. Update all 11 new-crate checklist sites (Phase 9).
- Provider id / `meta.source` string: **`openclaw`**. Path kind: `toolpath::v1::PATH_KIND_AGENT_CODING_SESSION`.
- Format reference of record: `docs/agents/formats/openclaw/` (v3, sourced from `openclaw/openclaw @ 68c533cf`).
- No first-hand OpenClaw install: all fixtures are **synthesized** from the documented v3 format and committed under `crates/toolpath-openclaw/tests/fixtures/` and `test-fixtures/openclaw/`.
- Commits are signed (`git commit -S`); in this background session signing may need the user's 1Password — accumulate and hand off if it fails.
- Actor strings: assistant `agent:<model>` (shared default), human session-level `human:<channel>/<peerId>` for DMs, `human:<channel>/group/<groupId>` for groups, fallback `human:user`.

---

## File Structure

New crate `crates/toolpath-openclaw/`:

| File | Responsibility |
|---|---|
| `Cargo.toml` | Package + deps (mirror pi). |
| `README.md` | Crate doc (wired via `#![doc = include_str!("../README.md")]`). |
| `src/lib.rs` | `OpenClawConvo` manager + re-exports. |
| `src/error.rs` | `OpenClawError` + `Result`. |
| `src/types.rs` | v3 JSONL schema: `SessionHeader`, `Entry` (10 variants), `AgentMessage`, `ContentBlock`, `Usage`, `StopReason`, `EntryBase`. |
| `src/paths.rs` | `PathResolver`: state-dir resolution, agent sessions dir, `sessions.json` index, session-id resolution + routing-key lookup. |
| `src/reader.rs` | `OpenClawSession`, `read_session_from_file`, parentSession chaining, leaf/tree assembly, `main_thread`. |
| `src/io.rs` | Lightweight listing: `list_agents`, `list_sessions`, `first_user_message`, header peek. |
| `src/provider.rs` | `session_to_view`, `classify_tool`, `native_name`, channel/peer parsing, `impl ConversationProvider`. |
| `src/derive.rs` | `derive_path`/`derive_graph` wrappers: compute `user_actor`, inject `meta.extra["openclaw"]`. |
| `src/project.rs` | `OpenClawProjector` (ConversationView→`OpenClawSession`) + `sessions.json` inception entry. |
| `tests/fixtures/*.jsonl` + `tests/*.rs` | Synthesized fixtures + reader/provider/roundtrip integration tests. |

Shared change: `crates/toolpath-convo/src/derive.rs` (+ its tests).

CLI wiring (Phase 8): `crates/path-cli/src/{cmd_import,cmd_list,cmd_show,cmd_share,cmd_resume,cmd_export}.rs` + `crates/path-cli/Cargo.toml`.

Checklist (Phase 9): root `Cargo.toml`, `CLAUDE.md`, `README.md`, `site/_data/crates.json`, `site/pages/crates.md`, `scripts/release.sh`, the `docs/agents/formats/openclaw/` caveats + changelog.

---

## Phase 0: Shared IR — session-level `user_actor`

### Task 0: Add `DeriveConfig.user_actor` and honor it in `actor_for_turn`

**Files:**
- Modify: `crates/toolpath-convo/src/derive.rs` (`DeriveConfig` struct, `Default`, `actor_for_turn` ~476, call site ~118)
- Test: `crates/toolpath-convo/src/derive.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `DeriveConfig.user_actor: Option<String>`; `actor_for_turn(turn, provider, user_actor: Option<&str>) -> String`.

- [ ] **Step 1: Failing test** — in derive.rs tests:

```rust
#[test]
fn user_actor_override_sets_human_actor() {
    let view = view_with(vec![base_turn("t1", Role::User)]);
    let cfg = DeriveConfig { user_actor: Some("human:whatsapp/15555550123".into()), ..Default::default() };
    let path = derive_path(&view, &cfg);
    assert_eq!(path.steps[0].step.actor, "human:whatsapp/15555550123");
}

#[test]
fn user_actor_default_is_human_user() {
    let view = view_with(vec![base_turn("t1", Role::User)]);
    let path = derive_path(&view, &DeriveConfig::default());
    assert_eq!(path.steps[0].step.actor, "human:user");
}
```

- [ ] **Step 2: Run, expect FAIL** (`cargo test -p toolpath-convo user_actor`) — `user_actor` field missing.

- [ ] **Step 3: Implement.** Add to `DeriveConfig` (keep `Default` derive):

```rust
/// Session-level actor string for user turns (e.g. a channel-aware
/// `human:whatsapp/<peerId>`). When `None`, user turns get the default
/// `human:user`. Used by providers (OpenClaw) whose human is a known
/// per-session identity rather than the local shell user.
pub user_actor: Option<String>,
```

Change the call site (~118) from `actor_for_turn(turn, provider)` to `actor_for_turn(turn, provider, config.user_actor.as_deref())` and:

```rust
fn actor_for_turn(turn: &Turn, provider: &str, user_actor: Option<&str>) -> String {
    match &turn.role {
        Role::User => user_actor.map(str::to_string).unwrap_or_else(|| "human:user".to_string()),
        Role::Assistant => {
            let model = turn.model.as_deref().unwrap_or("unknown");
            format!("agent:{}", model)
        }
        Role::System | Role::Other(_) => format!("tool:{}", provider),
    }
}
```

- [ ] **Step 4: Run, expect PASS.** Also run full `cargo test -p toolpath-convo` to confirm no regressions (all existing `DeriveConfig{..}` callers use `..Default::default()` or the struct's Default, so they're unaffected — verify).

- [ ] **Step 5: Bump `toolpath-convo` version** (minor: additive public field). Update `crates/toolpath-convo/Cargo.toml`, root `Cargo.toml` `[workspace.dependencies]`, `site/_data/crates.json`, `CHANGELOG.md`.

- [ ] **Step 6: Commit** `feat(convo): session-level user_actor override in DeriveConfig`.

---

## Phase 1: Crate scaffold + types

### Task 1: Crate skeleton

**Files:** Create `crates/toolpath-openclaw/Cargo.toml`, `src/lib.rs`, `src/error.rs`, `README.md`.

- [ ] **Step 1:** `Cargo.toml` — copy pi's, rename to `toolpath-openclaw`, version `0.1.0`, description "Derive Toolpath provenance documents from OpenClaw agent-session logs", keywords `["openclaw","provenance","toolpath","audit","ai"]`. Deps: `toolpath`, `toolpath-convo`, `anyhow`, `chrono`, `serde`, `serde_json`, `thiserror`; dev-dep `tempfile`.
- [ ] **Step 2:** `src/error.rs` — mirror pi's `PiError` as `OpenClawError` (`Io`, `Json`, `SessionNotFound`, `AgentNotFound`, `InvalidSessionFile`, `MalformedHeader`, `UnsupportedVersion(u32)`, `Convo`, `Other`) + `Result<T>` alias. `thiserror`.
- [ ] **Step 3:** `src/lib.rs` — `#![doc = include_str!("../README.md")]`, `pub mod {error,types,paths,reader,io,provider,derive,project};`, re-exports placeholder (filled later). Minimal `README.md` (expand in Phase 9).
- [ ] **Step 4:** Add `crates/toolpath-openclaw` to root `Cargo.toml` `members` + a `[workspace.dependencies]` entry so it builds. `cargo build -p toolpath-openclaw` (empty modules → may need stub `pub fn` or empty files; create empty module files now).
- [ ] **Step 5: Commit** `feat(openclaw): crate skeleton`.

### Task 2: `types.rs` — v3 schema + serde round-trip

**Files:** Create `crates/toolpath-openclaw/src/types.rs`. Reference: [`docs/agents/formats/openclaw/{jsonl-envelope,entry-types,messages,usage}.md`] and pi `types.rs`.

**Interfaces — Produces:**
- `SessionHeader { type_="session", version:u32, id:String, timestamp:String, cwd:String, parent_session:Option<String>, #[serde(flatten)] extra }`.
- `EntryBase { id:String, parent_id:Option<String>, timestamp:String, append_mode:Option<String> }` (flattened into entries).
- `Entry` enum tagged by `type`: `Message{base, message:AgentMessage}`, `ModelChange{base, provider, model_id}`, `ThinkingLevelChange{base, thinking_level}`, `Compaction{base, summary, first_kept_entry_id, tokens_before, details:Option<SummaryDetails>, from_hook:Option<bool>}`, `BranchSummary{base, from_id, summary, details, from_hook}`, `Custom{base, custom_type, data:Option<Value>}`, `CustomMessage{base, custom_type, content, details, display:bool}`, `Label{base, target_id, label:Option<String>}`, `SessionInfo{base, name:Option<String>}`, `Leaf{base, target_id:Option<String>, append_parent_id:Option<String>}`. Unknown `type` → tolerant `Other` variant (serde `#[serde(other)]` or untagged fallback) so future entry types don't break parsing.
- `AgentMessage` tagged by `role`: `User{content:Content, timestamp:i64}`, `Assistant{content:Vec<ContentBlock>, api, provider, model, response_model:Option, usage:Usage, stop_reason:StopReason, error_*:Option, timestamp:i64}`, `ToolResult{tool_call_id, tool_name, content:Vec<ContentBlock>, details:Option<Value>, is_error:bool, timestamp:i64}`, `BashExecution{...}`.
- `Content` = `enum { Text(String), Blocks(Vec<ContentBlock>) }` (untagged) for the user string-or-array case.
- `ContentBlock` tagged by `type`: `Text{text, text_signature:Option}`, `Thinking{thinking, thinking_signature:Option, redacted:Option<bool>}`, `Image{data, mime_type}`, `ToolCall{id, name, arguments:Value, thought_signature:Option, execution_mode:Option<String>}`.
- `Usage { input:u32, output:u32, cache_read:u32, cache_write:u32, total_tokens:u32, cost:Option<CostBreakdown> }`.
- `StopReason` enum with `Other(String)` fallback for `stop|length|toolUse|error|aborted`.

- [ ] **Step 1: Failing tests** — serde round-trip for header + one of each entry type. Example:

```rust
#[test]
fn header_roundtrip() {
    let j = r#"{"type":"session","version":3,"id":"s1","timestamp":"2026-06-30T12:00:00Z","cwd":"/p"}"#;
    let h: SessionHeader = serde_json::from_str(j).unwrap();
    assert_eq!(h.version, 3);
    assert_eq!(serde_json::from_str::<SessionHeader>(&serde_json::to_string(&h).unwrap()).unwrap().id, "s1");
}

#[test]
fn assistant_message_with_blocks_roundtrip() {
    let j = r#"{"type":"message","id":"e1","parentId":"e0","timestamp":"2026-06-30T12:00:05Z",
      "message":{"role":"assistant","content":[
        {"type":"thinking","thinking":"hm"},
        {"type":"text","text":"hi"},
        {"type":"toolCall","id":"c1","name":"read_file","arguments":{"path":"x"}}],
      "api":"anthropic-messages","provider":"anthropic","model":"claude-x",
      "usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":3},
      "stopReason":"toolUse","timestamp":1751284805000}}"#;
    let e: Entry = serde_json::from_str(j).unwrap();
    match e { Entry::Message{message: AgentMessage::Assistant{content, usage, ..}, ..} => {
        assert_eq!(content.len(), 3); assert_eq!(usage.total_tokens, 3);
    } _ => panic!("wrong variant") }
}

#[test]
fn user_content_string_or_array() {
    let s: AgentMessage = serde_json::from_str(r#"{"role":"user","content":"hi","timestamp":1}"#).unwrap();
    let a: AgentMessage = serde_json::from_str(r#"{"role":"user","content":[{"type":"text","text":"hi"}],"timestamp":1}"#).unwrap();
    // both deserialize; text extraction yields "hi"
}

#[test]
fn leaf_and_compaction_roundtrip() { /* leaf targetId/appendParentId; compaction firstKeptEntryId/tokensBefore/details */ }

#[test]
fn unknown_entry_type_tolerated() {
    let e: Entry = serde_json::from_str(r#"{"type":"future_thing","id":"e","parentId":null,"timestamp":"t"}"#).unwrap();
    matches!(e, Entry::Other{..});
}
```

- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement** `types.rs` with `#[serde(rename_all = "camelCase")]`, `#[serde(tag = "type")]` on `Entry`/`ContentBlock`, `#[serde(tag = "role")]` on `AgentMessage`, `#[serde(untagged)]` on `Content`, `#[serde(other)]` `Other` on `Entry`/`StopReason`. Flatten `EntryBase` into each entry variant. Add `text()`/`thinking()`/`tool_calls()` helper methods on `AgentMessage` (mirror pi).
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** `feat(openclaw): v3 JSONL schema types + serde round-trip`.

---

## Phase 2: paths.rs

### Task 3: State-dir resolution + session-id resolution + routing key

**Files:** Create `crates/toolpath-openclaw/src/paths.rs`. Reference: [`docs/agents/formats/openclaw/directory-layout.md`].

**Interfaces — Produces:**
- `PathResolver { home: Option<PathBuf>, state_dir: PathBuf }` with `new()` (reads `OPENCLAW_STATE_DIR`→existing `~/.openclaw`→existing `~/.clawdbot`→`~/.openclaw`; `~` via `OPENCLAW_HOME`→`HOME`→`USERPROFILE`), `with_state_dir(p)`, `with_home(p)`, `agent_sessions_dir(agent_id:&str) -> PathBuf` (default agent `"main"`), `list_agent_ids() -> io::Result<Vec<String>>`.
- `SessionsIndex` = parsed `sessions.json`: `BTreeMap<String /*sessionKey*/, IndexEntry { session_id, session_file, updated_at, ... }>`; `load(dir) -> Option<SessionsIndex>`, `routing_key_for(session_id) -> Option<(String /*key*/, ParsedKey)>`.
- `ParsedKey { agent_id, channel:Option<String>, peer_kind:Option<String>, peer_id:Option<String>, thread_id:Option<String> }` from `parse_session_key("agent:main:whatsapp:group:123")`.
- `resolve_session_file(agent_id, session_id) -> Result<PathBuf>` (match `sessions.json` sessionId, then filename stems `<id>.jsonl` / `<ISO>_<id>.jsonl` / `<id>-topic-*.jsonl`).
- `DEFAULT_AGENT_ID: &str = "main"`.

- [ ] **Step 1: Failing tests** (temp dirs):

```rust
#[test]
fn resolves_state_dir_from_env() {
    let tmp = tempfile::tempdir().unwrap();
    let r = PathResolver::new_with_env(Some(tmp.path()), None); // test seam: explicit state-dir
    assert_eq!(r.state_dir(), tmp.path());
    assert_eq!(r.agent_sessions_dir("main"), tmp.path().join("agents/main/sessions"));
}

#[test]
fn parses_session_key_dm_and_group() {
    let dm = parse_session_key("agent:main:whatsapp:direct:15555550123");
    assert_eq!(dm.channel.as_deref(), Some("whatsapp"));
    assert_eq!(dm.peer_kind.as_deref(), Some("direct"));
    assert_eq!(dm.peer_id.as_deref(), Some("15555550123"));
    let grp = parse_session_key("agent:main:slack:group:T42");
    assert_eq!(grp.peer_kind.as_deref(), Some("group"));
    assert_eq!(grp.peer_id.as_deref(), Some("T42"));
    let main = parse_session_key("agent:main:main");
    assert!(main.channel.is_none());
}

#[test]
fn resolve_session_file_by_stem_and_index() { /* write a sessions/ dir with <id>.jsonl + sessions.json; resolve both ways */ }
```

- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.** Provide a test seam (`new_with_env` or builder) so tests don't touch real `$HOME`. `parse_session_key`: split on `:`, first seg `agent`, second `agentId`; if 3rd seg is `main`/`direct` handle DM-scope forms; else `channel:peerKind:peerId` with optional trailing `thread:<id>`. Be lenient (return `None` parts on unexpected shapes).
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** `feat(openclaw): path + session-key resolution`.

---

## Phase 3: reader.rs + io.rs

### Task 4: `reader.rs` — parse session, build tree, follow parentSession, main thread

**Files:** Create `crates/toolpath-openclaw/src/reader.rs`. Create fixtures `tests/fixtures/dm_session.jsonl` (header + user + model_change + assistant[thinking,text,toolCall] + toolResult + leaf + compaction) and `tests/fixtures/sessions.json` (key→file). Reference: [`docs/agents/formats/openclaw/walkthrough.md`] (its lines are a ready fixture).

**Interfaces — Produces:**
- `OpenClawSession { header:SessionHeader, entries:Vec<Entry>, file_path:PathBuf, parent:Option<Box<OpenClawSession>>, session_key:Option<String>, parsed_key:Option<ParsedKey> }`.
- `read_session_from_file(&Path) -> Result<OpenClawSession>` (parse header line, reject `version != 3` → `UnsupportedVersion`, skip blank lines, tolerant entry parse).
- `read_session_with_parent(&Path, max_depth) -> Result<OpenClawSession>` (follow `parentSession`).
- `OpenClawSession::main_thread(&self) -> Vec<&Entry>` (resolve visible leaf via last `Leaf.target_id`, else last message entry; walk `parentId → root`, reverse).
- `attach_routing_key(&mut self, resolver)` (lookup `sessions.json` in the file's dir; set `session_key`/`parsed_key`).

- [ ] **Step 1: Failing tests:**

```rust
#[test]
fn reads_dm_fixture() {
    let s = read_session_from_file(Path::new("tests/fixtures/dm_session.jsonl")).unwrap();
    assert_eq!(s.header.version, 3);
    assert!(s.entries.iter().any(|e| matches!(e, Entry::Message{..})));
}
#[test]
fn rejects_non_v3() { /* version:2 fixture → UnsupportedVersion */ }
#[test]
fn main_thread_follows_leaf_to_root() {
    let s = read_session_from_file(Path::new("tests/fixtures/dm_session.jsonl")).unwrap();
    let thread = s.main_thread();
    // first entry is the root user message; tool result present; ordering root→leaf
    assert!(matches!(thread.first(), Some(Entry::Message{..})));
}
```

- [ ] **Step 2: FAIL → Step 3: implement** (mirror pi `reader.rs`; the only real delta is `Leaf`-row-driven head selection in `main_thread` instead of pi's newest-leaf assumption). → **Step 4: PASS.**
- [ ] **Step 5: Commit** `feat(openclaw): session reader + tree assembly`.

### Task 5: `io.rs` — listing + first_user_message

**Files:** Create `crates/toolpath-openclaw/src/io.rs`. Mirror pi `io.rs`.

**Interfaces — Produces:** `list_sessions(resolver, agent_id) -> Result<Vec<SessionMeta>>`, `list_all_sessions(resolver) -> Result<Vec<SessionMeta>>` (across agents), `SessionMeta { id, timestamp, file_path, entry_count, first_user_message, cwd:Option<String>, session_key:Option<String> }`, `extract_first_user_message(&Path)`.

- [ ] **Step 1: Failing tests** — list the fixtures dir; assert one `SessionMeta` with `first_user_message == "Fix the bug in x.ts"` and `cwd` from header. **2: FAIL → 3: implement** (`extract_first_user_message` walks lines for `type=="message"`, `message.role=="user"`, content string-or-array text). **4: PASS. → 5: Commit** `feat(openclaw): session listing + metadata`.

---

## Phase 4: provider.rs (forward)

### Task 6: `session_to_view` + tool classification + channel/peer

**Files:** Create `crates/toolpath-openclaw/src/provider.rs`. Reference: pi `provider.rs`; [`docs/agents/formats/openclaw/{tools,usage,channels-and-actors,lineage}.md`].

**Interfaces — Produces:**
- `session_to_view(&OpenClawSession) -> ConversationView` (two-pass: turns then tool-result correlation; `provider_id = Some("openclaw")`).
- `classify_tool(name:&str) -> Option<ToolCategory>` and `native_name(category, args) -> Option<&'static str>` (OpenClaw tool vocabulary; lenient like pi).
- `pub const PROVIDER_ID: &str = "openclaw";`
- `user_actor_for(parsed: Option<&ParsedKey>) -> Option<String>` (DM → `human:<channel>/<peerId>`; group/channel → `human:<channel>/group/<peerId>`; else `None`).
- `openclaw_meta_extra(session) -> serde_json::Map` (`channel`, `peerKind`, `peerId`, `sessionKey`, `sessionKind`, `agentId`).

Mapping rules (the deltas from pi):
1. `Entry::Message` → `Turn { id: base.id, parent_id: base.parent_id, role, text, thinking, tool_uses, model (assistant), token_usage: Some(usage→TokenUsage) for assistant, timestamp: entry ISO, stop_reason }`. Each assistant message is its own step → set `token_usage` directly (no `group_id`; no `attributed_token_usage`).
2. `toolCall` blocks → `ToolInvocation{ id, name, input: arguments, category: classify_tool(name), result: None }`; record `(turn_idx, tool_idx)` by call id.
3. `Entry::Message{ToolResult}` → pass-2 correlate by `tool_call_id`; set `inv.result = Some(ToolResult{ content: joined text, is_error })`. Don't emit a standalone turn.
4. Write/edit tool calls → push `FileMutation{ path: extract_path(args), tool_id: Some(call_id), operation: Some(op_from_name), raw_diff: None, before: None, after: None, rename_to: None }`. **No raw perspective.**
5. `Entry::Compaction`/`BranchSummary` → synthetic `Role::System` turn carrying `text = summary` (and stash markers — drop if not needed). `ModelChange`/`ThinkingLevelChange`/`Label`/`Leaf`/`Custom` → drop from turns (Leaf only affects ordering).
6. Delegations: OpenClaw sub-agents are cross-session (not inline) → v1 leaves `delegations` empty; lineage captured in meta (Phase 5). Note as a known gap.
7. `ConversationView`: `id = header.id`, `session_ids` = chain, `base = SessionBase{ working_dir: header.cwd, ... }`, `total_usage` summed, `files_changed` from file_mutations.

- [ ] **Step 1: Failing tests** (against `dm_session.jsonl` with `attach_routing_key`):

```rust
#[test]
fn view_has_user_assistant_and_tool_result() {
    let s = load_fixture_with_key();
    let v = session_to_view(&s);
    assert_eq!(v.provider_id.as_deref(), Some("openclaw"));
    let asst = v.turns.iter().find(|t| t.role == Role::Assistant).unwrap();
    assert!(asst.tool_uses[0].result.is_some());          // correlated
    assert_eq!(asst.tool_uses[0].category, Some(ToolCategory::FileRead));
    assert!(asst.token_usage.is_some());
}
#[test]
fn user_actor_from_dm_key() {
    assert_eq!(user_actor_for(Some(&parse_session_key("agent:main:whatsapp:direct:155"))).as_deref(),
               Some("human:whatsapp/155"));
}
#[test]
fn edit_tool_emits_structural_file_mutation_without_raw() {
    // assistant with an edit_file toolCall {path} → FileMutation{path, raw_diff:None}
}
```

- [ ] **Step 2: FAIL → Step 3: implement** (mirror pi two-pass; add channel/file-mutation/usage deltas). → **Step 4: PASS.**
- [ ] **Step 5: Commit** `feat(openclaw): session_to_view + tool/file/usage mapping`.

---

## Phase 5: derive.rs + lib.rs manager

### Task 7: `derive.rs` — config wrappers with channel-aware actor + meta extra

**Files:** Create `crates/toolpath-openclaw/src/derive.rs`. Reference pi `derive.rs`.

**Interfaces — Produces:**
- `DeriveConfig` (re-export `toolpath_convo::DeriveConfig`).
- `derive_path(&OpenClawSession, &DeriveConfig) -> toolpath::v1::Path`: build view, set `cfg.user_actor = user_actor_for(session.parsed_key)`, call `toolpath_convo::derive_path`, then inject `meta.extra["openclaw"] = openclaw_meta_extra(session)` and ensure `meta.source == "openclaw"`, `meta.kind == PATH_KIND_AGENT_CODING_SESSION`.
- `derive_graph(&[OpenClawSession], title, &DeriveConfig) -> Graph`.

- [ ] **Step 1: Failing test:**

```rust
#[test]
fn derive_sets_channel_actor_kind_and_meta() {
    let s = load_fixture_with_key(); // key agent:main:whatsapp:direct:155
    let p = derive_path(&s, &DeriveConfig::default());
    let user_step = p.steps.iter().find(|s| s.step.actor.starts_with("human:")).unwrap();
    assert_eq!(user_step.step.actor, "human:whatsapp/155");
    let meta = p.meta.unwrap();
    assert_eq!(meta.source.as_deref(), Some("openclaw"));
    assert_eq!(meta.kind.as_deref(), Some(PATH_KIND_AGENT_CODING_SESSION));
    assert_eq!(meta.extra["openclaw"]["channel"], "whatsapp");
}
```

- [ ] **2: FAIL → 3: implement → 4: PASS. → 5: Commit** `feat(openclaw): derive_path with channel-aware actor + meta`.

### Task 8: `lib.rs` — `OpenClawConvo` manager + ConversationProvider

**Files:** Modify `crates/toolpath-openclaw/src/lib.rs` + `provider.rs` (`impl ConversationProvider`).

**Interfaces — Produces:** `OpenClawConvo { resolver }` with `new()`, `with_resolver(r)`, `list_agents()`, `list_sessions(agent_id)`, `read_session(agent_id, session_id) -> Result<OpenClawSession>` (attaches routing key), `most_recent_session(agent_id)`, `to_view(&session)`. `impl ConversationProvider for OpenClawConvo` (project param = agentId).

- [ ] **Step 1: Failing test** — `OpenClawConvo::with_resolver(PathResolver::with_state_dir(tmp)).read_session("main", "<id>")` returns the session; `to_view` non-empty. **2: FAIL → 3: implement → 4: PASS → 5: Commit** `feat(openclaw): OpenClawConvo manager + ConversationProvider`.

---

## Phase 6: project.rs (reverse / inception)

### Task 9: `OpenClawProjector` — ConversationView → OpenClawSession + sessions.json

**Files:** Create `crates/toolpath-openclaw/src/project.rs`. Reference: pi `project.rs`, [`docs/agents/adding-a-projector.md`], [`docs/agents/formats/openclaw/{jsonl-envelope,entry-types,messages,tools}.md`].

**Interfaces — Produces:**
- `OpenClawProjector { default_api, default_provider, cwd:Option<String>, agent_id:String, channel:Option<String>, peer_id:Option<String> }` + builders.
- `impl ConversationProjector` → `OpenClawSession` (header from `cwd`/env, entries: per user turn a `Message{User}`; per assistant turn a `Message{Assistant}` with `thinking`+`text`+`toolCall` blocks AND, for each tool result, a separate `Message{ToolResult}` entry; `parentId` chain preserved from `Turn.parent_id`/order; append a final `Leaf` row pointing at the last entry). Drop foreign data; remap tool names via `category`+`native_name(args)` when source name isn't OpenClaw-native.
- `write_session(&OpenClawSession, dir, agent_id) -> Result<PathBuf>` — writes `<sessionId>.jsonl` (0600) **and** upserts a `sessions.json` routing entry `agent:<agentId>:<channel>:direct:<peerId>` (or `agent:<agentId>:main` when no channel), so an OpenClaw instance can route the incepted session.

- [ ] **Step 1: Failing tests** — round-trip contract:

```rust
#[test]
fn roundtrip_preserves_messages_tools_usage() {
    let src = read_session_from_file(Path::new("tests/fixtures/dm_session.jsonl")).unwrap();
    let view = session_to_view(&src);
    let projected = OpenClawProjector::default().project(&view).unwrap();
    // header version 3; user+assistant+toolResult entries present; toolCall id matches toolResult.toolCallId
    assert_eq!(projected.header.version, 3);
    // serialize each entry → reparse via Entry: lossless for text/role/toolCall/usage
}
#[test]
fn inception_writes_session_and_routing_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let view = session_to_view(&read_session_from_file(...).unwrap());
    let proj = OpenClawProjector::default().with_channel("whatsapp").with_peer("155");
    let path = proj.write_session(&proj.project(&view).unwrap(), tmp.path(), "main").unwrap();
    assert!(path.exists());
    let idx: serde_json::Value = serde_json::from_reader(File::open(tmp.path().join("sessions.json")).unwrap()).unwrap();
    assert!(idx.as_object().unwrap().keys().any(|k| k.contains("whatsapp")));
}
```

- [ ] **2: FAIL → 3: implement → 4: PASS → 5: Commit** `feat(openclaw): projector + sessions.json inception`.

### Task 10: Round-trip integration test

**Files:** Create `crates/toolpath-openclaw/tests/projection_roundtrip.rs`. Walk `fixture → to_view → derive_path → serialize+reparse Path → extract_conversation → project → OpenClawSession`; assert field-by-field (messages, roles, content text, toolCall name/args/result/error, usage, channel meta survives on the Path).

- [ ] **1: write test → 2: run (FAIL where gaps) → 3: fix provider/projector → 4: PASS → 5: Commit** `test(openclaw): full projection round-trip`.

---

## Phase 7: CLI — import / list / show

### Task 11: `p import openclaw`

**Files:** Modify `crates/path-cli/Cargo.toml` (add `toolpath-openclaw` dep in the native target deps block), `crates/path-cli/src/cmd_import.rs`. Reference: pi/opencode variants.

**Interfaces — Produces:** `ImportSource::Openclaw { agent:Option<String>, session:Option<String>, all:bool, base:Option<PathBuf> }`; `derive_openclaw(...)`, `derive_openclaw_session(agent, session, base) -> DerivedDoc`, `pick_openclaw(...)`; cache id `make_id("openclaw", &doc_inner_id(&doc))`.

- [ ] **Step 1:** Add enum variant + match arm + `derive_openclaw` (mirror `derive_pi`; "project"→agentId, default `main`; `--all` → Graph). Picker `pick_openclaw` lists via `OpenClawConvo`.
- [ ] **Step 2:** Integration test in `cmd_import.rs::tests` — temp state-dir with a fixture session, `derive_openclaw(Some("main"), Some("<id>"), Some(base))` returns a `Path` with `meta.source=="openclaw"` and full message count.
- [ ] **Step 3: Run → PASS. Step 4: Commit** `feat(cli): path p import openclaw`.

### Task 12: `p list openclaw` + `show openclaw`

**Files:** Modify `crates/path-cli/src/cmd_list.rs`, `cmd_show.rs`.

- [ ] **Step 1:** `ListSource::Openclaw { agent:Option<String>, base:Option<PathBuf> }` + `run_openclaw` (tsv: `agent\tsession_id\trender_row(...,first_user_message)`). `ShowSource::Openclaw { agent:String, session:String, base:Option<PathBuf> }` + `derive_one` arm (mirror pi).
- [ ] **Step 2:** tests: list emits a tsv row; show returns markdown-able Path. **Step 3: PASS → Commit** `feat(cli): path p list/show openclaw`.

---

## Phase 8: CLI — share / resume / export

### Task 13: `share` integration

**Files:** Modify `crates/path-cli/src/cmd_share.rs`.

- [ ] **Step 1:** Add `Openclaw` to `HarnessArg`, `Harness` (`name()="openclaw"`, `symbol()="openclaw"`, `project_keyed()=false` — rank by header cwd, `parse("openclaw")`), `HarnessBundle.openclaw` + `from_environment`, and `collect_openclaw` (list all sessions across agents; `SessionRow{ harness:Openclaw, cwd: Some(header_cwd), session_id, title: first_user_message, matches_cwd }`). Wire into `gather_sessions`.
- [ ] **Step 2:** test: `collect_openclaw` over a temp state-dir yields a row with `matches_cwd` true when header cwd == canonical cwd. **Step 3: PASS → Commit** `feat(cli): openclaw in path share`.

### Task 14: `resume` source recognition + `p export openclaw`

**Files:** Modify `crates/path-cli/src/cmd_resume.rs`, `cmd_export.rs`.

- [ ] **Step 1 (resume):** `infer_source_harness`: add `"openclaw" => Some(Harness::Openclaw)` and actor fallback `agent:openclaw`/`human:` … but **OpenClaw is not an interactive resume target** (no `--resume` CLI). In the resume harness picker, exclude Openclaw from selectable targets (or map it to "incept-only"): document that resuming an openclaw-sourced Path defaults to another harness; OpenClaw inception is via `p export openclaw --project`. Add a guard returning a clear error if a user forces `--harness openclaw` for resume.
- [ ] **Step 2 (export):** `ExportTarget::Openclaw { input, agent:Option<String>, project:Option<PathBuf>, output:Option<PathBuf> }`; `build_openclaw_conversation(path) -> OpenClawSession` shared by three modes: `--project DIR` writes resume-ready layout under `DIR/.openclaw`-style or the real `~/.openclaw` (use `OpenClawProjector::write_session`, reading channel/peer from `path.meta.extra["openclaw"]`); `--output FILE` writes the `.jsonl`; neither → stdout. Mirror the Claude/Gemini three-mode dispatch.
- [ ] **Step 3:** integration test in `cmd_export.rs::tests`: export a fixture Path with `--project tmp`; assert the written `.jsonl` is readable by `OpenClawConvo`/`read_session_from_file` under the same session id, and `sessions.json` got a routing entry. **Step 4: PASS → Commit** `feat(cli): path p export openclaw + resume source recognition`.

---

## Phase 9: New-crate checklist + docs + verification

### Task 15: Workspace + metadata wiring

**Files:** root `Cargo.toml` (members + `[workspace.dependencies]` `toolpath-openclaw = { version="0.1.0", path="crates/toolpath-openclaw" }`), `CLAUDE.md` (repo layout, dep graph, satellite cross-dep line, provider notes, test counts), `README.md` (crate line), `site/_data/crates.json` (full entry), `site/pages/crates.md` (diagram), `scripts/release.sh` (`_all_crates` Tier-2 + dep-order comment), `crates/toolpath-openclaw/README.md` (full), `lib.rs` re-exports.

- [ ] **Step 1:** Make all edits (copy pi/opencode entries as templates). **Step 2:** `cargo build --workspace` + `cargo clippy --workspace -- -D warnings` clean. **Step 3:** `cd site && pnpm run build` (expect page count +0 or as appropriate). **Step 4: Commit** `chore(openclaw): wire crate into workspace + site + release`.

### Task 16: Format-doc upgrade + changelog

**Files:** `docs/agents/formats/openclaw/{README,format-changelog,known-issues}.md`.

- [ ] **Step 1:** Now that a parser exists, upgrade the "no crate yet / source-only" caveats: cite `crates/toolpath-openclaw` as corroboration where the parser confirms a field; add a `format-changelog.md` maintenance note; resolve the "two code layers" reconciliation item with whatever the parser proved. **Step 2: Commit** `docs(openclaw): cross-link the new toolpath-openclaw crate`.

### Task 17: Final verification

- [ ] **Step 1:** `cargo test --workspace` green; record per-crate test counts; update `CLAUDE.md` Testing section with `toolpath-openclaw` counts.
- [ ] **Step 2 (live-ish):** project a real Claude session into OpenClaw format and validate shape:

```bash
cargo run -q -p path-cli -- p import claude --project "$PWD" --session <uuid> --no-cache --pretty > /tmp/src.path.json
cargo run -q -p path-cli -- p export openclaw --input /tmp/src.path.json --output /tmp/out.jsonl
# assert: /tmp/out.jsonl line 1 is a v3 session header; every line re-parses via Entry; tool names remapped
```

- [ ] **Step 3:** `path p validate` any derived doc; confirm `meta.kind`/`source`. **Step 4: Commit** `test(openclaw): workspace test pass + live projection check`.

---

## Self-Review

- **Spec coverage:** forward (Tasks 4–8), reverse/inception (9–10, 14), channel-aware actor (0, 6–7), CLI import/list/show/share/resume/export (11–14), new-crate checklist (15), docs (16), verification (17). ✓
- **Decisions honored:** build both derive+project ✓; channel-aware human actors via `DeriveConfig.user_actor` ✓; inception via projector+sessions.json ✓.
- **Known gaps (documented, not silent):** no `raw` diff perspective (Task 6.4); cross-session sub-agent delegations not inlined in v1 (Task 6.6); OpenClaw not an interactive resume target (Task 14.1).
- **Type consistency:** `session_to_view`/`user_actor_for`/`PROVIDER_ID`/`OpenClawConvo`/`OpenClawProjector`/`PathResolver`/`OpenClawSession` used consistently across tasks.
- **Placeholder scan:** none (mechanical mirrors explicitly point at the pi file + the documented delta).
