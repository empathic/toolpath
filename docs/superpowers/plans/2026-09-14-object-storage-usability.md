# Object Storage Identity, Credentials, Automation, and Record-Store Posture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `p export object`, `p import object`, `path resume <destination>`, `path share --to`, and `path auth s3` serve a solo developer, a security owner, and a platform lead end to end, with identity derived from the document, credentials resolved only for S3, a scriptable surface, and an opt-in record-store posture.

**Architecture:** `crates/path-cli/src/store.rs` keeps owning naming (`ObjectName`), locations (`Destination`, `ObjectUri`), and S3 settings; identity now comes from `graph.id` and the object name carries a reserved `--` before it. Credential resolution moves behind the URL scheme. A new `export_ledger.rs` records every upload locally and lets bulk export skip unchanged documents. `cmd_export`, `cmd_import`, `cmd_list`, `cmd_resume`, `cmd_share`, `cmd_auth`, `remote.rs`, and `share_config.rs` grow the automation surface on top of those primitives.

**Tech Stack:** Rust 2024 edition, pinned toolchain 1.94.0; `object_store` 0.14.1 (`aws` feature); `sha2`/`hex` (already dependencies of `path-cli`); `assert_cmd` + `predicates` integration tests; `clap` derive.

**Spec:** `docs/superpowers/specs/2026-09-14-object-storage-usability-design.md`

## Global Constraints

- All commits must be signed. Signing goes through the 1Password SSH agent; if a commit fails with a 1Password error, stop and wait for the user rather than retrying or bypassing.
- Commit messages describe the change, never the conversation or a quality rating.
- In prose (help text, docs, comments) write "ID", never lowercase "ID", except for literal symbols like `graph.id` or `cache_id`.
- `path-cli` stays at 0.21.0 (unreleased on this branch). `toolpath-convo` bumps 0.11.1 → 0.11.2 and the bump must land in `crates/toolpath-convo/Cargo.toml`, the root `Cargo.toml` `[workspace.dependencies]`, `site/_data/crates.json`, and `CHANGELOG.md`.
- Every `path` binary invocation in an integration test must go through the `cmd(config_dir)` helper in `crates/path-cli/tests/object_storage.rs` (it sandboxes `TOOLPATH_CONFIG_DIR`, `AWS_SHARED_CREDENTIALS_FILE`, `AWS_CONFIG_FILE`, and strips `AWS_*`), or the `cmd()` helper in `crates/path-cli/tests/integration.rs`. Never let a test read the developer's real `~/.aws` or `~/.toolpath`.
- Object metadata attributes are attached only for `s3`/`s3a` URLs; the local backend rejects them.
- Overwrite stays the default put mode. `--no-overwrite` is opt-in.
- Run `cargo fmt --all` before every commit and `cargo clippy --workspace --all-targets -- -D warnings` before the final commit of each task.
- The emscripten build must keep compiling: keep every new object-storage code path inside the existing `#[cfg(not(target_os = "emscripten"))]` blocks or add the same gate.

## File Structure

| File | Responsibility after this plan |
|---|---|
| `crates/toolpath-convo/src/derive.rs` | Path ID is `path-<provider>-<16 chars of session ID>` |
| `crates/path-cli/src/sync/engine.rs` | Removes the superseded cache document when a record's cache ID changes on re-derive |
| `crates/path-cli/src/store.rs` | `ObjectName` (`<date>-<topic>--<id>`, `parse`, `id_of`), `name_for(&Graph)`, `ObjectUri::cache_id` → `object-<id>`, per-scheme `store_options`, `PutSpec` (create-only + metadata), `terse` with cause chain, IMDS explanation, folder permissions, new `S3Settings` fields |
| `crates/path-cli/src/aws_creds.rs` | Unchanged resolver; `Source::Environment` reachable through the env flag |
| `crates/path-cli/src/export_ledger.rs` (new) | `~/.toolpath/exports.json`: destination → cache ID → `{uri, sha256, bytes, uploaded_at, uploader}` |
| `crates/path-cli/src/config.rs` | `EXPORTS_FILE_NAME` constant |
| `crates/path-cli/src/cmd_export.rs` | `ObjectExportArgs` (`--input`/`--all`, `--to`, `--force`, `--dry-run`, `--no-overwrite`, `--include-imported`), `export_body` shared with `share` |
| `crates/path-cli/src/cmd_import.rs` | `p import object <uri-or-destination>`: prefix import with per-object failures |
| `crates/path-cli/src/cmd_list.rs` | `p list object <destination>` in pretty/tsv/json |
| `crates/path-cli/src/cmd_resume.rs` | Help lists object shapes; non-TTY picker error names the lister |
| `crates/path-cli/src/cmd_auth.rs` | `auth s3 status` prints key ID and default region; `auth s3 whoami`; login flags `--no-overwrite`/`--overwrite`/`--sse`/`--kms-key-id`; no `path target` |
| `crates/path-cli/src/remote.rs` | `Remote` enum: Pathbase repo or object destination |
| `crates/path-cli/src/share_config.rs` | `ConfiguredRemote.remote: Remote` |
| `crates/path-cli/src/cmd_share.rs` | `--to <destination>`; dispatches to Pathbase or object export |
| `crates/path-cli/src/query/mod.rs` | Warns when `--source` matches nothing |
| `crates/path-cli/tests/object_storage.rs` | Integration coverage for everything above |
| `crates/path-cli/tests/integration.rs` | `share --to` and config-remote tests |
| `scripts/test-object-storage-live.sh` (new) | MinIO round trip |
| Docs: `README.md`, `crates/path-cli/README.md`, `site/pages/cli.md`, `site/_data/crates.json`, `CLAUDE.md`, `CHANGELOG.md` | Reconciled with the shipped behavior |

---

### Task 1: Widen derived path IDs to 16 characters

**Files:**
- Modify: `crates/toolpath-convo/src/derive.rs:49` and its test near line 1478
- Modify: `crates/toolpath-convo/Cargo.toml:3`
- Modify: `Cargo.toml:28` (workspace dependency `toolpath-convo`)
- Modify: `site/_data/crates.json` (`toolpath-convo` entry, `version`)
- Modify: `CHANGELOG.md` (new section at the top)

**Interfaces:**
- Produces: `derive_path` returns a `Path` whose `path.id` is `path-<provider>-<first 16 chars of view.id>`. Every harness derive's cache ID is `<source>-<path.id>`, so newly derived cache IDs change shape (e.g. `claude-path-claude-code-c0ee4a7d5a064f2d`).

- [ ] **Step 1: Update the failing test**

In `crates/toolpath-convo/src/derive.rs`, the test `test_path_id_default_format` asserts the 8-character form. The fixture `view_with` uses `id: "abcdef012345"` (12 characters), so 16 characters of it is the whole string. Change the assertion:

```rust
    #[test]
    fn test_path_id_default_format() {
        let view = view_with(vec![]);
        let path = derive_path(&view, &DeriveConfig::default());
        assert_eq!(path.path.id, "path-pi-abcdef012345");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p toolpath-convo test_path_id_default_format`
Expected: FAIL with `left: "path-pi-abcdef01"`, `right: "path-pi-abcdef012345"`.

- [ ] **Step 3: Widen the prefix**

In `crates/toolpath-convo/src/derive.rs` line 49:

```rust
    // 16 characters of the session ID: wide enough that a team bucket
    // holding thousands of sessions never sees two collide, short enough
    // to stay legible in a listing.
    let id_prefix: String = view.id.chars().take(16).collect();
```

- [ ] **Step 4: Run the whole workspace test suite**

Run: `cargo test --workspace`
Expected: PASS. The provider crates assert only `starts_with("path-<provider>-")`, so nothing else pins the width. If any snapshot under `crates/path-cli/tests/snapshots/` fails, it renders an example document (fixed IDs, not derived), so a failure there means something else changed; investigate rather than accept.

- [ ] **Step 5: Bump the crate version in all four places**

`crates/toolpath-convo/Cargo.toml`:
```toml
version = "0.11.2"
```

Root `Cargo.toml` line 28:
```toml
toolpath-convo = { version = "0.11.2", path = "crates/toolpath-convo" }
```

`site/_data/crates.json`, the `toolpath-convo` entry:
```json
    "version": "0.11.2",
```

`CHANGELOG.md`, insert directly under the `# Changelog` heading and its intro line, above `## path-cli 0.21.0 — 2026-09-11`:

```markdown
## toolpath-convo 0.11.2 — 2026-09-14

- **`toolpath-convo`** (0.11.2): `derive_path` now builds the default
  path ID from the first 16 characters of the session ID instead of 8
  (`path-claude-code-c0ee4a7d5a064f2d`). Object-storage keys and
  cache IDs are derived from this ID, and 32 bits was thin for a bucket
  shared by a team. Documents already in a cache keep their IDs; the
  next re-derive of a changed session writes the new ID and the sync
  engine removes the superseded document.
```

- [ ] **Step 6: Build and commit**

Run: `cargo build --workspace && cargo fmt --all -- --check`
Expected: builds; fmt clean.

```bash
git add crates/toolpath-convo/src/derive.rs crates/toolpath-convo/Cargo.toml Cargo.toml Cargo.lock site/_data/crates.json CHANGELOG.md
git commit -m "feat(convo): derive path IDs from 16 characters of the session ID"
```

---

### Task 2: Sync removes the superseded cache document when a record's cache ID changes

**Files:**
- Modify: `crates/path-cli/src/sync/engine.rs` (the `Ok(derived) =>` arm around line 264, and the test module)

**Interfaces:**
- Consumes: `SyncRecord.cache_id`, `crate::cache::cache_path`, `crate::cache::write_cached`.
- Produces: no new API; behavior only.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/path-cli/src/sync/engine.rs`, next to `evicted_cache_entry_rematerializes_on_next_sync`:

```rust
    #[test]
    fn a_rederive_that_changes_the_cache_id_removes_the_superseded_doc() {
        with_cfg(|home, config_dir| {
            write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
            let bundle = claude_bundle(home);

            // A record from an older CLI whose derive produced a different
            // cache ID for the same session, with its document still on disk.
            let stale_id = "claude-path-claude-code-stale";
            let doc = toolpath::v1::Graph::from_json(r#"{"graph":{"ID":"g"},"paths":[]}"#).unwrap();
            crate::cache::write_cached(stale_id, &doc, true).unwrap();
            let artifact = crate::artifact::ArtifactRef {
                artifact_type: ArtifactType::Claude,
                ID: "sess-aaa".to_string(),
                path: Some("-test-project".to_string()),
                // No fingerprint: the next sync must treat the source as changed.
                modified: None,
                size: None,
            };
            let config = crate::config::Config {
                toolpath_config_dir: Some(config_dir.to_path_buf()),
                ..Default::default()
            };
            record_artifact(&config, &artifact, stale_id).unwrap();
            assert!(crate::cache::cache_path(stale_id).unwrap().exists());

            sync_bundle(config_dir, &bundle, &[ArtifactType::Claude], None, &mut ()).unwrap();

            let new_id = load_manifest(config_dir).unwrap()["claude"]["sess-aaa"]
                .cache_id
                .clone()
                .unwrap();
            assert_ne!(new_id, stale_id);
            assert!(crate::cache::cache_path(&new_id).unwrap().exists());
            assert!(
                !crate::cache::cache_path(stale_id).unwrap().exists(),
                "the superseded document must be removed"
            );
        });
    }
```

If `Config` cannot be constructed that way in this module, look at how `record_artifact` is called in the existing test `import_records_provenance_that_sync_then_skips` (search for `record_artifact(&config` in the same file) and build the `Config` the same way.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p path-cli a_rederive_that_changes_the_cache_id_removes_the_superseded_doc`
Expected: FAIL on the last assertion (`the superseded document must be removed`).

- [ ] **Step 3: Remove the stale document after the new one is written**

In the `Ok(derived) =>` arm of the sync loop (around `write_cached(&derived.cache_id, &derived.doc, true)?;`):

```rust
            Ok(derived) => {
                // force: sync owns refresh semantics — a re-sync or a
                // prior manual `p import` of the same session must not
                // error on the existing cache entry.
                write_cached(&derived.cache_id, &derived.doc, true)?;
                // A derive whose cache ID differs from the record's (the
                // path-ID width changed, for instance) would otherwise
                // leave the old document behind as an orphan that `path
                // query` keeps seeing.
                if let Some(old) = existing.and_then(|r| r.cache_id.as_deref())
                    && old != derived.cache_id
                    && let Ok(stale) = crate::cache::cache_path(old)
                    && stale.exists()
                {
                    std::fs::remove_file(&stale)
                        .with_context(|| format!("remove superseded {}", stale.display()))?;
                }
                stage(
```

`existing` is the `Option<&SyncRecord>` already bound earlier in the loop body (it is used just above for `memoized_path`). If `Context` is not imported in this file, add `use anyhow::Context;` alongside the existing anyhow imports.

- [ ] **Step 4: Run the sync tests**

Run: `cargo test -p path-cli sync::`
Expected: PASS, including the new test.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/sync/engine.rs
git commit -m "fix(sync): remove the superseded cache document when a re-derive changes the cache ID"
```

---

### Task 3: Object names carry the document ID after a reserved `--`

**Files:**
- Modify: `crates/path-cli/src/store.rs` (`ObjectName`, `name_for`, remove `name_for_body`, naming tests)

**Interfaces:**
- Produces:
  - `ObjectName::new(id: &str, date: Option<&str>, title: Option<&str>) -> ObjectName` — name is `<date>-<topic>--<bounded id>` or `<bounded id>` when there is no date and no topic.
  - `ObjectName::ID_SEPARATOR: &str = "--"`.
  - `ObjectName::id_of(stem: &str) -> &str` — text after the last `--`, else the whole stem.
  - `ObjectName::parse(stem: &str) -> NameParts { date: Option<String>, topic: Option<String>, id: String }`.
  - `name_for(doc: &Graph) -> ObjectName` — takes the ID from `doc.graph.id`; no cache ID parameter.
  - `bounded_id(raw: &str) -> String` (private) — slug of `raw`; if longer than 64 chars, the first 48 (on a dash boundary) plus `-` plus 8 hex of SHA-256 of `raw`.
- `name_for_body` is deleted; callers move to `name_for` on a parsed document (Task 5).

- [ ] **Step 1: Rewrite the naming tests to the new contract**

In the `tests` module of `crates/path-cli/src/store.rs`, replace the four tests `a_name_leads_with_the_date_and_topic`, `a_name_is_stable_as_the_session_grows`, `a_long_prompt_is_truncated_on_a_word_boundary`, `a_prompt_of_pure_punctuation_degrades_to_date_and_id`, `a_synthesized_title_is_not_worth_slugging`, and `an_unparseable_body_still_gets_a_name` with:

```rust
    #[test]
    fn a_name_leads_with_the_date_and_topic_and_ends_with_the_document_id() {
        let doc = doc_with("Add S3 support to share", "2026-08-07T09:15:00Z");
        assert_eq!(
            name_for(&doc).to_string(),
            "2026-08-07-add-s3-support-to-share--g1"
        );
    }

    #[test]
    fn a_name_is_stable_as_the_session_grows() {
        // The date comes from the *earliest* step, so appending turns
        // can't move the object and leave a duplicate behind.
        let short = doc_with("Fix the parser", "2026-08-07T09:15:00Z");
        let name = name_for(&short);

        let mut grown = short.clone();
        if let toolpath::v1::PathOrRef::Path(p) = &mut grown.paths[0] {
            let mut later = p.steps[0].clone();
            later.step.id = "s2".to_string();
            later.step.timestamp = "2026-08-09T18:00:00Z".to_string();
            p.steps.push(later);
        }
        assert_eq!(name_for(&grown), name);
    }

    #[test]
    fn a_long_prompt_is_truncated_on_a_word_boundary() {
        let doc = doc_with(
            "Add support to share and resume to and from S3 and a way to configure credentials",
            "2026-08-07T00:00:00Z",
        );
        let name = name_for(&doc).to_string();
        assert!(
            name.starts_with("2026-08-07-add-support-to-share"),
            "{name}"
        );
        assert!(name.ends_with("--g1"), "{name}");
        // The separator appears exactly once: the slugger collapses dash runs.
        assert_eq!(name.matches("--").count(), 1, "{name}");
    }

    #[test]
    fn a_prompt_of_pure_punctuation_degrades_to_date_and_id() {
        let doc = doc_with("!!! ???", "2026-08-07T00:00:00Z");
        assert_eq!(name_for(&doc).to_string(), "2026-08-07--g1");
    }

    #[test]
    fn a_synthesized_title_is_not_worth_slugging() {
        // `derive_path` writes "claude-code session: abc" when it has
        // nothing better; repeating the ID would waste the legible half
        // of the name.
        let body = serde_json::json!({
            "graph": { "ID": "g1" },
            "paths": [{
                "path": { "ID": "p1", "head": "s1" },
                "meta": { "title": "claude-code session: abc123" },
                "steps": [{
                    "step": { "ID": "s1", "parents": [], "actor": "agent:claude-code",
                              "timestamp": "2026-08-07T00:00:00Z" },
                    "change": { "f": { "structural": { "type": "file.edit" } } }
                }]
            }]
        });
        let doc = toolpath::v1::Graph::from_json(&body.to_string()).unwrap();
        assert_eq!(name_for(&doc).to_string(), "2026-08-07--g1");
    }

    #[test]
    fn a_document_with_no_date_or_topic_is_named_by_its_id_alone() {
        let doc = toolpath::v1::Graph::from_json(r#"{"graph":{"ID":"path-claude-code-abc"},"paths":[]}"#)
            .unwrap();
        assert_eq!(name_for(&doc).to_string(), "path-claude-code-abc");
    }

    #[test]
    fn the_id_is_read_back_from_after_the_last_separator() {
        assert_eq!(
            ObjectName::id_of("2026-08-07-fix-the-parser--path-claude-code-abc"),
            "path-claude-code-abc"
        );
        // Legacy names without a separator: the whole stem is the ID.
        assert_eq!(ObjectName::id_of("2026-08-07-fix-the-parser-doc"), "2026-08-07-fix-the-parser-doc");
        assert_eq!(ObjectName::id_of("path-claude-code-abc"), "path-claude-code-abc");
    }

    #[test]
    fn parse_splits_date_topic_and_id() {
        let p = ObjectName::parse("2026-08-07-fix-the-parser--path-claude-code-abc");
        assert_eq!(p.date.as_deref(), Some("2026-08-07"));
        assert_eq!(p.topic.as_deref(), Some("fix-the-parser"));
        assert_eq!(p.id, "path-claude-code-abc");

        let p = ObjectName::parse("2026-08-07--g1");
        assert_eq!(p.date.as_deref(), Some("2026-08-07"));
        assert_eq!(p.topic, None);
        assert_eq!(p.id, "g1");

        let p = ObjectName::parse("g1");
        assert_eq!((p.date, p.topic, p.id.as_str()), (None, None, "g1"));

        // A topic that happens to start with digits is not a date.
        let p = ObjectName::parse("2026-fixes--g1");
        assert_eq!(p.date, None);
        assert_eq!(p.topic.as_deref(), Some("2026-fixes"));
    }

    #[test]
    fn an_overlong_id_is_bounded_with_a_hash_suffix() {
        let long = "x".repeat(200);
        let name = ObjectName::new(&long, None, None).to_string();
        assert!(name.len() <= 64, "{}", name.len());
        assert!(name.starts_with(&"x".repeat(48)), "{name}");
        // 48 x's, a dash, 8 hex chars.
        assert_eq!(name.len(), 48 + 1 + 8, "{name}");
        // Two different overlong IDs get different names.
        let other = format!("{}y", "x".repeat(199));
        assert_ne!(ObjectName::new(&other, None, None), ObjectName::new(&long, None, None));
    }

    #[test]
    fn an_id_never_contains_the_separator() {
        // slugify collapses dash runs, so `--` in a raw ID can't leak
        // into the name and confuse `id_of`.
        let name = ObjectName::new("weird--id", Some("2026-01-01"), Some("topic")).to_string();
        assert_eq!(name, "2026-01-01-topic--weird-id");
        assert_eq!(ObjectName::id_of(&name), "weird-id");
    }
```

Also update the two existing destination tests that call `ObjectName::bare("claude-abc")`: they keep working (bare names are unchanged), so leave them.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p path-cli store::tests`
Expected: compile errors (`name_for` takes two arguments, `id_of`/`parse` missing).

- [ ] **Step 3: Implement the new naming**

Replace the `ObjectName` impl block, `name_for`, and `name_for_body` in `crates/path-cli/src/store.rs` with:

```rust
/// The three pieces of an object name, recovered from its stem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameParts {
    pub date: Option<String>,
    pub topic: Option<String>,
    pub ID: String,
}

impl ObjectName {
    /// Longest slug we'll put in a name. Long enough to recognize a
    /// session, short enough that the ID stays visible in a
    /// terminal-width listing.
    const SLUG_MAX: usize = 48;
    /// Longest ID we'll put in a name verbatim. Derived IDs are ~40
    /// chars; anything longer is a hand-written document, and a name
    /// must stay under filesystem limits however long that ID is.
    const ID_MAX: usize = 64;
    /// Reserved: the slugger collapses dash runs, so neither the date
    /// nor the topic can contain it, and automation splits on the last
    /// occurrence to get the ID.
    pub(crate) const ID_SEPARATOR: &'static str = "--";

    pub(crate) fn new(ID: &str, date: Option<&str>, title: Option<&str>) -> Self {
        let mut prefix: Vec<String> = Vec::new();
        if let Some(d) = date.map(slugify).filter(|d| !d.is_empty()) {
            prefix.push(d);
        }
        if let Some(t) = title.map(slugify).filter(|t| !t.is_empty()) {
            prefix.push(truncate_slug(&t, Self::SLUG_MAX));
        }
        let ID = bounded_id(ID);
        if prefix.is_empty() {
            ObjectName(ID)
        } else {
            ObjectName(format!("{}{}{ID}", prefix.join("-"), Self::ID_SEPARATOR))
        }
    }

    /// The name for a document with no usable metadata — the ID
    /// alone, which is what the whole scheme degrades to.
    #[cfg(test)]
    pub(crate) fn bare(ID: &str) -> Self {
        Self::new(ID, None, None)
    }

    /// The ID half of a name stem: everything after the last `--`. A
    /// stem with no separator (a name from before the separator
    /// existed, or a bare ID) is taken whole.
    pub(crate) fn id_of(stem: &str) -> &str {
        stem.rsplit_once(Self::ID_SEPARATOR)
            .map(|(_, ID)| ID)
            .unwrap_or(stem)
    }

    /// Split a stem into date, topic, and ID. The date is recognized
    /// only as a leading `YYYY-MM-DD`; everything else before the
    /// separator is the topic.
    pub(crate) fn parse(stem: &str) -> NameParts {
        let (prefix, ID) = match stem.rsplit_once(Self::ID_SEPARATOR) {
            Some((p, ID)) => (Some(p), ID),
            None => (None, stem),
        };
        let mut date = None;
        let mut topic = None;
        if let Some(prefix) = prefix {
            let looks_like_date = prefix.len() >= 10
                && prefix.as_bytes()[..10]
                    .iter()
                    .enumerate()
                    .all(|(i, b)| if i == 4 || i == 7 { *b == b'-' } else { b.is_ascii_digit() })
                && (prefix.len() == 10 || prefix.as_bytes()[10] == b'-');
            if looks_like_date {
                date = Some(prefix[..10].to_string());
                let rest = prefix[10..].trim_start_matches('-');
                if !rest.is_empty() {
                    topic = Some(rest.to_string());
                }
            } else if !prefix.is_empty() {
                topic = Some(prefix.to_string());
            }
        }
        NameParts {
            date,
            topic,
            ID: id.to_string(),
        }
    }
}

/// Slug of an ID, bounded: past `ID_MAX` the slug is cut on a dash
/// boundary at 48 and suffixed with 8 hex characters of the raw ID's
/// SHA-256, so two long IDs that share a prefix still get distinct names.
fn bounded_id(raw: &str) -> String {
    use sha2::Digest;
    let slug = slugify(raw);
    if slug.len() <= ObjectName::ID_MAX {
        return slug;
    }
    let digest = hex::encode(sha2::Sha256::digest(raw.as_bytes()));
    format!("{}-{}", truncate_slug(&slug, 48), &digest[..8])
}
```

Then replace `name_for` and delete `name_for_body`:

```rust
/// Name a document for a destination: date and topic from the document
/// itself, ID from `graph.id`. Nothing about the input path is
/// consulted, so `share` and `p export object` agree, and two different
/// documents that happen to share a filename land on two keys.
pub(crate) fn name_for(doc: &toolpath::v1::Graph) -> ObjectName {
    let path = doc.paths.iter().find_map(|p| match p {
        toolpath::v1::PathOrRef::Path(p) => Some(p.as_ref()),
        toolpath::v1::PathOrRef::Ref(_) => None,
    });
    let Some(path) = path else {
        return ObjectName::new(&doc.graph.id, None, None);
    };

    // Earliest step wins: a session is dated when it started, so the
    // name doesn't move as the conversation grows.
    let date = path
        .steps
        .iter()
        .map(|s| s.step.timestamp.as_str())
        .min()
        .and_then(|ts| ts.split('T').next())
        .map(str::to_string);

    ObjectName::new(&doc.graph.id, date.as_deref(), topic_of(path).as_deref())
}
```

Update the `ObjectName` doc comment above the struct so the example reads `2026-08-07-add-s3-support-to-share--path-claude-code-6f2a1c9e5b3d4a70` and the "Stable" bullet says the ID is `graph.id`, never the input filename.

- [ ] **Step 4: Fix the one remaining caller so the crate compiles**

`cmd_export::run_object` still calls `name_for_body(&body, &cache_id)`. Task 5 rewrites that function; for now make it compile by parsing first:

```rust
        let doc = toolpath::v1::Graph::from_json(&body)
            .map_err(|e| anyhow::anyhow!("{} is not a toolpath document: {e}", file.display()))?;
        let uri = dest.uri_for(&crate::store::name_for(&doc));
```

and delete the `cache_id` computation from `file.file_stem()` in that function.

- [ ] **Step 5: Run the unit tests**

Run: `cargo test -p path-cli store::tests`
Expected: PASS.

Run: `cargo test -p path-cli --test object_storage`
Expected: two failures, `export_then_import_round_trips_through_a_folder` and `re_exporting_a_session_overwrites_its_own_object`, both because the name is now `2026-01-01-hello--g1.json`. Task 5 rewrites those tests; for now update their expected names so the branch stays green: replace `2026-01-01-hello-doc.json` with `2026-01-01-hello--g1.json` in both tests.

Run: `cargo test -p path-cli --test object_storage`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/store.rs crates/path-cli/src/cmd_export.rs crates/path-cli/tests/object_storage.rs
git commit -m "feat(store): name objects <date>-<topic>--<graph id>, never after the input file"
```

---

### Task 4: Imported objects cache under `object-<id>`

**Files:**
- Modify: `crates/path-cli/src/store.rs` (`ObjectUri::cache_id`, test `cache_id_flattens_the_key`)
- Modify: `crates/path-cli/tests/object_storage.rs` (`export_then_import_round_trips_through_a_folder` cache ID assertion)

**Interfaces:**
- Produces: `ObjectUri::cache_id(&self) -> String` returns `object-<id>` where `<id>` is `ObjectName::id_of(stem)`, slugified, cut to at most 100 characters. Pure function of the URI.

- [ ] **Step 1: Replace the unit test**

In `crates/path-cli/src/store.rs` tests, replace `cache_id_flattens_the_key` with:

```rust
    #[test]
    fn cache_id_is_the_document_id_from_the_object_name() {
        let uri = ObjectUri::parse("s3://bkt/traces/2026-01-01-hello--path-claude-code-abc.json").unwrap();
        assert_eq!(uri.cache_id(), "object-path-claude-code-abc");
        // s3a is the same store under a different scheme spelling, so
        // it must not fork the cache; neither must the container.
        let alias = ObjectUri::parse("s3a://other/prefix/2026-01-01-hello--path-claude-code-abc.json").unwrap();
        assert_eq!(alias.cache_id(), uri.cache_id());
        let local = ObjectUri::parse("file:///srv/traces/2026-01-01-hello--path-claude-code-abc.json").unwrap();
        assert_eq!(local.cache_id(), uri.cache_id());
    }

    #[test]
    fn cache_id_of_a_legacy_name_is_the_whole_stem_bounded() {
        let uri = ObjectUri::parse("s3://bkt/traces/2026-01-01-hello-doc.json").unwrap();
        assert_eq!(uri.cache_id(), "object-2026-01-01-hello-doc");

        let long = format!("s3://bkt/{}.json", "k".repeat(300));
        let ID = ObjectUri::parse(&long).unwrap().cache_id();
        assert!(id.len() <= "object-".len() + 100, "{}", id.len());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p path-cli store::tests::cache_id`
Expected: FAIL (`s3-bkt-traces_...` vs `object-...`).

- [ ] **Step 3: Implement**

Replace `ObjectUri::cache_id`:

```rust
    /// The cache ID a download of this object lands at: `object-<id>`,
    /// where the ID is read from the object name (Task: `ObjectName::id_of`).
    /// A function of the URI alone, so a cache hit costs no request; a
    /// function of the *name* rather than the whole URI, so the same
    /// document fetched from two prefixes is one cache entry and a
    /// re-export of it names itself the same way.
    pub(crate) fn cache_id(&self) -> String {
        let stem = self
            .url
            .path()
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_end_matches(".json");
        let ID = slugify(ObjectName::id_of(stem));
        let ID = if id.len() > 100 { truncate_slug(&ID, 100) } else { ID };
        crate::cache::make_id("object", &ID)
    }
```

- [ ] **Step 4: Update the integration test and run**

In `crates/path-cli/tests/object_storage.rs`, `export_then_import_round_trips_through_a_folder`, change the last assertion:

```rust
    assert_eq!(IDs, vec!["object-g1.json".to_string()], "unexpected cache ID: {IDs:?}");
```

Run: `cargo test -p path-cli store::tests --test object_storage`
Expected: PASS. (Run both: `cargo test -p path-cli store::tests` and `cargo test -p path-cli --test object_storage`.)

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/store.rs crates/path-cli/tests/object_storage.rs
git commit -m "feat(store): cache imported objects under object-<document id>"
```

---
### Task 5: Export validates the document before any put and names it from the parsed document

**Files:**
- Modify: `crates/path-cli/src/cmd_export.rs` (`ExportTarget::Object`, `run` dispatch, `run_object`)
- Modify: `crates/path-cli/tests/object_storage.rs` (`write_doc` helper, existing two tests, four new tests)

**Interfaces:**
- Produces:
  - `pub(crate) struct ObjectExportArgs { pub input: String, pub to: String, pub force: bool }` (extended in Task 13).
  - `pub(crate) fn object_name_for(body: &str, source: &std::path::Path, force: bool) -> Result<crate::store::ObjectName>` — parses and schema-validates; on failure errors unless `force`, in which case it warns and names the object after the file stem.
- Consumes: `crate::store::name_for(&Graph)`, `crate::schema::validate(&serde_json::Value)`.

- [ ] **Step 1: Extend the test fixture and write the failing tests**

In `crates/path-cli/tests/object_storage.rs`, replace `write_doc` with a two-function form:

```rust
/// A minimal single-step agent document with graph ID `id`, written to
/// `dir/doc.json`. Two documents with different IDs and the same
/// basename are how collision tests are built.
fn write_doc_with_id(dir: &Path, ID: &str) -> std::path::PathBuf {
    let body = serde_json::json!({
        "graph": { "ID": ID },
        "paths": [{
            "path": { "ID": "p1", "head": "s1" },
            "steps": [{
                "step": {
                    "ID": "s1", "parents": [],
                    "actor": "agent:claude-code",
                    "timestamp": "2026-01-01T00:00:00Z"
                },
                "change": { "claude-code://object-int": { "structural": {
                    "type": "conversation.append", "role": "user", "text": "hello"
                }}}
            }]
        }]
    });
    let p = dir.join("doc.json");
    std::fs::write(&p, serde_json::to_string(&body).unwrap()).unwrap();
    p
}

fn write_doc(dir: &Path) -> std::path::PathBuf {
    write_doc_with_id(dir, "g1")
}

fn folder_names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
```

Then add these tests after `re_exporting_a_session_overwrites_its_own_object`:

```rust
#[test]
fn two_documents_with_the_same_basename_land_on_two_keys() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let doc_a = write_doc_with_id(a.path(), "path-claude-code-aaaa");
    let doc_b = write_doc_with_id(b.path(), "path-claude-code-bbbb");

    for doc in [&doc_a, &doc_b] {
        cmd(config.path())
            .args(["p", "export", "object"])
            .args(["--input", doc.to_str().unwrap()])
            .args(["--to", &folder.path().to_string_lossy()])
            .assert()
            .success();
    }
    assert_eq!(
        folder_names(folder.path()),
        vec![
            "2026-01-01-hello--path-claude-code-aaaa.json".to_string(),
            "2026-01-01-hello--path-claude-code-bbbb.json".to_string(),
        ]
    );
}

#[test]
fn the_same_document_from_a_cache_id_and_a_file_lands_on_one_key() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc_with_id(work.path(), "path-claude-code-aaaa");
    // The same bytes under a cache ID that has nothing to do with the
    // file's basename.
    let documents = config.path().join("documents");
    std::fs::create_dir_all(&documents).unwrap();
    std::fs::copy(&doc, documents.join("claude-path-claude-code-aaaa.json")).unwrap();

    for input in [doc.to_str().unwrap(), "claude-path-claude-code-aaaa"] {
        cmd(config.path())
            .args(["p", "export", "object"])
            .args(["--input", input])
            .args(["--to", &folder.path().to_string_lossy()])
            .assert()
            .success();
    }
    assert_eq!(
        folder_names(folder.path()),
        vec!["2026-01-01-hello--path-claude-code-aaaa.json".to_string()]
    );
}

#[test]
fn a_non_document_is_refused_before_anything_is_written() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let junk = work.path().join("id_rsa");
    std::fs::write(&junk, "PRIVATE KEY MATERIAL\nnot json at all\n").unwrap();

    cmd(config.path())
        .args(["p", "export", "object"])
        .args(["--input", junk.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a toolpath document"));
    assert!(folder_names(folder.path()).is_empty());
}

#[test]
fn a_schema_invalid_document_is_refused_unless_forced() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    // Parses as a Graph, but a step without an actor fails the schema.
    let bad = work.path().join("bad.json");
    std::fs::write(
        &bad,
        r#"{"graph":{"ID":"g-bad"},"paths":[{"path":{"ID":"p","head":"s"},"steps":[{"step":{"ID":"s","timestamp":"2026-01-01T00:00:00Z"},"change":{}}]}]}"#,
    )
    .unwrap();

    cmd(config.path())
        .args(["p", "export", "object"])
        .args(["--input", bad.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a valid toolpath document"));
    assert!(folder_names(folder.path()).is_empty());

    cmd(config.path())
        .args(["p", "export", "object", "--force"])
        .args(["--input", bad.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success()
        .stderr(predicate::str::contains("uploading anyway"));
    assert_eq!(folder_names(folder.path()).len(), 1);
}
```

(The schema at `schema/toolpath.schema.json` lists `actor` among a step's required keys, so the fixture parses as a `Graph` but fails validation.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p path-cli --test object_storage`
Expected: `a_non_document_is_refused_before_anything_is_written` may already pass (Task 3 parses first); `a_schema_invalid_document_is_refused_unless_forced` fails (no `--force` flag; no schema check); the collision test passes already after Task 3. That is fine: the remaining red test drives this task.

- [ ] **Step 3: Add `--force`, the args struct, and the validating namer**

In `crates/path-cli/src/cmd_export.rs`, change the `Object` variant:

```rust
    /// Upload a toolpath document to object storage: an S3 bucket, an
    /// S3-compatible endpoint, or a plain folder.
    ///
    /// S3 credentials come from your `~/.aws` profiles, the AWS
    /// environment, or `path auth s3 login`; a folder needs none. The
    /// object is named `<date>-<topic>--<graph id>.json`, and the
    /// printed location is what `path resume` takes. The object is the
    /// full document: every turn, verbatim diffs, and tool output.
    #[command(alias = "s3")]
    Object(ObjectExportArgs),
```

Add, near `PathbaseExportArgs`:

```rust
#[derive(clap::Args, Debug)]
pub(crate) struct ObjectExportArgs {
    /// Input: cache ID (e.g. `claude-abc`) or path to a toolpath JSON file
    #[arg(short, long)]
    pub input: String,

    /// Destination: `s3://bucket/prefix`, or a folder (`~/traces`,
    /// `file:///srv/traces`).
    #[arg(long, value_name = "DESTINATION")]
    pub to: String,

    /// Upload even if the input does not validate as a toolpath document
    #[arg(long)]
    pub force: bool,
}
```

Change the dispatch arm to `ExportTarget::Object(args) => run_object(args),` and rewrite `run_object`:

```rust
fn run_object(args: ObjectExportArgs) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = args;
        anyhow::bail!("'path p export object' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let file = cache_ref(&args.input)?;
        let body = std::fs::read_to_string(&file)
            .with_context(|| format!("Failed to read {}", file.display()))?;

        let dest = crate::store::Destination::parse(&args.to)?;
        let settings = crate::store::effective_settings()?;
        let name = object_name_for(&body, &file, args.force)?;

        let uri = dest.uri_for(&name);
        uri.put(&settings, body.as_bytes())?;
        println!("{uri}");
        eprintln!("Uploaded {} bytes → {uri}", body.len());
        eprintln!("Resume it with: path resume {uri}");
        Ok(())
    }
}

/// Parse and schema-check the bytes about to be uploaded, and name the
/// object from the parsed document. A body that is not a valid toolpath
/// document is an error — a bucket of "traces" must not quietly collect
/// whatever file was passed — unless `force`, which warns and falls back
/// to naming the object after the file.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn object_name_for(
    body: &str,
    source: &std::path::Path,
    force: bool,
) -> Result<crate::store::ObjectName> {
    let checked = toolpath::v1::Graph::from_json(body)
        .map_err(|e| anyhow::anyhow!("{} is not a toolpath document: {e}", source.display()))
        .and_then(|doc| {
            let value: serde_json::Value = serde_json::from_str(body)?;
            crate::schema::validate(&value).map_err(|e| {
                anyhow::anyhow!("{} is not a valid toolpath document: {e}", source.display())
            })?;
            Ok(doc)
        });
    match checked {
        Ok(doc) => Ok(crate::store::name_for(&doc)),
        Err(e) if force => {
            eprintln!("warning: {e:#}; uploading anyway (--force)");
            let stem = source
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "document".to_string());
            Ok(crate::store::ObjectName::new(&stem, None, None))
        }
        Err(e) => Err(e),
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p path-cli --test object_storage`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/cmd_export.rs crates/path-cli/tests/object_storage.rs
git commit -m "feat(export): validate a document before uploading it and name it from graph.id"
```

---

### Task 6: Credentials resolve only for S3 schemes, and resolution errors propagate

**Files:**
- Modify: `crates/path-cli/src/store.rs` (`S3Settings::resolved_credentials` removed, `store_options`, `open`, `open_with`, `Opened`, callers, unit tests)
- Modify: `crates/path-cli/tests/object_storage.rs` (two new tests with a stub `aws`)

**Interfaces:**
- Produces:
  - `struct Opened { store: Box<dyn ObjectStore>, path: object_store::path::Path, source: Option<crate::aws_creds::Source> }` (private to `store.rs`).
  - `fn store_options(cfg: &S3Settings, scheme: &str) -> Result<(Vec<(&'static str, String)>, Option<crate::aws_creds::Source>)>` — empty options and `None` for non-S3 schemes; otherwise the resolved credentials, region, endpoint, and the `Source` that won.
  - `fn open(url: &Url, cfg: &S3Settings) -> Result<Opened>`.
- Consumes: `S3Settings::resolve_real() -> Result<Resolved>`.
- `S3Settings::resolved_credentials` is deleted.

- [ ] **Step 1: Write the failing unit tests**

Replace `store_options_carry_credentials_and_endpoint` and `https_endpoint_does_not_allow_http` in `store.rs` tests with:

```rust
    #[test]
    fn store_options_carry_credentials_and_endpoint() {
        let (opts, source) = store_options(
            &S3Settings {
                access_key_id: Some("AK".to_string()),
                secret_access_key: Some("SK".to_string()),
                endpoint: Some("http://127.0.0.1:9000".to_string()),
                ..Default::default()
            },
            "s3",
        )
        .unwrap();
        let get = |k: &str| {
            opts.iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("aws_access_key_id"), Some("AK"));
        assert_eq!(get("aws_secret_access_key"), Some("SK"));
        assert_eq!(get("aws_endpoint"), Some("http://127.0.0.1:9000"));
        // Plaintext endpoints have to be opted into explicitly.
        assert_eq!(get("aws_allow_http"), Some("true"));
        assert_eq!(get("aws_region"), Some(DEFAULT_REGION));
        assert_eq!(source, Some(crate::aws_creds::Source::Stored));
    }

    #[test]
    fn https_endpoint_does_not_allow_http() {
        let (opts, _) = store_options(
            &S3Settings {
                endpoint: Some("https://minio.example".to_string()),
                ..Default::default()
            },
            "s3",
        )
        .unwrap();
        assert!(!opts.iter().any(|(k, _)| *k == "aws_allow_http"));
    }

    #[test]
    fn a_folder_never_resolves_credentials() {
        // A stored profile that does not exist would make resolution
        // fail — and must not even be attempted for a folder.
        let cfg = S3Settings {
            profile: Some("definitely-not-a-profile".to_string()),
            ..Default::default()
        };
        let (opts, source) = store_options(&cfg, "file").unwrap();
        assert!(opts.is_empty());
        assert_eq!(source, None);
        let err = store_options(&cfg, "s3").unwrap_err().to_string();
        assert!(err.contains("definitely-not-a-profile"), "{err}");
    }
```

`Source` must derive `PartialEq` (it already does: `#[derive(Debug, Clone, PartialEq, Eq)]`).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p path-cli store::tests`
Expected: compile error (`store_options` takes one argument).

- [ ] **Step 3: Implement**

In `store.rs`, delete `S3Settings::resolved_credentials` and its doc comment. Replace `store_options`, `open`, and `open_with`:

```rust
/// Settings as `object_store` key/value options, plus which credential
/// source won.
///
/// Only an `s3`/`s3a` URL resolves credentials at all. A folder needs
/// none, so for `file` this returns nothing and never touches `~/.aws`
/// or spawns the AWS CLI — which also means a folder export can never
/// trip an SSO login prompt.
///
/// A resolution *error* propagates. The resolver already answers
/// "nothing configured" with the instance chain, so an error here is
/// an explicit failure (a named profile that doesn't exist, an expired
/// SSO session with nobody to ask), and silently falling through to
/// instance metadata would write under whatever principal the machine
/// happens to have.
fn store_options(
    cfg: &S3Settings,
    scheme: &str,
) -> Result<(Vec<(&'static str, String)>, Option<crate::aws_creds::Source>)> {
    fn push(opts: &mut Vec<(&'static str, String)>, k: &'static str, v: &Option<String>) {
        if let Some(v) = v.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            opts.push((k, v.to_string()));
        }
    }

    if !matches!(scheme, "s3" | "s3a") {
        return Ok((Vec::new(), None));
    }

    let mut opts: Vec<(&'static str, String)> = Vec::new();
    let resolved = cfg.resolve_real()?;
    if let Some(c) = &resolved.credentials {
        opts.push(("aws_access_key_id", c.access_key_id.clone()));
        opts.push(("aws_secret_access_key", c.secret_access_key.clone()));
        if let Some(t) = &c.session_token {
            opts.push(("aws_session_token", t.clone()));
        }
    }

    push(&mut opts, "aws_endpoint", &cfg.endpoint);
    let region = cfg
        .region
        .clone()
        .or_else(|| resolved.region.clone())
        .unwrap_or_else(|| DEFAULT_REGION.to_string());
    opts.push(("aws_region", region));

    if let Some(v) = cfg.virtual_hosted_style {
        opts.push(("aws_virtual_hosted_style_request", v.to_string()));
    }
    // A plaintext endpoint is a deliberate choice (MinIO on localhost,
    // a test fixture); object_store refuses http:// unless told.
    if cfg
        .endpoint
        .as_deref()
        .is_some_and(|e| e.starts_with("http://"))
    {
        opts.push(("aws_allow_http", "true".to_string()));
    }
    Ok((opts, Some(resolved.source)))
}

/// An open store plus the path inside it, and which credential source
/// was used (`None` for a folder).
struct Opened {
    store: Box<dyn ObjectStore>,
    path: object_store::path::Path,
    source: Option<crate::aws_creds::Source>,
}

fn open(url: &Url, cfg: &S3Settings) -> Result<Opened> {
    let (opts, source) = store_options(cfg, url.scheme())?;
    let (store, path) =
        object_store::parse_url_opts(url, opts).with_context(|| format!("open {}", friendly(url)))?;
    Ok(Opened {
        store,
        path,
        source,
    })
}
```

Delete `open_with` (its only caller was `open`). Update the three callers:

```rust
    pub(crate) fn get(&self, cfg: &S3Settings) -> Result<String> {
        let opened = open(&self.url, cfg)?;
        let bytes = block_on(async {
            let result = opened.store.get(&opened.path).await?;
            result.bytes().await
        })
        .map_err(|e| explain_location(e, "read", &self.to_string()))?;
        String::from_utf8(bytes.to_vec()).with_context(|| format!("{self} is not valid UTF-8"))
    }

    pub(crate) fn put(&self, cfg: &S3Settings, body: &[u8]) -> Result<()> {
        let opened = open(&self.url, cfg)?;
        let payload = object_store::PutPayload::from(body.to_vec());
        block_on(opened.store.put(&opened.path, payload))
            .map(|_| ())
            .map_err(|e| explain_location(e, "write", &self.to_string()))
    }
```

and in `Destination::list`:

```rust
        let opened = open(&self.base, cfg)?;
        let listed = block_on(opened.store.list_with_delimiter(Some(&opened.path)))
            .map_err(|e| explain_location(e, "list", &friendly(&self.base)))?;
```

The `source` field is unused until Task 8; add `#[allow(dead_code)]` on it for now, and remove that attribute in Task 8.

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p path-cli store::tests`
Expected: PASS.

- [ ] **Step 5: Write the failing integration tests with a stub `aws`**

Add to `crates/path-cli/tests/object_storage.rs`:

```rust
/// A fake `aws` on PATH that logs every invocation to `log` and reports
/// an expired SSO session, plus an `~/.aws/config` declaring an SSO
/// profile so resolution has to go through the CLI.
fn expired_sso_fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.path().join("aws-calls.log");
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> {}\necho 'Error loading SSO Token: Token for https://x.awsapps.com/start does not exist' >&2\nexit 255\n",
        log.display()
    );
    let aws = bin.join("aws");
    std::fs::write(&aws, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&aws, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let config = dir.path().join("aws-config");
    std::fs::write(
        &config,
        "[profile sso-team]\nsso_start_url = https://x.awsapps.com/start\nsso_region = us-east-1\nsso_account_id = 123456789012\nsso_role_name = Dev\nregion = us-east-1\n",
    )
    .unwrap();
    (dir, bin, log)
}

#[test]
fn a_folder_export_never_spawns_the_aws_cli() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());
    let (fixture, bin, log) = expired_sso_fixture();

    cmd(config.path())
        .env("PATH", &bin)
        .env("AWS_CONFIG_FILE", fixture.path().join("aws-config"))
        .env("AWS_PROFILE", "sso-team")
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success();

    assert!(!log.exists(), "the AWS CLI was spawned for a folder export: {:?}", std::fs::read_to_string(&log));
    assert_eq!(folder_names(folder.path()).len(), 1);
}

#[test]
fn an_expired_sso_session_on_s3_fails_with_the_login_command_not_imds() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());
    let (fixture, bin, log) = expired_sso_fixture();

    cmd(config.path())
        .env("PATH", &bin)
        .env("AWS_CONFIG_FILE", fixture.path().join("aws-config"))
        .env("AWS_PROFILE", "sso-team")
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", "s3://audit-bucket/traces"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("aws sso login --profile sso-team"))
        .stderr(predicate::str::contains("169.254.169.254").not());

    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(calls.contains("configure export-credentials"), "{calls}");
    assert!(!calls.contains("sso login"), "no terminal, so no login must be attempted: {calls}");
}
```

- [ ] **Step 6: Run the integration tests**

Run: `cargo test -p path-cli --test object_storage`
Expected: PASS. If `an_expired_sso_session…` fails because the message does not match, check `is_expired_sso` in `aws_creds.rs`: the stub's stderr contains both `sso` and `does not exist`, which it matches; the propagated error text comes from `resolve` via `bail!("the SSO session has expired.\n\nRun `{cmd}`, then try again.")`.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/store.rs crates/path-cli/tests/object_storage.rs
git commit -m "fix(store): resolve credentials only for s3 URLs and propagate resolution errors"
```

---

### Task 7: Credential provenance survives the env merge; `auth s3 status` and `whoami`

**Files:**
- Modify: `crates/path-cli/src/store.rs` (`S3Settings.credentials_from_env`, `merge_env`, `resolve_with`, tests)
- Modify: `crates/path-cli/src/cmd_auth.rs` (`S3Op::Whoami`, `s3_status`, `print_credential_source`, `s3_whoami`)
- Modify: `crates/path-cli/tests/object_storage.rs` (four new tests)

**Interfaces:**
- Produces:
  - `S3Settings { …, #[serde(skip)] pub credentials_from_env: bool }`.
  - `S3Op::Whoami`.
- Consumes: `crate::aws_creds::{Resolved, Source}`, `store::DEFAULT_REGION`.

- [ ] **Step 1: Write the failing unit test for provenance**

In `store.rs` tests:

```rust
    #[test]
    fn env_supplied_keys_resolve_as_the_environment_source() {
        let merged = merge_env(S3Settings::default(), |k| match k {
            "AWS_ACCESS_KEY_ID" => Some("AKIAENV".to_string()),
            "AWS_SECRET_ACCESS_KEY" => Some("SK".to_string()),
            _ => None,
        });
        assert!(merged.credentials_from_env);
        let resolved = merged
            .resolve_with(&crate::aws_creds::Env {
                home: None,
                var: &|_: &str| None,
                aws_cli: &|_: &str| anyhow::bail!("unused"),
                sso_login: &|_: &str| Ok(()),
                confirm: &|_: &str| false,
            })
            .unwrap();
        assert_eq!(resolved.source, crate::aws_creds::Source::Environment);
        assert_eq!(resolved.credentials.unwrap().access_key_id, "AKIAENV");

        // Stored keys stay "stored".
        let stored = S3Settings {
            access_key_id: Some("AKIASTORED".to_string()),
            secret_access_key: Some("SK".to_string()),
            ..Default::default()
        };
        assert!(!merge_env(stored.clone(), |_| None).credentials_from_env);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p path-cli env_supplied_keys_resolve_as_the_environment_source`
Expected: compile error (no field `credentials_from_env`).

- [ ] **Step 3: Implement the flag**

Add the field to `S3Settings` (after `profile`):

```rust
    /// Set by [`merge_env`] when the access key came from
    /// `AWS_ACCESS_KEY_ID` rather than the stored file, so the resolver
    /// can report the source honestly. Never persisted.
    #[serde(skip)]
    pub credentials_from_env: bool,
```

In `merge_env`, replace the two credential lines:

```rust
    if cfg.access_key_id.is_none()
        && let (Some(key), Some(secret)) = (
            first(&["AWS_ACCESS_KEY_ID"]),
            first(&["AWS_SECRET_ACCESS_KEY"]),
        )
    {
        cfg.access_key_id = Some(key);
        cfg.secret_access_key = Some(secret);
        cfg.credentials_from_env = true;
    }
```

(The old code filled the key and the secret independently; requiring both keeps a half-set environment from producing an unusable "stored" pair.)

In `resolve_with`:

```rust
        let mut resolved = crate::aws_creds::resolve(stored, self.profile.as_deref(), env)?;
        if self.credentials_from_env && resolved.source == crate::aws_creds::Source::Stored {
            resolved.source = crate::aws_creds::Source::Environment;
        }
        Ok(resolved)
```

Check `stored_settings_round_trip_through_disk` still passes: `#[serde(skip)]` defaults the field to `false` on load and the stored fixture never set it.

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p path-cli store::tests`
Expected: PASS.

- [ ] **Step 5: Write the failing integration tests for status and whoami**

Add to `crates/path-cli/tests/object_storage.rs`:

```rust
#[test]
fn auth_s3_status_reports_env_keys_as_the_environment() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["auth", "s3", "status"])
        .env("AWS_ACCESS_KEY_ID", "AKIAENVENVENVENV1234")
        .env("AWS_SECRET_ACCESS_KEY", "s3cret")
        .env("AWS_REGION", "us-west-2")
        .assert()
        .success()
        .stdout(predicate::str::contains("AWS_ACCESS_KEY_ID (environment)"))
        .stdout(predicate::str::contains("stored by").not());
}

#[test]
fn auth_s3_status_prints_the_key_id_for_a_profile_and_skips_the_login_advice() {
    let config = tempfile::tempdir().unwrap();
    let aws = tempfile::tempdir().unwrap();
    let creds = aws.path().join("credentials");
    std::fs::write(
        &creds,
        "[default]\naws_access_key_id = AKIAPROFILE\naws_secret_access_key = s3cret\n",
    )
    .unwrap();

    cmd(config.path())
        .args(["auth", "s3", "status"])
        .env("AWS_SHARED_CREDENTIALS_FILE", &creds)
        .assert()
        .success()
        .stdout(predicate::str::contains("access key ID:     AKIAPROFILE"))
        .stdout(predicate::str::contains("region:            us-east-1 (default)"))
        .stdout(predicate::str::contains("Run `path auth s3 login`").not());
}

#[test]
fn auth_s3_status_advises_login_only_when_nothing_resolves() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["auth", "s3", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("EC2/ECS/EKS credential chain"))
        .stdout(predicate::str::contains("Run `path auth s3 login`"));
}

#[test]
fn auth_s3_whoami_runs_sts_with_the_resolved_credentials() {
    let config = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.path().join("env.log");
    let aws = bin.join("aws");
    std::fs::write(
        &aws,
        format!(
            "#!/bin/sh\necho \"$AWS_ACCESS_KEY_ID\" >> {}\nif [ \"$1\" = \"sts\" ]; then echo '{{\"Account\":\"123456789012\",\"Arn\":\"arn:aws:iam::123456789012:user/alex\",\"UserId\":\"AIDAEXAMPLE\"}}'; exit 0; fi\nexit 1\n",
            log.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&aws, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    cmd(config.path())
        .env("PATH", &bin)
        .env("AWS_ACCESS_KEY_ID", "AKIAENVENVENVENV1234")
        .env("AWS_SECRET_ACCESS_KEY", "s3cret")
        .args(["auth", "s3", "whoami"])
        .assert()
        .success()
        .stdout(predicate::str::contains("arn:aws:iam::123456789012:user/alex"))
        .stdout(predicate::str::contains("account:     123456789012"))
        .stdout(predicate::str::contains("credentials: AWS_ACCESS_KEY_ID (environment)"));
    assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "AKIAENVENVENVENV1234");
}

#[test]
fn auth_s3_whoami_without_the_aws_cli_says_so() {
    let config = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    cmd(config.path())
        .env("PATH", empty.path())
        .env("AWS_ACCESS_KEY_ID", "AKIAENVENVENVENV1234")
        .env("AWS_SECRET_ACCESS_KEY", "s3cret")
        .args(["auth", "s3", "whoami"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("`aws` isn't on PATH"));
}
```

- [ ] **Step 6: Run to verify they fail**

Run: `cargo test -p path-cli --test object_storage auth_s3`
Expected: the three new status tests fail on their new assertions; the whoami tests fail with clap "unrecognized subcommand".

- [ ] **Step 7: Implement status and whoami**

In `crates/path-cli/src/cmd_auth.rs`:

Add to `S3Op`, after `Status`:

```rust
    /// Ask STS who the resolved credentials belong to (account, ARN,
    /// user ID). Runs `aws sts get-caller-identity` with the credentials
    /// this CLI would use, so it works for stored keys, environment
    /// keys, and any AWS profile alike.
    Whoami,
```

Also fix the `Login` doc comment: replace the sentence beginning `This does not set *where* shares go — that's `path target`` with:

```rust
    /// This does not set *where* shares go — that's `--to` on
    /// `p export object` and `share`, or a `[[project]]` remote in
    /// `~/.toolpath/config.toml` — so one stored credential serves any
    /// number of buckets.
```

Dispatch: `S3Op::Whoami => s3_whoami(),`.

Replace `s3_status` and `print_credential_source`:

```rust
fn s3_status(path: &Path) -> Result<()> {
    let stored = store::load_stored(path)?;
    let effective = store::effective_settings()?;

    match &stored {
        Some(_) => println!("S3 settings in {}", path.display()),
        None => println!("No stored S3 settings ({} does not exist).", path.display()),
    }
    if effective != S3Settings::default() {
        print_settings(&effective, &stored.unwrap_or_default());
    }
    let resolved = effective.resolve_real();
    print_credential_source(&effective, &resolved);
    // Advice only when it would change anything: someone whose profile
    // already resolves has nothing to store.
    if matches!(&resolved, Ok(r) if r.source == crate::aws_creds::Source::InstanceChain) {
        println!("Run `path auth s3 login` to store credentials, or configure an AWS profile.");
    }
    Ok(())
}

/// Say which credentials a share would actually use, and as which key.
///
/// The first question when an upload fails is *which* credential was
/// tried — a stored key, an AWS profile, or nothing at all are three
/// completely different fixes, and only this line distinguishes them.
/// The key ID is printed for every source (never the secret) so the
/// answer can be matched against IAM.
fn print_credential_source(effective: &S3Settings, resolved: &Result<crate::aws_creds::Resolved>) {
    match resolved {
        Ok(r) => {
            println!("  credentials:       {}", r.source);
            if let Some(c) = &r.credentials
                && effective.access_key_id.is_none()
            {
                println!("  access key ID:     {}", c.access_key_id);
            }
            if effective.region.is_none() {
                match &r.region {
                    Some(region) => println!("  region:            {region} (from the profile)"),
                    None => println!("  region:            {} (default)", store::DEFAULT_REGION),
                }
            }
        }
        // The reason *is* the answer here — "no such profile" tells the
        // user exactly what to fix.
        Err(e) => println!("  credentials:       unresolved — {e:#}"),
    }
}

fn s3_whoami() -> Result<()> {
    let effective = store::effective_settings()?;
    let resolved = effective.resolve_real()?;
    let Some(creds) = &resolved.credentials else {
        anyhow::bail!(
            "no local credentials to identify ({}); on a host with an instance role, run \
             `aws sts get-caller-identity` directly",
            resolved.source
        );
    };
    let region = effective
        .region
        .clone()
        .or_else(|| resolved.region.clone())
        .unwrap_or_else(|| store::DEFAULT_REGION.to_string());

    let mut command = std::process::Command::new("aws");
    command
        .args(["sts", "get-caller-identity", "--output", "json"])
        .env("AWS_ACCESS_KEY_ID", &creds.access_key_id)
        .env("AWS_SECRET_ACCESS_KEY", &creds.secret_access_key)
        .env("AWS_REGION", &region)
        .env_remove("AWS_PROFILE");
    match &creds.session_token {
        Some(t) => {
            command.env("AWS_SESSION_TOKEN", t);
        }
        None => {
            command.env_remove("AWS_SESSION_TOKEN");
        }
    }
    let out = command.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => anyhow!(
            "`aws` isn't on PATH; `path auth s3 whoami` asks STS through the AWS CLI. \
             Install it, or run `aws sts get-caller-identity` wherever it is installed."
        ),
        _ => anyhow!("running `aws sts get-caller-identity`: {e}"),
    })?;
    if !out.status.success() {
        anyhow::bail!(
            "`aws sts get-caller-identity` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .context("the AWS CLI returned output that isn't JSON")?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("?").to_string();
    println!("arn:         {}", field("Arn"));
    println!("account:     {}", field("Account"));
    println!("user ID:     {}", field("UserId"));
    println!("credentials: {}", resolved.source);
    Ok(())
}
```

Add `use anyhow::Context;` to the imports if it is not already there. Widen the label column in `print_settings` so the values line up with the new lines: change `"  {:<19}{v}{origin}"` to keep 19 (the new labels above are padded to the same width by hand: `credentials:       ` and `access key id:     ` are 19 characters wide including the trailing spaces).

- [ ] **Step 8: Run the tests**

Run: `cargo test -p path-cli --test object_storage`
Expected: PASS. If the column assertions fail by whitespace, print the actual stdout and align the literals in the tests to the `{:<19}` width used by `print_settings`.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/store.rs crates/path-cli/src/cmd_auth.rs crates/path-cli/tests/object_storage.rs
git commit -m "feat(auth): s3 status reports the key ID and true source; add s3 whoami"
```

---

### Task 8: Transport errors keep their cause; no-credentials on S3 explains itself

**Files:**
- Modify: `crates/path-cli/src/store.rs` (`terse`, `explain_location`, callers, tests)

**Interfaces:**
- Produces:
  - `fn terse(err: &object_store::Error) -> String` — head of the top message (before `, after `), prefixes stripped, plus `: <innermost cause>` when the cause is not already in the head.
  - `fn explain_location(err: object_store::Error, verb: &str, location: &str, source: Option<&crate::aws_creds::Source>) -> anyhow::Error`.
- Consumes: `Opened.source` from Task 6 (remove the `#[allow(dead_code)]`).

- [ ] **Step 1: Write the failing unit tests**

```rust
    #[test]
    fn terse_keeps_the_innermost_cause() {
        let inner = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused");
        let err = object_store::Error::Generic {
            store: "S3",
            source: Box::new(inner),
        };
        let msg = terse(&err);
        assert!(msg.contains("connection refused"), "{msg}");
        assert!(!msg.starts_with("Generic S3 error"), "{msg}");
    }

    #[test]
    fn terse_strips_the_local_filesystem_prefix() {
        let inner = std::io::Error::other("File name too long (os error 63)");
        let err = object_store::Error::Generic {
            store: "LocalFileSystem",
            source: Box::new(inner),
        };
        let msg = terse(&err);
        assert!(!msg.contains("Generic LocalFileSystem error"), "{msg}");
        assert!(msg.contains("File name too long"), "{msg}");
    }

    #[test]
    fn an_imds_failure_with_no_credentials_explains_where_it_looked() {
        let inner = std::io::Error::other(
            "Error performing PUT http://169.254.169.254/latest/api/token in 1.5s",
        );
        let err = object_store::Error::Generic {
            store: "S3",
            source: Box::new(inner),
        };
        let msg = explain_location(
            err,
            "write",
            "s3://b/k.json",
            Some(&crate::aws_creds::Source::InstanceChain),
        )
        .to_string();
        assert!(msg.contains("no credentials found"), "{msg}");
        assert!(msg.contains("~/.aws"), "{msg}");
        assert!(!msg.contains("169.254.169.254"), "{msg}");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p path-cli store::tests::terse store::tests::an_imds`
Expected: compile error (`explain_location` arity) and, for `terse_keeps_the_innermost_cause`, a failure if compiled alone.

- [ ] **Step 3: Implement**

```rust
/// Strip `object_store`'s internals out of an error message and keep
/// the cause.
///
/// Its transport errors carry a retry epilogue — "after 10 retries,
/// max_retries: 10, retry_timeout: 180s" — plus a `Generic S3 error:`
/// or `Generic LocalFileSystem error:` prefix. Neither tells a user
/// anything actionable. The *innermost* source ("connection refused",
/// "File name too long") is the actionable part and lives at the
/// bottom of the chain, so it is appended when the head doesn't
/// already say it.
fn terse(err: &object_store::Error) -> String {
    let top = err.to_string();
    let head = top
        .split(", after ")
        .next()
        .unwrap_or(&top)
        .trim_start_matches("Generic S3 error: ")
        .trim_start_matches("Generic LocalFileSystem error: ")
        .trim_end_matches([' ', '-'])
        .to_string();

    let mut cause: Option<String> = None;
    let mut cur: &dyn std::error::Error = err;
    while let Some(next) = cur.source() {
        cause = Some(next.to_string());
        cur = next;
    }
    match cause {
        Some(c) if !c.is_empty() && !head.contains(&c) => format!("{head}: {c}"),
        _ => head,
    }
}
```

```rust
/// Turn an `object_store` error into something a user can act on. Its
/// `NotFound` and `Unauthenticated` variants are the two that matter:
/// the first usually means a typo'd key, the second an unconfigured or
/// stale credential. A request that ended up at instance metadata
/// because nothing local resolved gets the real explanation instead of
/// a link-local IP.
fn explain_location(
    err: object_store::Error,
    verb: &str,
    location: &str,
    source: Option<&crate::aws_creds::Source>,
) -> anyhow::Error {
    match err {
        object_store::Error::NotFound { .. } => anyhow!("{location} not found"),
        object_store::Error::Unauthenticated { .. }
        | object_store::Error::PermissionDenied { .. } => {
            anyhow!(
                "not authorized to {verb} {location}. Run `path auth s3 login` to store \
                 credentials, or check the bucket policy for the ones you have."
            )
        }
        e => {
            let msg = terse(&e);
            if matches!(source, Some(crate::aws_creds::Source::InstanceChain))
                && msg.contains("169.254.169.254")
            {
                anyhow!(
                    "failed to {verb} {location}: no credentials found (tried ~/.aws, the \
                     environment, and the EC2/ECS/EKS chain). Run `path auth s3 login` or set \
                     AWS_PROFILE."
                )
            } else {
                anyhow!("failed to {verb} {location}: {msg}")
            }
        }
    }
}
```

Update the three callers to pass `opened.source.as_ref()`:

```rust
        .map_err(|e| explain_location(e, "read", &self.to_string(), opened.source.as_ref()))?;
```
```rust
            .map_err(|e| explain_location(e, "write", &self.to_string(), opened.source.as_ref()))
```
```rust
            .map_err(|e| explain_location(e, "list", &friendly(&self.base), opened.source.as_ref()))?;
```

Remove `#[allow(dead_code)]` from `Opened.source`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p path-cli store::tests && cargo test -p path-cli --test object_storage`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/store.rs
git commit -m "fix(store): keep the innermost cause in transport errors and explain a no-credential IMDS failure"
```

---
### Task 9: `p list object <destination>`

**Files:**
- Modify: `crates/path-cli/src/cmd_list.rs` (`ListSource::Object`, `run` dispatch, `run_object`)
- Modify: `crates/path-cli/tests/object_storage.rs` (three new tests)

**Interfaces:**
- Produces: `ListSource::Object { destination: String }`; `fn run_object(destination: String, fmt: ListFormat) -> Result<()>`.
- Consumes: `crate::store::{Destination, ObjectName, effective_settings}`, `ObjectName::parse`, `sanitize_tsv` (already in `cmd_list.rs`).

- [ ] **Step 1: Write the failing integration tests**

```rust
// ── p list object ───────────────────────────────────────────────────

fn folder_with_two_docs(config: &Path) -> tempfile::TempDir {
    let folder = tempfile::tempdir().unwrap();
    for ID in ["path-claude-code-aaaa", "path-claude-code-bbbb"] {
        let work = tempfile::tempdir().unwrap();
        let doc = write_doc_with_id(work.path(), ID);
        cmd(config)
            .args(["p", "export", "object"])
            .args(["--input", doc.to_str().unwrap()])
            .args(["--to", &folder.path().to_string_lossy()])
            .assert()
            .success();
    }
    folder
}

#[test]
fn list_object_tsv_is_one_line_per_document_with_the_id_first() {
    let config = tempfile::tempdir().unwrap();
    let folder = folder_with_two_docs(config.path());

    let out = cmd(config.path())
        .args(["p", "list", "object", &folder.path().to_string_lossy(), "--format", "tsv"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let mut rows: Vec<Vec<&str>> = stdout.lines().map(|l| l.split('\t').collect()).collect();
    rows.sort();
    assert_eq!(rows.len(), 2, "{stdout}");
    assert_eq!(rows[0][0], "path-claude-code-aaaa");
    assert_eq!(rows[0][1], "2026-01-01");
    assert_eq!(rows[0][2], "hello");
    assert!(rows[0][5].ends_with("2026-01-01-hello--path-claude-code-aaaa.json"), "{stdout}");
}

#[test]
fn list_object_json_carries_the_parsed_name_parts() {
    let config = tempfile::tempdir().unwrap();
    let folder = folder_with_two_docs(config.path());

    let out = cmd(config.path())
        .args(["p", "list", "object", &folder.path().to_string_lossy(), "--format", "json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["source"], "object");
    let objects = v["objects"].as_array().unwrap();
    assert_eq!(objects.len(), 2);
    let IDs: Vec<&str> = objects.iter().map(|o| o["ID"].as_str().unwrap()).collect();
    assert!(ids.contains(&"path-claude-code-aaaa"), "{IDs:?}");
    assert_eq!(objects[0]["date"], "2026-01-01");
    assert_eq!(objects[0]["topic"], "hello");
    assert!(objects[0]["size"].as_u64().unwrap() > 0);
    assert!(objects[0]["uri"].as_str().unwrap().ends_with(".json"));
}

#[test]
fn list_object_on_an_empty_destination_exits_zero() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();

    cmd(config.path())
        .args(["p", "list", "object", &folder.path().to_string_lossy(), "--format", "tsv"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    cmd(config.path())
        .args(["p", "list", "object", &folder.path().to_string_lossy(), "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"objects\": []"));
    cmd(config.path())
        .args(["p", "list", "object", &folder.path().to_string_lossy(), "--format", "pretty"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no documents in"));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p path-cli --test object_storage list_object`
Expected: FAIL with clap "unrecognized subcommand 'object'".

- [ ] **Step 3: Implement**

In `cmd_list.rs`, add to `ListSource` after `Pi`:

```rust
    /// List the documents shared to an object-storage destination: an
    /// `s3://bucket/prefix`, or a folder. Rows are built from object
    /// names alone; nothing is downloaded.
    Object {
        /// Destination: `s3://bucket/prefix`, `~/traces`, `file:///srv/traces`
        #[arg(value_name = "DESTINATION")]
        destination: String,
    },
```

Dispatch: `ListSource::Object { destination } => run_object(destination, fmt),`.

Add the runner (next to `run_git`):

```rust
// ── Object storage ──────────────────────────────────────────────────────────

fn run_object(destination: String, fmt: ListFormat) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (destination, fmt);
        anyhow::bail!("'path p list object' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        use crate::store::{Destination, ObjectName};

        let dest = Destination::parse(&destination)?;
        let settings = crate::store::effective_settings()?;
        let entries = dest.list(&settings)?;

        match fmt {
            ListFormat::Json => {
                let items: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|e| {
                        let parts = ObjectName::parse(&e.stem);
                        serde_json::json!({
                            "ID": parts.id,
                            "date": parts.date,
                            "topic": parts.topic,
                            "name": e.stem,
                            "size": e.size,
                            "modified": e.modified.map(|t| t.to_rfc3339()),
                            "uri": e.uri.to_string(),
                        })
                    })
                    .collect();
                let output = serde_json::json!({
                    "source": "object",
                    "destination": dest.to_string(),
                    "objects": items,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            ListFormat::Tsv => {
                for e in &entries {
                    let parts = ObjectName::parse(&e.stem);
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        sanitize_tsv(&parts.id),
                        sanitize_tsv(parts.date.as_deref().unwrap_or("")),
                        sanitize_tsv(parts.topic.as_deref().unwrap_or("")),
                        e.size,
                        e.modified.map(|t| t.to_rfc3339()).unwrap_or_default(),
                        sanitize_tsv(&e.uri.to_string()),
                    );
                }
            }
            ListFormat::Pretty => {
                if entries.is_empty() {
                    eprintln!("no documents in {dest}");
                    return Ok(());
                }
                println!("Destination: {dest}");
                println!();
                for e in &entries {
                    let when = e
                        .modified
                        .map(|t| t.format("%Y-%m-%d").to_string())
                        .unwrap_or_else(|| "          ".to_string());
                    println!("  {when}  {:>8}  {}", e.size, e.stem);
                }
            }
        }
        Ok(())
    }
}
```

`ObjectEntry.uri`, `.stem`, `.size`, `.modified` are already `pub` on the struct in `store.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p path-cli --test object_storage list_object`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/cmd_list.rs crates/path-cli/tests/object_storage.rs
git commit -m "feat(list): p list object enumerates a destination in pretty, tsv, and json"
```

---

### Task 10: `p import object` accepts a destination and imports everything under it

**Files:**
- Modify: `crates/path-cli/src/cmd_import.rs` (`ImportSource::Object` doc, `run`, `derive`, `derive_object`)
- Modify: `crates/path-cli/tests/object_storage.rs` (two new tests)

**Interfaces:**
- Produces: `fn derive_object(target: String) -> Result<(Vec<DerivedDoc>, usize)>` — documents plus the count of objects that failed. `run` bails after emitting when the count is non-zero.
- Consumes: `crate::store::{Destination, effective_settings}`, `crate::derive::object_fetch_to_doc(&str)`.

- [ ] **Step 1: Write the failing tests**

```rust
// ── p import object <destination> ───────────────────────────────────

#[test]
fn import_object_with_a_destination_imports_every_document_under_it() {
    let config = tempfile::tempdir().unwrap();
    let folder = folder_with_two_docs(config.path());

    cmd(config.path())
        .args(["p", "import", "object", &folder.path().to_string_lossy()])
        .assert()
        .success()
        .stderr(predicate::str::contains("Imported").count(2));

    let mut IDs = folder_names(&config.path().join("documents"));
    ids.sort();
    assert_eq!(
        IDs,
        vec![
            "object-path-claude-code-aaaa.json".to_string(),
            "object-path-claude-code-bbbb.json".to_string()
        ]
    );
}

#[test]
fn import_object_with_a_destination_skips_bad_objects_and_exits_nonzero() {
    let config = tempfile::tempdir().unwrap();
    let folder = folder_with_two_docs(config.path());
    std::fs::write(folder.path().join("garbage.json"), "not json").unwrap();

    cmd(config.path())
        .args(["p", "import", "object", &folder.path().to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("skipping"))
        .stderr(predicate::str::contains("garbage.json"))
        .stderr(predicate::str::contains("1 object(s) could not be imported"));

    assert_eq!(folder_names(&config.path().join("documents")).len(), 2);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p path-cli --test object_storage import_object_with_a_destination`
Expected: FAIL (`names a location but no object key`).

- [ ] **Step 3: Implement**

In `cmd_import.rs`, update the variant doc:

```rust
    /// Import from object storage — an S3 bucket, an S3-compatible
    /// endpoint, or a folder. A full object URL imports one document; a
    /// destination (bucket prefix or folder) imports every `.json`
    /// document directly under it, skipping any that fail and exiting 1
    /// at the end if one did.
    Object {
        /// Object URL (`s3://bucket/key.json`, `file:///dir/key.json`) or
        /// a destination (`s3://bucket/prefix`, `~/traces`)
        #[arg(index = 1)]
        target: String,
    },
```

Change `run`:

```rust
pub fn run(args: ImportArgs, pretty: bool, config: &Config) -> Result<()> {
    let (docs, skipped) = match args.source {
        ImportSource::Object { target } => derive_object(target)?,
        other => (derive(other, config)?, 0),
    };
    emit(&docs, args.force, args.no_cache, pretty, config)?;
    if skipped > 0 {
        anyhow::bail!("{skipped} object(s) could not be imported (see warnings above)");
    }
    Ok(())
}
```

Remove the `ImportSource::Object { target } => derive_object(target),` arm from `derive` and make that match arm `ImportSource::Object { .. } => unreachable!("handled in run")`.

Replace `derive_object`:

```rust
fn derive_object(target: String) -> Result<(Vec<DerivedDoc>, usize)> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = target;
        anyhow::bail!("'path p import object' requires a native environment with network access");
    }
    #[cfg(not(target_os = "emscripten"))]
    {
        // A shared document is always `<name>.json`; anything else names
        // a place to import everything from.
        if target.trim_end_matches('/').ends_with(".json") {
            return Ok((vec![crate::derive::object_fetch_to_doc(&target)?], 0));
        }
        let dest = crate::store::Destination::parse(&target)?;
        let settings = crate::store::effective_settings()?;
        let entries = dest.list(&settings)?;
        if entries.is_empty() {
            anyhow::bail!("no documents in {dest}");
        }
        let mut docs = Vec::with_capacity(entries.len());
        let mut skipped = 0;
        for entry in entries {
            let uri = entry.uri.to_string();
            match crate::derive::object_fetch_to_doc(&uri) {
                Ok(doc) => docs.push(doc),
                Err(e) => {
                    eprintln!("warning: skipping {uri}: {e:#}");
                    skipped += 1;
                }
            }
        }
        if docs.is_empty() {
            anyhow::bail!("{skipped} object(s) failed; nothing imported");
        }
        Ok((docs, skipped))
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p path-cli --test object_storage`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/cmd_import.rs crates/path-cli/tests/object_storage.rs
git commit -m "feat(import): p import object takes a destination and imports every document under it"
```

---

### Task 11: Resume help names object shapes; the non-TTY picker error names the lister

**Files:**
- Modify: `crates/path-cli/src/cmd_resume.rs` (`ResumeArgs` docs, `pick_from_destination`)
- Modify: `crates/path-cli/tests/object_storage.rs` (two new tests)

- [ ] **Step 1: Write the failing tests**

```rust
// ── path resume <destination> without a terminal ────────────────────

#[test]
fn resume_a_destination_without_a_terminal_points_at_the_lister() {
    let config = tempfile::tempdir().unwrap();
    let folder = folder_with_two_docs(config.path());

    cmd(config.path())
        .args(["resume", &folder.path().to_string_lossy(), "--harness", "claude"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("path p list object"))
        .stderr(predicate::str::contains("fzf").not());
}

#[test]
fn resume_help_lists_object_storage_inputs() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["resume", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("s3://"))
        .stdout(predicate::str::contains("folder"));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p path-cli --test object_storage resume_`
Expected: both FAIL.

- [ ] **Step 3: Implement**

In `ResumeArgs`:

```rust
    /// Toolpath document to resume from. Accepted shapes: a Pathbase
    /// URL (`https://host/owner/repo/slug`), a bare Pathbase shorthand
    /// (`owner/repo/slug`), an object in storage (`s3://bucket/key.json`,
    /// `file:///dir/key.json`), a destination to pick from
    /// (`s3://bucket/prefix`, a folder), a path to a local toolpath JSON
    /// file, or a cache ID (e.g. `claude-abc`, `pathbase-foo-bar-baz`).
    pub input: String,
```

```rust
    /// Skip the cache entirely when fetching from Pathbase or object
    /// storage: don't read an existing entry, don't write the fetched
    /// body. Useful for ephemeral environments where you don't want the
    /// cache to grow.
    #[arg(long)]
    pub no_cache: bool,

    /// Force a re-fetch from Pathbase or object storage even if a cache
    /// entry exists, overwriting it with the new bytes. Default behavior
    /// is to use the cached doc on hit and never round-trip.
    #[arg(long)]
    pub force: bool,
```

In `pick_from_destination`, replace the `if !crate::fuzzy::available()` block:

```rust
    if !crate::fuzzy::available() {
        anyhow::bail!(
            "picking one of {} documents needs an interactive terminal; list them with \
             `path p list object {dest}` and pass a full location instead",
            entries.len()
        );
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p path-cli --test object_storage resume_ && cargo test -p path-cli --test resume`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/cmd_resume.rs crates/path-cli/tests/object_storage.rs
git commit -m "fix(resume): document object-storage inputs and point a non-interactive pick at the lister"
```

---

### Task 12: Export ledger

**Files:**
- Create: `crates/path-cli/src/export_ledger.rs`
- Modify: `crates/path-cli/src/config.rs` (add `EXPORTS_FILE_NAME`)
- Modify: `crates/path-cli/src/lib.rs` (a plain `mod export_ledger;` next to `mod store;` at line 50; the file gates itself with `#![cfg(not(target_os = "emscripten"))]`, the same way `aws_creds.rs` does)
- Modify: `crates/path-cli/src/cmd_export.rs` (`run_object` records after a successful put)

**Interfaces:**
- Produces (all `pub(crate)`, in `crate::export_ledger`):
  - `struct ExportRecord { uri: String, sha256: String, bytes: u64, uploaded_at: chrono::DateTime<chrono::Utc>, uploader: String }` (Serialize, Deserialize, Clone, Debug, PartialEq).
  - `type Ledger = BTreeMap<String, BTreeMap<String, ExportRecord>>` — destination → cache ID → record.
  - `fn ledger_path() -> Result<PathBuf>` — `<config dir>/exports.json`.
  - `fn load(path: &Path) -> Result<Ledger>` — missing file is empty.
  - `fn record(path: &Path, destination: &str, cache_id: &str, rec: ExportRecord) -> Result<()>` — load, insert, write atomically (temp + rename), 0600.
  - `fn unchanged(ledger: &Ledger, destination: &str, cache_id: &str, sha256: &str) -> bool`.
  - `fn sha256_hex(body: &[u8]) -> String`.
  - `fn uploader() -> String` — `<USER or LOGNAME or "unknown">@<HOSTNAME env, else output of hostname(1), else "unknown">`.

- [ ] **Step 1: Write the failing unit tests**

Create `crates/path-cli/src/export_ledger.rs` with only the test module first:

```rust
//! What left this machine, to where, when, and as whom.
//!
//! `~/.toolpath/exports.json` maps destination → cache ID → the last
//! upload's URI, SHA-256, size, time, and uploader. Two jobs: bulk
//! export skips a document whose bytes already landed at that
//! destination, and anyone auditing egress from this machine has a
//! local record without asking the bucket.

#![cfg(not(target_os = "emscripten"))]

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(sha: &str) -> ExportRecord {
        ExportRecord {
            uri: "s3://b/2026-01-01-hello--g1.json".to_string(),
            sha256: sha.to_string(),
            bytes: 3,
            uploaded_at: chrono::Utc::now(),
            uploader: "alex@laptop".to_string(),
        }
    }

    #[test]
    fn a_missing_ledger_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("exports.json")).unwrap().is_empty());
    }

    #[test]
    fn records_accumulate_per_destination_and_cache_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exports.json");
        record(&path, "s3://b/traces", "claude-a", rec("aaa")).unwrap();
        record(&path, "s3://b/traces", "claude-b", rec("bbb")).unwrap();
        record(&path, "/srv/traces", "claude-a", rec("ccc")).unwrap();

        let ledger = load(&path).unwrap();
        assert_eq!(ledger["s3://b/traces"]["claude-a"].sha256, "aaa");
        assert_eq!(ledger["s3://b/traces"]["claude-b"].sha256, "bbb");
        assert_eq!(ledger["/srv/traces"]["claude-a"].sha256, "ccc");

        // A re-export replaces the entry.
        record(&path, "s3://b/traces", "claude-a", rec("ddd")).unwrap();
        assert_eq!(load(&path).unwrap()["s3://b/traces"]["claude-a"].sha256, "ddd");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn unchanged_matches_on_destination_cache_id_and_sha() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exports.json");
        record(&path, "s3://b/traces", "claude-a", rec("aaa")).unwrap();
        let ledger = load(&path).unwrap();
        assert!(unchanged(&ledger, "s3://b/traces", "claude-a", "aaa"));
        assert!(!unchanged(&ledger, "s3://b/traces", "claude-a", "zzz"));
        assert!(!unchanged(&ledger, "s3://b/traces", "claude-z", "aaa"));
        assert!(!unchanged(&ledger, "s3://other", "claude-a", "aaa"));
    }

    #[test]
    fn sha256_hex_is_lowercase_hex_of_the_body() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn uploader_has_a_user_and_a_host() {
        let who = uploader();
        assert!(who.contains('@'), "{who}");
        assert!(!who.starts_with('@') && !who.ends_with('@'), "{who}");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Add `mod export_ledger;` to `crates/path-cli/src/lib.rs` next to `mod store;`. Run: `cargo test -p path-cli export_ledger`
Expected: compile errors (nothing defined).

- [ ] **Step 3: Implement**

Add to `crates/path-cli/src/config.rs` after `S3_SETTINGS_FILE_NAME`:

```rust
/// Local record of every object-storage upload (see `export_ledger`).
pub(crate) const EXPORTS_FILE_NAME: &str = "exports.json";
```

Fill in `export_ledger.rs` above the test module:

```rust
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ExportRecord {
    pub uri: String,
    pub sha256: String,
    pub bytes: u64,
    pub uploaded_at: chrono::DateTime<chrono::Utc>,
    pub uploader: String,
}

/// destination → cache ID → last upload. `BTreeMap`s so the file on
/// disk is stably ordered.
pub(crate) type Ledger = BTreeMap<String, BTreeMap<String, ExportRecord>>;

pub(crate) fn ledger_path() -> Result<PathBuf> {
    Ok(crate::config::config_dir()?.join(crate::config::EXPORTS_FILE_NAME))
}

pub(crate) fn load(path: &Path) -> Result<Ledger> {
    Ok(crate::config::read_private_json(path)?.unwrap_or_default())
}

/// Insert one record and write the ledger back. Temp-and-rename so a
/// crash mid-write leaves the previous ledger intact; 0600 because
/// URIs and uploader names are nobody else's business.
pub(crate) fn record(
    path: &Path,
    destination: &str,
    cache_id: &str,
    rec: ExportRecord,
) -> Result<()> {
    let mut ledger = load(path)?;
    ledger
        .entry(destination.to_string())
        .or_default()
        .insert(cache_id.to_string(), rec);

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("ledger path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        crate::config::EXPORTS_FILE_NAME,
        std::process::ID()
    ));
    std::fs::write(&tmp, serde_json::to_string_pretty(&ledger)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

pub(crate) fn unchanged(ledger: &Ledger, destination: &str, cache_id: &str, sha256: &str) -> bool {
    ledger
        .get(destination)
        .and_then(|m| m.get(cache_id))
        .is_some_and(|r| r.sha256 == sha256)
}

pub(crate) fn sha256_hex(body: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(body))
}

/// `<user>@<host>` from the environment, falling back to `hostname(1)`
/// and then to `unknown`. Attribution, not authentication — the bucket's
/// own access log is the authoritative record of the principal.
pub(crate) fn uploader() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let host = std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string());
    format!("{user}@{host}")
}
```

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p path-cli export_ledger`
Expected: PASS.

- [ ] **Step 5: Record every single export**

In `cmd_export::run_object` (the non-emscripten block), after `uri.put(&settings, body.as_bytes())?;`:

```rust
        let ledger_key = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| args.input.clone());
        crate::export_ledger::record(
            &crate::export_ledger::ledger_path()?,
            &dest.to_string(),
            &ledger_key,
            crate::export_ledger::ExportRecord {
                uri: uri.to_string(),
                sha256: crate::export_ledger::sha256_hex(body.as_bytes()),
                bytes: body.len() as u64,
                uploaded_at: chrono::Utc::now(),
                uploader: crate::export_ledger::uploader(),
            },
        )?;
```

Add an integration test to `object_storage.rs`:

```rust
#[test]
fn every_export_is_recorded_in_the_local_ledger() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());

    cmd(config.path())
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success();

    let ledger: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config.path().join("exports.json")).unwrap()).unwrap();
    let dest = folder.path().to_string_lossy().to_string();
    let entry = &ledger[&dest]["doc"];
    assert!(entry["uri"].as_str().unwrap().ends_with("2026-01-01-hello--g1.json"), "{ledger}");
    assert_eq!(entry["sha256"].as_str().unwrap().len(), 64);
    assert!(entry["uploader"].as_str().unwrap().contains('@'));
}
```

If the destination key differs by a trailing slash or a canonicalized `/private` prefix on macOS, compare against `Destination`'s display form by reading it from the export's stdout: the printed URI minus `/2026-01-01-hello--g1.json`.

- [ ] **Step 6: Run and commit**

Run: `cargo test -p path-cli export_ledger --test object_storage`
Expected: PASS. (Run both commands separately if cargo rejects the combination.)

```bash
cargo fmt --all
git add crates/path-cli/src/export_ledger.rs crates/path-cli/src/config.rs crates/path-cli/src/lib.rs crates/path-cli/src/cmd_export.rs crates/path-cli/tests/object_storage.rs
git commit -m "feat(export): record every object upload in a local ledger"
```

---
### Task 13: Bulk export (`--all`), `--dry-run`, and a shared `export_body`

**Files:**
- Modify: `crates/path-cli/src/cmd_export.rs` (`ObjectExportArgs`, `run_object`, new `ExportOptions`, `ObjectOutcome`, `export_body`)
- Modify: `crates/path-cli/src/store.rs` (`Destination::scheme`)
- Modify: `crates/path-cli/tests/object_storage.rs` (three new tests)

**Interfaces:**
- Produces (in `cmd_export`, all `pub(crate)`, non-emscripten):
  - `struct ExportOptions { force: bool, dry_run: bool, no_overwrite: bool }` (Default). `no_overwrite` is wired in Task 14; declare it now so the signature is stable.
  - `enum ObjectOutcome { Uploaded(crate::store::ObjectUri), Unchanged(crate::store::ObjectUri), DryRun(crate::store::ObjectUri) }`.
  - `fn export_body(body: &str, ledger_key: &str, source_label: &std::path::Path, dest: &Destination, settings: &S3Settings, ledger: &Ledger, opts: &ExportOptions) -> Result<ObjectOutcome>` — the whole single-document pipeline: validate and name, ledger skip (only when `ledger` already holds a matching sha), dry run, put, ledger record.
  - `ObjectExportArgs` gains `input: Option<String>`, `all: bool`, `include_imported: bool`, `dry_run: bool`, `no_overwrite: bool`.
- Produces (in `store`): `Destination::scheme(&self) -> &str`.

- [ ] **Step 1: Write the failing tests**

```rust
// ── --all and --dry-run ─────────────────────────────────────────────

fn seed_cache(config: &Path, ID: &str) {
    let documents = config.join("documents");
    std::fs::create_dir_all(&documents).unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc_with_id(work.path(), ID);
    std::fs::copy(&doc, documents.join(format!("{ID}.json"))).unwrap();
}

#[test]
fn export_all_uploads_every_cached_document_except_imports_and_skips_unchanged_on_rerun() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    // Cache IDs are `<source>-<graph id>`; the fixture's graph ID is the
    // cache ID itself, which is what a real derive produces too.
    seed_cache(config.path(), "claude-path-claude-code-aaaa");
    seed_cache(config.path(), "codex-path-codex-bbbb");
    seed_cache(config.path(), "object-path-claude-code-cccc");
    seed_cache(config.path(), "pathbase-alex-pathstash-dddd");

    cmd(config.path())
        .args(["p", "export", "object", "--all"])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success()
        .stderr(predicate::str::contains("2 uploaded, 0 unchanged, 0 failed"));
    assert_eq!(
        folder_names(folder.path()),
        vec![
            "2026-01-01-hello--claude-path-claude-code-aaaa.json".to_string(),
            "2026-01-01-hello--codex-path-codex-bbbb.json".to_string(),
        ]
    );

    cmd(config.path())
        .args(["p", "export", "object", "--all"])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success()
        .stderr(predicate::str::contains("0 uploaded, 2 unchanged, 0 failed"));

    cmd(config.path())
        .args(["p", "export", "object", "--all", "--include-imported"])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success()
        .stderr(predicate::str::contains("2 uploaded, 2 unchanged, 0 failed"));
    assert_eq!(folder_names(folder.path()).len(), 4);
}

#[test]
fn export_all_reports_a_bad_document_and_keeps_going() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    seed_cache(config.path(), "claude-path-claude-code-aaaa");
    let documents = config.path().join("documents");
    std::fs::write(documents.join("claude-broken.json"), "not json").unwrap();

    cmd(config.path())
        .args(["p", "export", "object", "--all"])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("warning: claude-broken"))
        .stderr(predicate::str::contains("1 uploaded, 0 unchanged, 1 failed"));
    assert_eq!(folder_names(folder.path()).len(), 1);
}

#[test]
fn dry_run_prints_the_plan_and_writes_nothing() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());

    cmd(config.path())
        .args(["p", "export", "object", "--dry-run"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("would write"))
        .stderr(predicate::str::contains("2026-01-01-hello--g1.json"))
        .stderr(predicate::str::contains("credentials: none needed (folder)"))
        .stderr(predicate::str::contains("mode:        overwrite"));
    assert!(folder_names(folder.path()).is_empty());
    assert!(!config.path().join("exports.json").exists());
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p path-cli --test object_storage export_all dry_run`
Expected: FAIL with clap errors for the unknown flags.

- [ ] **Step 3: Add `Destination::scheme`**

In `store.rs`, inside `impl Destination`:

```rust
    pub(crate) fn scheme(&self) -> &str {
        self.base.scheme()
    }
```

- [ ] **Step 4: Restructure the export**

Replace `ObjectExportArgs`:

```rust
#[derive(clap::Args, Debug)]
pub(crate) struct ObjectExportArgs {
    /// Input: cache ID (e.g. `claude-abc`) or path to a toolpath JSON file
    #[arg(short, long, required_unless_present = "all", conflicts_with = "all")]
    pub input: Option<String>,

    /// Export every cached document instead of one. Documents that were
    /// themselves imported from object storage or Pathbase are skipped
    /// (see --include-imported), and documents already uploaded to this
    /// destination with the same bytes are skipped using the export
    /// ledger. Failures are reported and tallied, not fatal.
    #[arg(long)]
    pub all: bool,

    /// With --all: also export `object-` and `pathbase-` cache entries
    #[arg(long, requires = "all")]
    pub include_imported: bool,

    /// Destination: `s3://bucket/prefix`, or a folder (`~/traces`,
    /// `file:///srv/traces`).
    #[arg(long, value_name = "DESTINATION")]
    pub to: String,

    /// Upload even if the input does not validate as a toolpath document
    #[arg(long)]
    pub force: bool,

    /// Resolve everything and print what would be written, without
    /// writing: the object location, endpoint, region, credential
    /// source, and put mode.
    #[arg(long)]
    pub dry_run: bool,

    /// Refuse to replace an object that already exists at the computed
    /// key (a create-only put). Default is to overwrite, because a
    /// re-export of a session that grew should replace its own object.
    #[arg(long)]
    pub no_overwrite: bool,
}
```

Replace `run_object` and add the shared pieces:

```rust
/// Per-call knobs for an object export, shared by `p export object` and
/// `path share --to`.
#[cfg(not(target_os = "emscripten"))]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ExportOptions {
    pub force: bool,
    pub dry_run: bool,
    pub no_overwrite: bool,
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) enum ObjectOutcome {
    Uploaded(crate::store::ObjectUri),
    Unchanged(crate::store::ObjectUri),
    DryRun(crate::store::ObjectUri),
}

fn run_object(args: ObjectExportArgs) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = args;
        anyhow::bail!("'path p export object' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let dest = crate::store::Destination::parse(&args.to)?;
        let settings = crate::store::effective_settings()?;
        let opts = ExportOptions {
            force: args.force,
            dry_run: args.dry_run,
            no_overwrite: args.no_overwrite,
        };

        // (ledger key, file) pairs. For a single export the key is the
        // cache ID or the file stem; for --all it is always the cache ID.
        let inputs: Vec<(String, std::path::PathBuf)> = if args.all {
            crate::cache::list_cached()?
                .into_iter()
                .filter(|e| {
                    args.include_imported
                        || !(e.id.starts_with("object-") || e.id.starts_with("pathbase-"))
                })
                .map(|e| (e.id, e.path))
                .collect()
        } else {
            let input = args.input.as_deref().expect("clap: --input or --all");
            let file = cache_ref(input)?;
            let key = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| input.to_string());
            vec![(key, file)]
        };
        if inputs.is_empty() {
            anyhow::bail!("no cached documents to export; run `path p cache sync` first");
        }

        // Only --all consults the ledger to skip: a single explicit
        // export means "ship it", even if the bytes are unchanged.
        let ledger = if args.all {
            crate::export_ledger::load(&crate::export_ledger::ledger_path()?)?
        } else {
            crate::export_ledger::Ledger::new()
        };

        let (mut uploaded, mut unchanged, mut failed) = (0usize, 0usize, 0usize);
        for (key, file) in inputs {
            let body = match std::fs::read_to_string(&file)
                .with_context(|| format!("Failed to read {}", file.display()))
            {
                Ok(b) => b,
                Err(e) if args.all => {
                    eprintln!("warning: {key}: {e:#}");
                    failed += 1;
                    continue;
                }
                Err(e) => return Err(e),
            };
            match export_body(&body, &key, &file, &dest, &settings, &ledger, &opts) {
                Ok(ObjectOutcome::Uploaded(uri)) => {
                    println!("{uri}");
                    uploaded += 1;
                }
                Ok(ObjectOutcome::Unchanged(_)) => unchanged += 1,
                Ok(ObjectOutcome::DryRun(_)) => {}
                Err(e) if args.all => {
                    eprintln!("warning: {key}: {e:#}");
                    failed += 1;
                }
                Err(e) => return Err(e),
            }
        }

        if args.all {
            eprintln!("{uploaded} uploaded, {unchanged} unchanged, {failed} failed → {dest}");
            if failed > 0 {
                anyhow::bail!("{failed} document(s) failed to export");
            }
        }
        Ok(())
    }
}

/// Export one document body to `dest`: validate and name it, skip it if
/// the ledger says these bytes already landed there, honor --dry-run,
/// put, and record the upload. `ledger_key` is how the upload is
/// remembered (the cache ID, or the file stem for a loose file);
/// `source_label` names the input in errors.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn export_body(
    body: &str,
    ledger_key: &str,
    source_label: &std::path::Path,
    dest: &crate::store::Destination,
    settings: &crate::store::S3Settings,
    ledger: &crate::export_ledger::Ledger,
    opts: &ExportOptions,
) -> Result<ObjectOutcome> {
    let name = object_name_for(body, source_label, opts.force)?;
    let uri = dest.uri_for(&name);
    let sha256 = crate::export_ledger::sha256_hex(body.as_bytes());

    if crate::export_ledger::unchanged(ledger, &dest.to_string(), ledger_key, &sha256) {
        eprintln!("Unchanged: {uri}");
        return Ok(ObjectOutcome::Unchanged(uri));
    }

    if opts.dry_run {
        eprintln!("would write {} bytes → {uri}", body.len());
        match dest.scheme() {
            "s3" | "s3a" => {
                let resolved = settings.resolve_real()?;
                eprintln!(
                    "  endpoint:    {}",
                    settings.endpoint.as_deref().unwrap_or("AWS S3")
                );
                let region = settings
                    .region
                    .clone()
                    .or_else(|| resolved.region.clone())
                    .unwrap_or_else(|| crate::store::DEFAULT_REGION.to_string());
                eprintln!("  region:      {region}");
                eprintln!("  credentials: {}", resolved.source);
            }
            _ => eprintln!("  credentials: none needed (folder)"),
        }
        eprintln!(
            "  mode:        {}",
            if opts.no_overwrite { "create-only" } else { "overwrite" }
        );
        return Ok(ObjectOutcome::DryRun(uri));
    }

    uri.put(settings, body.as_bytes())?;
    crate::export_ledger::record(
        &crate::export_ledger::ledger_path()?,
        &dest.to_string(),
        ledger_key,
        crate::export_ledger::ExportRecord {
            uri: uri.to_string(),
            sha256,
            bytes: body.len() as u64,
            uploaded_at: chrono::Utc::now(),
            uploader: crate::export_ledger::uploader(),
        },
    )?;
    eprintln!("Uploaded {} bytes → {uri}", body.len());
    eprintln!("Resume it with: path resume {uri}");
    Ok(ObjectOutcome::Uploaded(uri))
}
```

Delete the ledger-recording block that Task 12 added to `run_object`; it now lives in `export_body`. `opts.no_overwrite` is not yet passed to `put` — Task 14 does that; until then clippy may flag it as unused only if nothing reads it, and the dry-run branch reads it, so it compiles clean.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p path-cli --test object_storage`
Expected: PASS. The `every_export_is_recorded_in_the_local_ledger` test from Task 12 still passes because a single export still records.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/cmd_export.rs crates/path-cli/src/store.rs crates/path-cli/tests/object_storage.rs
git commit -m "feat(export): p export object --all skips unchanged documents; --dry-run prints the plan"
```

---

### Task 14: Opt-in record-store posture: `--no-overwrite`, SSE settings, uploader metadata

**Files:**
- Modify: `crates/path-cli/src/store.rs` (`S3Settings` fields, `store_options`, `PutSpec`, `ObjectUri::put`, tests)
- Modify: `crates/path-cli/src/cmd_auth.rs` (`S3LoginArgs` flags, `s3_login`, `print_settings`)
- Modify: `crates/path-cli/src/cmd_export.rs` (`export_body` builds a `PutSpec`)
- Modify: `crates/path-cli/tests/object_storage.rs`

**Interfaces:**
- Produces:
  - `S3Settings { …, no_overwrite: Option<bool>, server_side_encryption: Option<String>, sse_kms_key_id: Option<String> }` (all `#[serde(default, skip_serializing_if = "Option::is_none")]`).
  - `pub(crate) struct PutSpec { pub create_only: bool, pub metadata: Vec<(&'static str, String)> }` with `Default`.
  - `ObjectUri::put(&self, cfg: &S3Settings, body: &[u8], spec: &PutSpec) -> Result<()>` — `PutMode::Create` when `create_only`; metadata attached only for `s3`/`s3a`; `AlreadyExists` becomes `"<uri> already exists; drop --no-overwrite to replace it"`.
  - `S3LoginArgs { …, no_overwrite: bool, overwrite: bool, sse: Option<String>, kms_key_id: Option<String> }`.

- [ ] **Step 1: Write the failing unit tests**

In `store.rs` tests:

```rust
    #[test]
    fn a_create_only_put_refuses_to_replace_an_existing_object() {
        let dir = tempfile::tempdir().unwrap();
        let dest = Destination::parse(&dir.path().to_string_lossy()).unwrap();
        let cfg = S3Settings::default();
        let uri = dest.uri_for(&ObjectName::bare("claude-abc"));
        let create_only = PutSpec {
            create_only: true,
            ..Default::default()
        };

        uri.put(&cfg, b"{\"v\":1}", &create_only).unwrap();
        let err = uri.put(&cfg, b"{\"v\":2}", &create_only).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert!(err.contains("--no-overwrite"), "{err}");
        assert_eq!(uri.get(&cfg).unwrap(), "{\"v\":1}");

        // The default still overwrites.
        uri.put(&cfg, b"{\"v\":3}", &PutSpec::default()).unwrap();
        assert_eq!(uri.get(&cfg).unwrap(), "{\"v\":3}");
    }

    #[test]
    fn metadata_is_not_sent_to_a_folder() {
        // The local backend rejects attributes outright; a folder export
        // carrying uploader metadata must still succeed.
        let dir = tempfile::tempdir().unwrap();
        let dest = Destination::parse(&dir.path().to_string_lossy()).unwrap();
        let uri = dest.uri_for(&ObjectName::bare("claude-abc"));
        let spec = PutSpec {
            create_only: false,
            metadata: vec![("toolpath-graph-id", "g1".to_string())],
        };
        uri.put(&S3Settings::default(), b"{}", &spec).unwrap();
    }

    #[test]
    fn store_options_carry_server_side_encryption() {
        let (opts, _) = store_options(
            &S3Settings {
                access_key_id: Some("AK".to_string()),
                secret_access_key: Some("SK".to_string()),
                server_side_encryption: Some("aws:kms".to_string()),
                sse_kms_key_id: Some("arn:aws:kms:us-east-1:1:key/k".to_string()),
                ..Default::default()
            },
            "s3",
        )
        .unwrap();
        let get = |k: &str| opts.iter().find(|(key, _)| *key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("aws_server_side_encryption"), Some("aws:kms"));
        assert_eq!(get("aws_sse_kms_key_id"), Some("arn:aws:kms:us-east-1:1:key/k"));
    }
```

Update every existing `uri.put(&cfg, body)` / `.put(&S3Settings::default(), b"{}")` call in the `store.rs` tests to pass `&PutSpec::default()` as the third argument.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p path-cli store::tests`
Expected: compile errors (`PutSpec`, new fields).

- [ ] **Step 3: Implement in `store.rs`**

Add to `S3Settings` after `credentials_from_env`... no: before it (keep the serde-skipped field last):

```rust
    /// Refuse to replace an existing object by default (a create-only
    /// put). `--no-overwrite` on a command does the same per call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_overwrite: Option<bool>,
    /// `AES256` or `aws:kms`; passed through as `x-amz-server-side-encryption`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_side_encryption: Option<String>,
    /// KMS key for `aws:kms` encryption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sse_kms_key_id: Option<String>,
```

In `store_options`, after the `aws_region` push:

```rust
    push(&mut opts, "aws_server_side_encryption", &cfg.server_side_encryption);
    push(&mut opts, "aws_sse_kms_key_id", &cfg.sse_kms_key_id);
```

Add `PutSpec` near `ObjectUri`:

```rust
/// How an object is written: overwrite (default) or create-only, and
/// what metadata to attach. Metadata reaches S3 as `x-amz-meta-*`
/// headers; the local backend rejects attributes, so it is dropped for
/// folders rather than failing the write.
#[derive(Debug, Clone, Default)]
pub(crate) struct PutSpec {
    pub create_only: bool,
    pub metadata: Vec<(&'static str, String)>,
}
```

Replace `ObjectUri::put`:

```rust
    /// Upload `body` to the object. Overwrite by default: the object name
    /// is a pure function of the document, so re-sharing a session that
    /// has grown replaces its own object rather than accumulating
    /// near-duplicates. `spec.create_only` turns an existing object into
    /// an error instead, for destinations that are a record.
    pub(crate) fn put(&self, cfg: &S3Settings, body: &[u8], spec: &PutSpec) -> Result<()> {
        let opened = open(&self.url, cfg)?;
        let mut options = object_store::PutOptions::default();
        if spec.create_only {
            options.mode = object_store::PutMode::Create;
        }
        if matches!(self.url.scheme(), "s3" | "s3a") {
            let mut attributes = object_store::Attributes::new();
            for (key, value) in &spec.metadata {
                attributes.insert(
                    object_store::Attribute::Metadata((*key).into()),
                    value.clone().into(),
                );
            }
            options.attributes = attributes;
        }
        let payload = object_store::PutPayload::from(body.to_vec());
        match block_on(opened.store.put_opts(&opened.path, payload, options)) {
            Ok(_) => Ok(()),
            Err(object_store::Error::AlreadyExists { .. }) => {
                bail!("{self} already exists; drop --no-overwrite to replace it")
            }
            Err(e) => Err(explain_location(e, "write", &self.to_string(), opened.source.as_ref())),
        }
    }
```

If `value.clone().into()` does not satisfy `AttributeValue`, use `object_store::AttributeValue::from(value.clone())`.

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p path-cli store::tests`
Expected: PASS.

- [ ] **Step 5: Wire the export and the login flags**

In `cmd_export::export_body`, replace `uri.put(settings, body.as_bytes())?;` with:

```rust
    let graph_id = crate::store::ObjectName::id_of(&name.to_string()).to_string();
    let spec = crate::store::PutSpec {
        create_only: opts.no_overwrite || settings.no_overwrite.unwrap_or(false),
        metadata: vec![
            ("toolpath-graph-id", graph_id),
            ("toolpath-sha256", sha256.clone()),
            ("toolpath-uploader", crate::export_ledger::uploader()),
            ("toolpath-cli-version", env!("CARGO_PKG_VERSION").to_string()),
            ("toolpath-uploaded-at", chrono::Utc::now().to_rfc3339()),
        ],
    };
    uri.put(settings, body.as_bytes(), &spec)?;
```

and make the dry-run `mode:` line use the same expression: `if opts.no_overwrite || settings.no_overwrite.unwrap_or(false) { "create-only" } else { "overwrite" }`.

In `cmd_auth.rs`, add to `S3LoginArgs`:

```rust
    /// Refuse to replace existing objects on every export (create-only
    /// puts). Pass --no-overwrite on a single export for a one-off.
    #[arg(long, conflicts_with = "overwrite")]
    pub no_overwrite: bool,

    /// Clear a stored --no-overwrite
    #[arg(long)]
    pub overwrite: bool,

    /// Server-side encryption for uploads: `AES256` or `aws:kms`
    #[arg(long, value_name = "ALGORITHM")]
    pub sse: Option<String>,

    /// KMS key ID or ARN for `--sse aws:kms`
    #[arg(long, value_name = "KEY", requires = "sse")]
    pub kms_key_id: Option<String>,
```

Also give `--access-key-id` its missing help: `/// Access key ID (stored in ~/.toolpath/s3.json, 0600)`.

In `s3_login`, after the `virtual_hosted_style` handling:

```rust
    if args.no_overwrite {
        cfg.no_overwrite = Some(true);
    }
    if args.overwrite {
        cfg.no_overwrite = None;
    }
    set(&mut cfg.server_side_encryption, args.sse);
    set(&mut cfg.sse_kms_key_id, args.kms_key_id);
```

In `print_settings`, after the `virtual_hosted_style` line (or at the end of the function):

```rust
    line(
        "encryption",
        effective.server_side_encryption.as_deref(),
        stored.server_side_encryption.is_some(),
    );
    line(
        "kms key ID",
        effective.sse_kms_key_id.as_deref(),
        stored.sse_kms_key_id.is_some(),
    );
    if effective.no_overwrite == Some(true) {
        println!("  {:<19}create-only (existing objects are never replaced)", "put mode:");
    }
```

- [ ] **Step 6: Write the failing integration tests and run**

```rust
// ── --no-overwrite and record-store settings ────────────────────────

#[test]
fn no_overwrite_refuses_the_second_export_of_the_same_document() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());

    cmd(config.path())
        .args(["p", "export", "object", "--no-overwrite"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success();
    cmd(config.path())
        .args(["p", "export", "object", "--no-overwrite"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"))
        .stderr(predicate::str::contains("--no-overwrite"));
    // Without the flag the default overwrite still applies.
    cmd(config.path())
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success();
}

#[test]
fn a_stored_no_overwrite_applies_to_every_export() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());

    cmd(config.path())
        .args(["auth", "s3", "login", "--no-overwrite", "--sse", "aws:kms", "--kms-key-id", "alias/traces"])
        .assert()
        .success()
        .stdout(predicate::str::contains("create-only"))
        .stdout(predicate::str::contains("aws:kms"))
        .stdout(predicate::str::contains("alias/traces"));

    for expect_ok in [true, false] {
        let assert = cmd(config.path())
            .args(["p", "export", "object"])
            .args(["--input", doc.to_str().unwrap()])
            .args(["--to", &folder.path().to_string_lossy()])
            .assert();
        if expect_ok {
            assert.success();
        } else {
            assert.failure().stderr(predicate::str::contains("already exists"));
        }
    }

    cmd(config.path())
        .args(["auth", "s3", "login", "--overwrite"])
        .assert()
        .success()
        .stdout(predicate::str::contains("create-only").not());
}
```

Run: `cargo test -p path-cli --test object_storage`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/store.rs crates/path-cli/src/cmd_auth.rs crates/path-cli/src/cmd_export.rs crates/path-cli/tests/object_storage.rs
git commit -m "feat(store): opt-in create-only puts, SSE settings, and uploader metadata on S3 objects"
```

---

### Task 15: Folder objects are 0600 and created directories 0700

**Files:**
- Modify: `crates/path-cli/src/store.rs` (`ObjectUri::put`, helper `tighten_local_permissions`, test)
- Modify: `crates/path-cli/tests/object_storage.rs` (one test)

**Interfaces:**
- Produces: private `fn tighten_local_permissions(file: &Path, created_dirs: &[PathBuf])` (unix only; no-op elsewhere). `put` computes which ancestors of the target are absent before writing and passes them.

- [ ] **Step 1: Write the failing test**

Integration, in `object_storage.rs`:

```rust
#[cfg(unix)]
#[test]
fn folder_exports_are_private_and_created_directories_are_too() {
    use std::os::unix::fs::PermissionsExt;
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());
    let dest = root.path().join("new").join("deeper");

    cmd(config.path())
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &dest.to_string_lossy()])
        .assert()
        .success();

    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dest.join("2026-01-01-hello--g1.json")), 0o600);
    assert_eq!(mode(&dest), 0o700);
    assert_eq!(mode(&root.path().join("new")), 0o700);
    // A directory that existed before the export is left alone.
    let before = mode(root.path());
    cmd(config.path())
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &dest.to_string_lossy()])
        .assert()
        .success();
    assert_eq!(mode(root.path()), before);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p path-cli --test object_storage folder_exports_are_private`
Expected: FAIL (`0o644` ≠ `0o600`).

- [ ] **Step 3: Implement**

In `ObjectUri::put`, before `let opened = open(...)`:

```rust
        // For a folder, remember which directories don't exist yet so
        // only the ones this write creates get tightened.
        let created_dirs: Vec<PathBuf> = if self.url.scheme() == "file" {
            self.url
                .to_file_path()
                .ok()
                .map(|target| {
                    target
                        .ancestors()
                        .skip(1)
                        .take_while(|d| !d.exists())
                        .map(std::path::Path::to_path_buf)
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
```

and change the `Ok(_) => Ok(())` arm to:

```rust
            Ok(_) => {
                if self.url.scheme() == "file"
                    && let Ok(target) = self.url.to_file_path()
                {
                    tighten_local_permissions(&target, &created_dirs);
                }
                Ok(())
            }
```

Add the helper near `friendly`:

```rust
/// A folder destination gets the same protection as the cache: the
/// object 0600, and every directory this write created 0700. Directories
/// that already existed (a Dropbox root, a shared mount) are left as the
/// user had them. Best effort: a permission failure on a foreign
/// filesystem must not turn a successful upload into an error.
#[cfg(unix)]
fn tighten_local_permissions(file: &std::path::Path, created_dirs: &[PathBuf]) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600));
    for dir in created_dirs {
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
}

#[cfg(not(unix))]
fn tighten_local_permissions(_file: &std::path::Path, _created_dirs: &[PathBuf]) {}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p path-cli store::tests && cargo test -p path-cli --test object_storage`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/store.rs crates/path-cli/tests/object_storage.rs
git commit -m "fix(store): write folder objects 0600 and created directories 0700"
```

---
### Task 16: `path share --to <destination>` and object destinations in `[[project]] remote`

**Files:**
- Modify: `crates/path-cli/src/remote.rs` (`Remote` enum, `parse_remote`, tests)
- Modify: `crates/path-cli/src/share_config.rs` (`ConfiguredRemote`, `global_rule`, tests)
- Modify: `crates/path-cli/src/cmd_share.rs` (`ShareArgs.to`, `run`, `share_explicit`, picker path, `ShareDestination`, `resolve_destination`)
- Modify: `crates/path-cli/tests/integration.rs` (two new tests next to `share_configured_repo_requires_login`)

**Interfaces:**
- Produces:
  - `pub(crate) enum Remote { Pathbase { repo: RepoSpec, base_url: Option<String> }, Object(String) }`; `pub(crate) fn parse_remote(value: &str, origin: &str) -> Result<Remote>`.
  - `ConfiguredRemote { remote: Remote, display: String, origin: String }` (fields `repo` and `base_url` removed).
  - `ShareArgs.to: Option<String>`, conflicting with `repo`, `anon`, `name`, `public`, `url`.
  - `enum ShareTarget { Pathbase { repo: Option<RepoSpec>, base_url: String }, Object(crate::store::Destination) }`; `ShareDestination { target: ShareTarget }`.
- Consumes: `cmd_export::{export_body, ExportOptions, ObjectOutcome}`, `crate::store::Destination::parse`, `crate::export_ledger::Ledger`.

- [ ] **Step 1: Write the failing remote and config unit tests**

In `remote.rs` tests (add a `#[cfg(test)] mod tests` if there is none; check the bottom of the file):

```rust
    #[test]
    fn object_remotes_are_recognized_by_scheme_or_path_shape() {
        for value in [
            "s3://team-bucket/traces",
            "s3a://team-bucket/traces",
            "file:///srv/traces",
            "/srv/traces",
            "~/Dropbox/traces",
            "./traces",
        ] {
            match parse_remote(value, "test").unwrap() {
                Remote::Object(d) => assert_eq!(d, value),
                other => panic!("{value} parsed as {other:?}"),
            }
        }
    }

    #[test]
    fn pathbase_remotes_still_parse() {
        assert!(matches!(
            parse_remote("team/sessions", "test").unwrap(),
            Remote::Pathbase { base_url: None, .. }
        ));
        assert!(matches!(
            parse_remote("https://pathbase.dev/u/team/sessions", "test").unwrap(),
            Remote::Pathbase { base_url: Some(_), .. }
        ));
    }

    #[test]
    fn a_bare_relative_object_remote_is_rejected_like_a_destination() {
        // `team-bucket/traces` is ambiguous with `owner/name`, and as a
        // destination it is the bare-relative trap; it stays a Pathbase
        // repo spec, which is what it always was.
        assert!(matches!(
            parse_remote("team-bucket/traces", "test").unwrap(),
            Remote::Pathbase { .. }
        ));
        let err = parse_remote("gs://bucket", "test").unwrap_err().to_string();
        assert!(err.contains("unsupported remote scheme"), "{err}");
    }
```

`Remote` must derive `Debug`. In `share_config.rs` tests, add:

```rust
    #[test]
    fn an_object_destination_remote_resolves_to_an_object_target() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let config = temp.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "[[project]]\ndir = {:?}\nremote = \"s3://team-bucket/traces\"\n",
                project.display().to_string()
            ),
        )
        .unwrap();
        let found = resolve_remote_from(&config, None, &project)
            .unwrap()
            .expect("rule matches");
        assert!(matches!(found.remote, crate::remote::Remote::Object(ref d) if d == "s3://team-bucket/traces"));
        assert_eq!(found.display, "s3://team-bucket/traces");
        assert_eq!(validate_config_text(&std::fs::read_to_string(&config).unwrap(), "config.toml").unwrap(), 1);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p path-cli remote:: share_config::`
Expected: compile errors (`Remote` missing).

- [ ] **Step 3: Implement `Remote`**

Replace `parse_remote` in `remote.rs`:

```rust
/// Where a configured share goes.
#[derive(Debug, Clone)]
pub(crate) enum Remote {
    /// A Pathbase repo, optionally pinned to a server.
    Pathbase {
        repo: RepoSpec,
        base_url: Option<String>,
    },
    /// An object-storage destination (`s3://bucket/prefix`, `file:///dir`,
    /// or a folder path), kept as written and parsed with
    /// `store::Destination::parse` at use.
    Object(String),
}

/// Parse a remote value: bare `owner/name` (Pathbase, default server), a
/// canonical Pathbase repo web URL whose authority becomes the server
/// base URL, or an object-storage destination — `s3://`, `s3a://`,
/// `file://`, or a folder path starting with `/`, `~`, `./`, or `../`.
/// `origin` names where the value came from, for error messages.
pub(crate) fn parse_remote(value: &str, origin: &str) -> Result<Remote> {
    if let Some((scheme, _)) = value.split_once("://") {
        return match scheme {
            "http" | "https" => {
                let (base_url, repo) = parse_pathbase_repo_url(value, origin)?;
                Ok(Remote::Pathbase {
                    repo,
                    base_url: Some(base_url),
                })
            }
            "s3" | "s3a" | "file" => {
                crate::store::Destination::parse(value)
                    .map_err(|e| anyhow!("invalid destination in {origin}: {e:#}"))?;
                Ok(Remote::Object(value.to_string()))
            }
            other => bail!(
                "unsupported remote scheme `{other}` in {origin}: expected `owner/name`, a \
                 Pathbase repo URL like https://pathbase.dev/u/owner/name, or an object \
                 destination like s3://bucket/prefix or a folder path"
            ),
        };
    }
    if value.starts_with('/')
        || value.starts_with('~')
        || value.starts_with("./")
        || value.starts_with("../")
    {
        crate::store::Destination::parse(value)
            .map_err(|e| anyhow!("invalid destination in {origin}: {e:#}"))?;
        return Ok(Remote::Object(value.to_string()));
    }
    let repo = parse_repo_spec(value).map_err(|e| anyhow!("invalid remote in {origin}: {e}"))?;
    Ok(Remote::Pathbase {
        repo,
        base_url: None,
    })
}
```

Update the module doc's "unknown schemes are rejected today" sentence to say object destinations are accepted.

In `share_config.rs`, change `ConfiguredRemote`:

```rust
#[derive(Debug)]
pub(crate) struct ConfiguredRemote {
    pub(crate) remote: crate::remote::Remote,
    pub(crate) display: String,
    pub(crate) origin: String,
}
```

and the tail of `global_rule`:

```rust
    let remote = parse_remote(value, &origin)?;
    Ok(Some(ConfiguredRemote {
        remote,
        display: value.to_string(),
        origin,
    }))
```

Fix the existing `share_config` tests that read `found.repo` / `found.base_url`: destructure `found.remote` as `crate::remote::Remote::Pathbase { repo, base_url }` and assert on those.

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p path-cli remote:: share_config::`
Expected: PASS (the crate still fails to compile until `cmd_share` is updated; if so, do Step 5 first, then run).

- [ ] **Step 5: Write the failing share integration tests**

In `crates/path-cli/tests/integration.rs`, after `share_configured_repo_requires_login`:

```rust
/// `--to` sends the session to object storage instead of Pathbase: no
/// login, no server, one legible object in the destination.
#[test]
fn share_to_a_folder_writes_one_object_and_needs_no_pathbase() {
    let (temp, project) = claude_session_fixture();
    let cfg = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();

    let out = cmd()
        .env("HOME", temp.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .env("AWS_SHARED_CREDENTIALS_FILE", "/nonexistent/credentials")
        .env("AWS_CONFIG_FILE", "/nonexistent/config")
        .args(["share", "--harness", "claude", "--session", "session-abc", "--project"])
        .arg(&project)
        .args(["--no-cache", "--to"])
        .arg(folder.path())
        .assert()
        .success();
    let uri = String::from_utf8(out.get_output().stdout.clone()).unwrap().trim().to_string();
    assert!(uri.contains("--path-claude-code-"), "{uri}");
    let names: Vec<String> = std::fs::read_dir(folder.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].ends_with(".json"));
}

/// A `[[project]]` rule whose remote is a destination routes `share`
/// there without `--to`, and says so.
#[test]
fn share_follows_a_configured_object_destination() {
    let (temp, project) = claude_session_fixture();
    let cfg = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.toml"),
        format!(
            "[[project]]\ndir = {:?}\nremote = {:?}\n",
            project.display().to_string(),
            folder.path().display().to_string()
        ),
    )
    .unwrap();

    cmd()
        .env("HOME", temp.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .env("AWS_SHARED_CREDENTIALS_FILE", "/nonexistent/credentials")
        .env("AWS_CONFIG_FILE", "/nonexistent/config")
        .args(["share", "--harness", "claude", "--session", "session-abc", "--project"])
        .arg(&project)
        .args(["--no-cache", "--url", "http://127.0.0.1:1"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Sharing to"))
        .stderr(predicate::str::contains("Uploaded"));
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
}
```

- [ ] **Step 6: Implement in `cmd_share.rs`**

Add to `ShareArgs`:

```rust
    /// Share to object storage instead of Pathbase: `s3://bucket/prefix`,
    /// or a folder (`~/traces`). Needs no Pathbase login.
    #[arg(long, value_name = "DESTINATION", conflicts_with_all = ["repo", "anon", "name", "public", "url"])]
    pub to: Option<String>,
```

Replace `ShareDestination` and `resolve_destination`:

```rust
/// Where an upload goes.
#[derive(Debug)]
enum ShareTarget {
    /// A Pathbase repo (`None` = the pathstash default) on a server.
    Pathbase {
        repo: Option<RepoSpec>,
        base_url: String,
    },
    /// An object-storage destination.
    Object(crate::store::Destination),
}

#[derive(Debug)]
struct ShareDestination {
    target: ShareTarget,
}

/// Apply flags and config to the upload destination. `--to` wins and
/// names object storage; `--repo` wins for Pathbase; explicit `--anon`
/// skips config entirely (anonymous uploads have no repo); otherwise a
/// remote configured for the session's directory applies (see
/// `share_config`), which may be a Pathbase repo or an object
/// destination. A URL-form Pathbase remote also carries the server,
/// which replaces `base_url` unless `--url` was given — flags win. A
/// configured Pathbase remote needs an authed upload, so hitting one
/// while unauthenticated is an error rather than a silent fall-through
/// to the anonymous endpoint.
fn resolve_destination(
    args: &ShareArgs,
    auth: &crate::cmd_pathbase::AuthMode,
    base_url: String,
    session_dir: Option<PathBuf>,
) -> Result<ShareDestination> {
    if let Some(to) = &args.to {
        return Ok(ShareDestination {
            target: ShareTarget::Object(crate::store::Destination::parse(to)?),
        });
    }
    let pathbase = |repo: Option<RepoSpec>, base_url: String| ShareDestination {
        target: ShareTarget::Pathbase { repo, base_url },
    };
    if args.repo.is_some() || args.anon {
        return Ok(pathbase(args.repo.clone(), base_url));
    }
    let Some(dir) = session_dir else {
        return Ok(pathbase(None, base_url));
    };
    let Some(found) = crate::share_config::resolve_remote(&dir)? else {
        return Ok(pathbase(None, base_url));
    };
    match found.remote {
        crate::remote::Remote::Object(destination) => {
            eprintln!("Sharing to {} ({})", found.display, found.origin);
            Ok(ShareDestination {
                target: ShareTarget::Object(crate::store::Destination::parse(&destination)?),
            })
        }
        crate::remote::Remote::Pathbase {
            repo,
            base_url: remote_url,
        } => {
            if matches!(auth, crate::cmd_pathbase::AuthMode::Anon) {
                let login_url = remote_url
                    .as_ref()
                    .map(|u| format!(" --url {u}"))
                    .unwrap_or_default();
                anyhow::bail!(
                    "sessions in {} are configured to upload to {} ({}), which requires login.\n\
                     Run `path auth login{login_url}`, or pass --anon to upload anonymously instead.",
                    dir.display(),
                    found.display,
                    found.origin,
                );
            }
            let base_url = match (&args.url, remote_url) {
                (None, Some(remote_url)) => remote_url,
                _ => base_url,
            };
            eprintln!("Sharing to {} ({})", found.display, found.origin);
            Ok(pathbase(Some(repo), base_url))
        }
    }
}

/// Send `body` where `dest` says. Pathbase uploads print their share
/// URL; object uploads print the object location.
fn deliver(
    dest: ShareDestination,
    args: &ShareArgs,
    auth: crate::cmd_pathbase::AuthMode,
    body: &str,
    ledger_key: &str,
    summary: &str,
) -> Result<()> {
    match dest.target {
        ShareTarget::Pathbase { repo, base_url } => {
            let upload = crate::cmd_export::PathbaseUploadArgs {
                url: args.url.clone(),
                anon: args.anon,
                repo,
                name: args.name.clone(),
                public: args.public,
            };
            crate::cmd_export::run_pathbase_inner(auth, base_url, upload, body, summary)
        }
        ShareTarget::Object(destination) => {
            let settings = crate::store::effective_settings()?;
            let outcome = crate::cmd_export::export_body(
                body,
                ledger_key,
                std::path::Path::new(summary),
                &destination,
                &settings,
                &crate::export_ledger::Ledger::new(),
                &crate::cmd_export::ExportOptions::default(),
            )?;
            if let crate::cmd_export::ObjectOutcome::Uploaded(uri) = outcome {
                println!("{uri}");
            }
            Ok(())
        }
    }
}
```

In `run`, make preflight conditional on not having `--to`. Replace the explicit-args block and the pre-picker preflight:

```rust
    let preflight = |needs_auth: bool| -> Result<crate::cmd_pathbase::AuthMode> {
        if args.to.is_some() {
            // Object storage needs no Pathbase session at all.
            return Ok(crate::cmd_pathbase::AuthMode::Anon);
        }
        crate::cmd_pathbase::preflight_auth(&base_url, upload_args.anon, needs_auth)
    };

    if let (Some(h), Some(session)) = (harness, &args.session) {
        // Explicit-args: validate creds before derive so a credential
        // failure doesn't waste the derive/cache work.
        let auth = preflight(needs_auth)?;
        return share_explicit(h, session.as_str(), &args, auth, base_url);
    }
```

and further down replace `let auth = crate::cmd_pathbase::preflight_auth(&base_url, upload_args.anon, needs_auth)?;` with `let auth = preflight(needs_auth)?;`. Also update the picker header so it does not promise Pathbase when `--to` is set:

```rust
    let header = match &args.to {
        Some(to) => format!("share an agent session (Enter = export to {to})"),
        None => format!("share an agent session (Enter = upload to {base_url})"),
    };
```

and the non-TTY manual recipe:

```rust
        eprintln!(
            "Interactive `path share` needs an interactive terminal.\n\
             \n\
             Manual recipe:\n  \
             path p import <harness>            # writes a cache entry, prints its ID\n  \
             path p export pathbase --input <id> # or: path p export object --input <id> --to <destination>"
        );
        anyhow::bail!("no interactive terminal; run `path p import <harness>` then `path p export`");
```

In `share_explicit`, replace both `run_pathbase_inner` calls. The fast path (cached doc):

```rust
        let dest = resolve_destination(args, &auth, base_url, session_dir)?;
        let summary = format!("{} session {}", harness.name(), cache_id);
        return deliver(dest, args, auth, &body, &cache_id, &summary);
```

and the derive path:

```rust
    let dest = resolve_destination(args, &auth, base_url, session_dir)?;
    let body = derived.doc.to_json()?;
    deliver(dest, args, auth, &body, &derived.cache_id, &summary)
```

The picker path ends by building an `explicit: ShareArgs { … }` and calling `share_explicit`, so it needs no upload change of its own; add the new field to that struct literal so `--to` survives the round trip:

```rust
    let explicit = ShareArgs {
        url: args.url.clone(),
        anon: args.anon,
        repo: args.repo.clone(),
        name: args.name.clone(),
        public: args.public,
        harness: h.harness(),
        session: None, // unused by share_explicit
        project: if h.path_keyed() {
            Some(PathBuf::from(&key))
        } else {
            None
        },
        no_cache: args.no_cache,
        to: args.to.clone(),
    };
```

- [ ] **Step 7: Run everything**

Run: `cargo test -p path-cli`
Expected: PASS, including the two new integration tests and the older share tests (their Pathbase behavior is unchanged).

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/remote.rs crates/path-cli/src/share_config.rs crates/path-cli/src/cmd_share.rs crates/path-cli/tests/integration.rs
git commit -m "feat(share): --to <destination> and object-storage remotes in [[project]] rules"
```

---

### Task 17: `path query --source` warns when nothing matches

**Files:**
- Modify: `crates/path-cli/src/query/mod.rs` (`select_files`)
- Modify: `crates/path-cli/tests/query.rs` (one test)

- [ ] **Step 1: Write the failing test**

`crates/path-cli/tests/query.rs` already has a `cmd()` helper (sandboxed `$HOME`, no `CLAUDE_CONFIG_DIR`) and a `seed(cfg, id, json)` helper that writes a cache document. Add:

```rust
#[test]
fn an_unknown_source_warns_instead_of_silently_returning_nothing() {
    let cfg = tempfile::tempdir().unwrap();
    seed(cfg.path(), "claude-abc", r#"{"graph":{"ID":"g"},"paths":[]}"#);

    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["query", "--no-sync", "--source", "objekt", "length"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("0"))
        .stderr(predicate::str::contains("no cached documents with source `objekt`"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p path-cli --test query an_unknown_source_warns`
Expected: FAIL (no warning on stderr).

- [ ] **Step 3: Implement**

In `select_files`, after the `for entry in crate::cache::list_cached()?` loop and before the `--id` check:

```rust
        // A `--source` that selects nothing is almost always a typo or a
        // source that was never imported; say so rather than answering
        // an empty question with an empty answer.
        if let Some(name) = &scope.source
            && id_set.is_none()
            && sources.is_empty()
        {
            eprintln!(
                "warning: no cached documents with source `{name}`; run `path p cache ls` to see \
                 what's cached"
            );
        }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p path-cli --test query`
Expected: PASS. If a query test asserts an exactly-empty stderr for a `--source` that matches nothing, update it to expect the warning.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/path-cli/src/query/mod.rs crates/path-cli/tests/query.rs
git commit -m "fix(query): warn when --source selects no cached documents"
```

---

### Task 18: Reconcile the docs with the shipped behavior

**Files:**
- Modify: `crates/path-cli/src/store.rs` (module doc lines 5-9, the comment on `S3Settings::profile`/`resolve_with` mentioning `--profile` on a command, the `ObjectName` doc)
- Modify: `crates/path-cli/src/cmd_auth.rs` (already fixed `path target` in Task 7; verify)
- Modify: `CLAUDE.md` (the "Object-storage transport", "S3 credentials", and precedence paragraphs; the "CLI usage" block)
- Modify: `CHANGELOG.md` (rewrite the `path-cli 0.21.0` section)
- Modify: `README.md` (CLI reference block; a new "Object storage" subsection under "Beyond sessions" or "Quick start")
- Modify: `crates/path-cli/README.md` (new sections)
- Modify: `site/pages/cli.md` (command block)
- Modify: `site/_data/crates.json` (`path-cli` role text)
- Test: `crates/path-cli/tests/object_storage.rs` (one help-text test)

- [ ] **Step 1: Write the failing help-text test**

```rust
#[test]
fn help_text_describes_nothing_that_does_not_exist() {
    let config = tempfile::tempdir().unwrap();
    for args in [
        vec!["auth", "s3", "login", "--help"],
        vec!["auth", "s3", "--help"],
        vec!["p", "export", "object", "--help"],
        vec!["p", "import", "object", "--help"],
        vec!["resume", "--help"],
    ] {
        let out = cmd(config.path()).args(&args).assert().success();
        let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(!text.contains("path target"), "{args:?}: {text}");
    }
    cmd(config.path())
        .args(["p", "export", "object", "--help"])
        .assert()
        .stdout(predicate::str::contains("full document"))
        .stdout(predicate::str::contains("--<graph id>"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p path-cli --test object_storage help_text`
Expected: FAIL on the `path target` reference in `store.rs`'s module doc only if that text reaches help (it does not; it is a module doc), so this test may already pass after Task 7. Keep it: it pins the contract.

- [ ] **Step 3: Fix in-code docs**

`crates/path-cli/src/store.rs` module doc, lines 5-9, replace:

```rust
//! any S3-compatible endpoint (Cloudflare R2, MinIO, Ceph, Backblaze
//! B2), and a plain local directory via `file://`. A folder is a
//! first-class destination, not a testing affordance: `--to ~/Dropbox/traces`
//! is a complete setup, needing no credentials at all — credentials are
//! resolved only for `s3://`. It is also what the tests round-trip
//! against, so share and resume are exercised end-to-end without a
//! network.
```

Replace the doc comment on `impl S3Settings` (`profile` is threaded through so `--profile` on a command reaches the resolver):

```rust
/// The credential resolution this settings blob implies.
///
/// `profile` names an AWS profile stored by `path auth s3 login
/// --profile`; `AWS_PROFILE` is the per-invocation override, as with
/// every other AWS tool.
```

Search the file for `path target` and `--profile` and fix any remaining mention. Search `crates/path-cli/src` for `path target` (`grep -rn "path target" crates/path-cli/src`) and confirm zero hits.

- [ ] **Step 4: CLAUDE.md**

Replace the four object-storage paragraphs (from `**Object-storage transport**` through the `Precedence (AWS's own…)` paragraph) with:

```markdown
**Object-storage transport** lives in `crates/path-cli/src/store.rs`, which splits *where a document goes* (`Destination`, `ObjectUri`, `ObjectName` — pure URL parsing and naming, no credentials) from *how to reach it* (`S3Settings` at `~/.toolpath/s3.json`). Transport is the `object_store` crate, so one code path serves AWS S3, any S3-compatible endpoint (R2, MinIO, Ceph, B2 — set `endpoint`), and `file://` for a local folder; it's async, so it tunnels through the same `cmd_pathbase::block_on` runtime the Pathbase client uses. `file://` is a first-class destination: it is what the tests round-trip against, so export/import/resume/list are covered with no network and no mock HTTP server, and it resolves **no credentials** — `store_options` is keyed on the URL scheme, so a folder export never reads `~/.aws` or spawns the AWS CLI. Accepted schemes are deliberately narrower than what `object_store` parses (`s3`, `s3a`, `file`): `http`/`https` belong to Pathbase in the same dispatch, `gs://`/`az://` would need feature flags we don't compile in, and `memory://` is a fresh per-process store whose contents vanish before the command exits. A scheme-less destination is a **local path**, and a *bare relative* one (`my-bucket/traces`) is rejected — it's overwhelmingly a bucket name typed from memory.

Object names are `<date>-<topic>--<id>.json` (`store::name_for`), where the ID is the document's `graph.id` and `--` is reserved (the slugger collapses dash runs, so neither date nor topic can contain it; `ObjectName::id_of` splits on the last `--`, and a name with no date or topic is `<id>.json`). Every component is a pure function of the document — never of the input filename — so a re-export overwrites its own object, two different documents with the same basename land on two keys, and the date is the *earliest* step's so it doesn't move as the session grows. Imports cache under `object-<id>` (`ObjectUri::cache_id`, from the URI alone), so a round trip is identity-preserving and a mirror loop is a fixed point. `p export object` validates the body (parse + schema) before any put; `--force` uploads anyway. Every upload is recorded in `~/.toolpath/exports.json` (`export_ledger`: destination → cache ID → uri, sha256, bytes, time, uploader), which `p export object --all` uses to skip unchanged documents and which doubles as the local egress record. Overwrite is the default put; `--no-overwrite` (or `no_overwrite` in `s3.json`) is a create-only put. S3 objects carry `x-amz-meta-toolpath-*` metadata (graph ID, sha256, uploader, CLI version, time); folders can't, and get 0600 files / 0700 created directories instead.

**S3 credentials** are resolved by `crate::aws_creds`, not by `object_store` alone. `object_store` covers the *server* cases (EKS/IRSA web identity, ECS task roles, EC2 instance metadata) and deliberately reads no `~/.aws` at all, because it avoids depending on the AWS SDK. `aws_creds` fills that in: static-key profiles are parsed straight out of `~/.aws/credentials`, and anything else — SSO, `role_arn` chains, `credential_process` — is delegated to `aws configure export-credentials --format process`. An expired SSO session is offered `aws sso login --profile <name>` once, on a TTY only; with no terminal it fails with the exact command. Resolution errors **propagate** on `s3://` (no fall-through to instance metadata under a stranger's principal); "nothing configured" is not an error and uses the instance chain. Depending on `aws-config` instead would be 31 crates *and* an MSRV treadmill.

Precedence (AWS's own, with our stored settings layered on top): `path auth s3 login` keys → a named profile (`s3.json` `profile`, then `$AWS_PROFILE`) → `AWS_ACCESS_KEY_ID` → the `[default]` profile → `object_store`'s instance chain. Region falls back to the profile's, then `us-east-1`. `path auth s3 status` prints *which* source won, the access key ID for every source (never the secret), and the effective region; `path auth s3 whoami` asks STS through the AWS CLI with the resolved credentials. `AWS_SHARED_CREDENTIALS_FILE` / `AWS_CONFIG_FILE` are honored, which is also how the integration tests stay off a developer's real profiles.
```

In the "CLI usage" block, after the `p export pathbase` line, add:

```bash
cargo run -p path-cli -- p export object --input <ref> --to s3://bucket/prefix   # or a folder; --all, --dry-run, --no-overwrite
cargo run -p path-cli -- p import object s3://bucket/prefix/2026-03-04-fix-the-parser--path-claude-code-abc.json
cargo run -p path-cli -- p import object s3://bucket/prefix      # every document under a prefix
cargo run -p path-cli -- p list object s3://bucket/prefix --format tsv
cargo run -p path-cli -- share --to ~/Dropbox/traces
cargo run -p path-cli -- auth s3 login | status | whoami | logout
```

Also in the share paragraph under "CLI behaviors", after the sentence about `remote` grammar, add: "`remote` may also be an object destination (`s3://bucket/prefix`, `file:///dir`, or a folder path), in which case `share` exports there instead of to Pathbase; `--to` on the command does the same per call."

- [ ] **Step 5: CHANGELOG**

Replace the whole `## path-cli 0.21.0 — 2026-09-11` section (up to but not including `## path-cli 0.20.0`) with:

```markdown
## path-cli 0.21.0 — 2026-09-14

**Share and resume over object storage.** `path-cli` (0.21.0) can write
toolpath documents to an S3 bucket, any S3-compatible endpoint (R2,
MinIO, Ceph, B2), or a plain folder — and read them back.

```bash
path p export object --input claude-abc --to s3://my-bucket/traces
path p export object --all --to ~/Dropbox/toolpath-traces   # every cached session; unchanged ones skipped
path p list object s3://my-bucket/traces --format tsv
path p import object s3://my-bucket/traces                  # every document under the prefix
path share --to s3://my-bucket/traces
path resume s3://my-bucket/traces                            # pick from the bucket
```

**Objects are named to be read and to be parsed.** A document lands at
`<date>-<topic>--<id>.json`, e.g.
`2026-08-07-add-s3-support--path-claude-code-6f2a1c9e5b3d4a70.json`.
The ID is the document's `graph.id`; the date is the session's first
step; `--` is reserved, so automation takes everything after the last
`--`. Every part is a function of the document, never of the input
filename, so a re-export overwrites its own object and two different
documents never collide. Imports land in the cache as `object-<id>`.

**Credentials come from wherever you already keep them.** `~/.aws`
profiles, `AWS_PROFILE`, environment keys, and SSO / `role_arn` /
`credential_process` profiles via `aws configure export-credentials`.
A folder needs none and never consults them. An expired SSO session is
offered `aws sso login` on a terminal and fails with that command
otherwise; a resolution failure on `s3://` is an error, never a silent
fall-through to instance metadata. `path auth s3 login` stores
connection settings for endpoints the AWS tooling doesn't know;
`path auth s3 status` says which credential source won and as which
key; `path auth s3 whoami` asks STS.

**A record store when you want one.** `--no-overwrite` (or a stored
`no_overwrite`) makes puts create-only; `auth s3 login --sse aws:kms
--kms-key-id …` sets server-side encryption; S3 objects carry
`toolpath-*` metadata naming the graph ID, sha256, uploader, CLI
version, and time; every upload is recorded locally in
`~/.toolpath/exports.json`. Folder objects are 0600.

`p export object` validates the document before uploading (`--force`
to skip). `--dry-run` prints the resolved location, endpoint, region,
credential source, and put mode. `[[project]] remote` in
`~/.toolpath/config.toml` accepts an object destination. `path query
--source` warns when it matches nothing.

**`toolpath-cli`** (0.21.0): lockstep bump of the deprecated shim.
```

- [ ] **Step 6: README, path-cli README, site, crates.json**

`README.md` CLI reference block: add these lines in the right places.

Under `auth`:
```
  auth        login | status | whoami | logout [--url URL]
              s3 login | status | whoami | logout
```
Under `p list`:
```
      object    DESTINATION [--format ...]
```
Under `p import`:
```
      object    OBJECT-URL-OR-DESTINATION
```
Under `p export`:
```
      object    (--input REF | --all [--include-imported]) --to DESTINATION [--force] [--dry-run] [--no-overwrite]
```
And under `share`:
```
  share       # one-shot interactive picker + Pathbase upload, or --to DESTINATION for object storage
```

Add a subsection to `README.md` after the "Quick start" section:

```markdown
## Back up sessions to a bucket or a folder

```bash
path p export object --all --to ~/Dropbox/toolpath-traces      # a folder needs no credentials
path p export object --all --to s3://my-bucket/traces           # uses your ~/.aws profile
path resume s3://my-bucket/traces                                # pick a session on another machine
```

Objects are named `<date>-<topic>--<id>.json`; re-running overwrites a
session's own object and skips unchanged ones. Objects hold the full
document: every turn, verbatim diffs, and tool output.
```

`crates/path-cli/README.md`: add after `### p import`:

```markdown
### p export object / p import object / p list object

Object storage — an S3 bucket, an S3-compatible endpoint, or a folder —
as a backup, a hand-off between machines, or a team's shared record.

```bash
# One session, or every cached session (unchanged ones are skipped)
path p export object --input claude-abc --to s3://my-bucket/traces
path p export object --all --to ~/Dropbox/toolpath-traces

# See what is there without downloading anything
path p list object s3://my-bucket/traces --format tsv

# Bring one back, or everything under a prefix
path p import object s3://my-bucket/traces/2026-03-04-fix-the-parser--path-claude-code-abc.json
path p import object s3://my-bucket/traces

# Nightly, from cron: sync the cache, push what changed
path p cache sync && path p export object --all --to s3://team-bucket/traces
```

Objects are named `<date>-<topic>--<id>.json`. The ID is the document's
`graph.id`, and `--` is reserved: split on the last `--` to get it.
Objects hold the full document — every turn, verbatim diffs, and tool
output — so treat the bucket as you would the sessions themselves.

Credentials: a folder needs none. For `s3://`, your `~/.aws` profiles
(SSO included, via the AWS CLI), `AWS_PROFILE`, or environment keys are
used automatically; `path auth s3 login` stores settings for endpoints
the AWS tooling doesn't know (MinIO, R2). `path auth s3 status` shows
which credential source is in effect; `path auth s3 whoami` asks STS.

#### Object storage as a record store

By default a re-export replaces a session's own object. For a bucket
that must be a record:

- `path auth s3 login --no-overwrite` makes every put create-only
  (`--no-overwrite` on a single export does the same once).
- `path auth s3 login --sse aws:kms --kms-key-id <key>` sets server-side
  encryption; a bucket policy that denies unencrypted puts then works.
- S3 objects carry `x-amz-meta-toolpath-graph-id`, `-sha256`,
  `-uploader`, `-cli-version`, and `-uploaded-at`.
- `~/.toolpath/exports.json` records every upload from this machine.

The bucket supplies the rest: Versioning and Object Lock for
immutability, bucket-owner-enforced ownership, server access logging,
and CloudTrail data events for the authoritative principal behind each
write.
```

`site/pages/cli.md`: in the command block add `object    DESTINATION [--format ...]` under `list`, `object    OBJECT-URL-OR-DESTINATION` under `import`, `object    (--input REF | --all) --to DESTINATION [--force] [--dry-run] [--no-overwrite]` under `export`, and `s3 login | status | whoami | logout` under `auth`; change the `share` comment to `# picker + Pathbase upload, or --to DESTINATION for object storage`.

`site/_data/crates.json`, `path-cli` `role`: append ` Object storage round-trip via \`p export object\` / \`p import object\` / \`p list object\` (S3, S3-compatible, or a folder), with \`share --to\` and \`resume <destination>\`.`

- [ ] **Step 7: Verify and commit**

Run:
```bash
cargo test -p path-cli --test object_storage help_text
grep -rn "path target" crates/path-cli/src CLAUDE.md README.md crates/path-cli/README.md site/pages/cli.md; echo "expect no output above"
grep -n "\-\-profile" CLAUDE.md CHANGELOG.md | grep -v "sso login --profile\|login --profile\|auth s3 login" ; echo "expect no output above"
cd site && pnpm run build && cd ..
```
Expected: test passes; both greps print nothing; the site builds (12 pages).

```bash
git add crates/path-cli/src/store.rs CLAUDE.md CHANGELOG.md README.md crates/path-cli/README.md site/pages/cli.md site/_data/crates.json crates/path-cli/tests/object_storage.rs
git commit -m "docs: describe object storage as shipped — naming contract, credentials, automation, record-store posture"
```

---

### Task 19: Live S3 round trip and a MinIO script

**Files:**
- Modify: `crates/path-cli/tests/object_storage.rs` (one `#[ignore]` test)
- Create: `scripts/test-object-storage-live.sh`

**Interfaces:**
- The test reads `TOOLPATH_S3_TEST_BUCKET` (required), `TOOLPATH_S3_TEST_ENDPOINT` (optional), and the standard `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_REGION`.

- [ ] **Step 1: Write the ignored live test**

```rust
// ── live S3 (opt in) ────────────────────────────────────────────────

/// Round-trips one document through a real S3 endpoint. Ignored unless
/// run explicitly with the environment below; `scripts/test-object-storage-live.sh`
/// wires it to a MinIO container.
#[test]
#[ignore = "needs TOOLPATH_S3_TEST_BUCKET and credentials; run via scripts/test-object-storage-live.sh"]
fn live_s3_round_trip() {
    let bucket = std::env::var("TOOLPATH_S3_TEST_BUCKET").expect("TOOLPATH_S3_TEST_BUCKET");
    let endpoint = std::env::var("TOOLPATH_S3_TEST_ENDPOINT").ok();
    let key = std::env::var("AWS_ACCESS_KEY_ID").expect("AWS_ACCESS_KEY_ID");
    let secret = std::env::var("AWS_SECRET_ACCESS_KEY").expect("AWS_SECRET_ACCESS_KEY");
    let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string());

    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc_with_id(work.path(), "path-live-test-0001");
    let prefix = format!("s3://{bucket}/toolpath-live-{}", std::process::ID());

    let mut export = cmd(config.path());
    export
        .env("AWS_ACCESS_KEY_ID", &key)
        .env("AWS_SECRET_ACCESS_KEY", &secret)
        .env("AWS_REGION", &region)
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &prefix]);
    if let Some(e) = &endpoint {
        export.env("AWS_ENDPOINT_URL_S3", e);
    }
    let out = export.assert().success();
    let uri = String::from_utf8(out.get_output().stdout.clone()).unwrap().trim().to_string();
    assert!(uri.ends_with("2026-01-01-hello--path-live-test-0001.json"), "{uri}");

    let mut list = cmd(config.path());
    list.env("AWS_ACCESS_KEY_ID", &key)
        .env("AWS_SECRET_ACCESS_KEY", &secret)
        .env("AWS_REGION", &region)
        .args(["p", "list", "object", &prefix, "--format", "tsv"]);
    if let Some(e) = &endpoint {
        list.env("AWS_ENDPOINT_URL_S3", e);
    }
    list.assert()
        .success()
        .stdout(predicate::str::starts_with("path-live-test-0001\t"));

    let mut import = cmd(config.path());
    import
        .env("AWS_ACCESS_KEY_ID", &key)
        .env("AWS_SECRET_ACCESS_KEY", &secret)
        .env("AWS_REGION", &region)
        .args(["p", "import", "object", &uri]);
    if let Some(e) = &endpoint {
        import.env("AWS_ENDPOINT_URL_S3", e);
    }
    import.assert().success();
    assert!(config.path().join("documents/object-path-live-test-0001.json").is_file());
}
```

- [ ] **Step 2: Confirm it is skipped by default**

Run: `cargo test -p path-cli --test object_storage live_s3`
Expected: `1 ignored`.

- [ ] **Step 3: Write the script**

`scripts/test-object-storage-live.sh` (make it executable; follow the shell style: `${var}` form, `_lower` locals, UPPER only for exported env):

```bash
#!/usr/bin/env bash
# Live round trip for `p export/list/import object` against a real S3
# endpoint. With no arguments it starts a MinIO container, creates a
# bucket, runs the ignored live test, and tears the container down.
# Point it at an existing endpoint instead with the environment:
#
#   TOOLPATH_S3_TEST_BUCKET=my-bucket AWS_ACCESS_KEY_ID=… AWS_SECRET_ACCESS_KEY=… \
#     [TOOLPATH_S3_TEST_ENDPOINT=https://…] scripts/test-object-storage-live.sh --existing
#
# Preconditions for the MinIO path: `docker` and `aws` on PATH.

set -euo pipefail

_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "${_root}"

if [[ "${1:-}" == "--existing" ]]; then
    : "${TOOLPATH_S3_TEST_BUCKET:?set TOOLPATH_S3_TEST_BUCKET}"
    : "${AWS_ACCESS_KEY_ID:?set AWS_ACCESS_KEY_ID}"
    : "${AWS_SECRET_ACCESS_KEY:?set AWS_SECRET_ACCESS_KEY}"
    cargo test -p path-cli --test object_storage live_s3_round_trip -- --ignored --nocapture
    exit 0
fi

command -v docker >/dev/null || { echo "docker not on PATH" >&2; exit 64; }
command -v aws >/dev/null || { echo "aws CLI not on PATH (needed to create the bucket)" >&2; exit 64; }

_container="toolpath-minio-$$"
_port=9000
docker run -d --rm --name "${_container}" -p "${_port}:9000" \
    -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
    minio/minio server /data >/dev/null
trap 'docker stop "${_container}" >/dev/null 2>&1 || true' EXIT

export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin
export AWS_REGION=us-east-1
export TOOLPATH_S3_TEST_ENDPOINT="http://127.0.0.1:${_port}"
export TOOLPATH_S3_TEST_BUCKET="toolpath-live"

for _attempt in $(seq 1 30); do
    if curl -sf "${TOOLPATH_S3_TEST_ENDPOINT}/minio/health/live" >/dev/null; then
        break
    fi
    sleep 1
done
aws --endpoint-url "${TOOLPATH_S3_TEST_ENDPOINT}" s3 mb "s3://${TOOLPATH_S3_TEST_BUCKET}" >/dev/null

cargo test -p path-cli --test object_storage live_s3_round_trip -- --ignored --nocapture
```

- [ ] **Step 4: Run it if Docker is available; otherwise shellcheck it**

Run: `shellcheck scripts/test-object-storage-live.sh && chmod +x scripts/test-object-storage-live.sh`
Expected: clean. If `docker` is present: `scripts/test-object-storage-live.sh` → the live test passes. If not, note in the commit message body that the MinIO path was not exercised on this machine.

- [ ] **Step 5: Commit**

```bash
git add crates/path-cli/tests/object_storage.rs scripts/test-object-storage-live.sh
git commit -m "test: opt-in live S3 round trip and a MinIO runner script"
```

---

## Final gate (after Task 19)

Run, in order, and fix anything red before opening the PR for review:

```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace
cargo test -p path-cli --features resume-remote
cargo clippy --workspace --all-targets -- -D warnings
cargo build --manifest-path crates/toolpath-cli/Cargo.toml
./scripts/build-wasm.sh    # the emscripten build must still compile
cd site && pnpm run build && cd ..
```

Then re-run the three-persona audit workflow (`Workflow` run `wf_c9b2fd86-170`'s script, with a fresh run) against the branch and compare its findings to the report at `docs/superpowers/specs/2026-09-14-object-storage-usability-design.md`'s source audit: every "fix before merge" item should be gone.
