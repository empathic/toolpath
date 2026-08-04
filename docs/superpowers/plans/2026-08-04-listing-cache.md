# Listing cache — implementation plan

Spec: `docs/superpowers/specs/2026-08-04-listing-cache-design.md`
(issue #158; branch `bryan/listing-cache`, based on
`bryan/parallel-gather`).

## Task 1: Spec + plan

- [x] Write the design spec with the locked-in decisions table
- [x] Write this plan

## Task 2: `listing_cache` module

- [x] `crates/path-cli/src/listing_cache.rs`: `CachedRow`,
      `CachedListing` (stamp + row), `ListingCache` (load / section /
      replace_section / save_if_dirty), version field, atomic 0600
      write, corrupt-or-missing → empty
- [x] `LISTING_CACHE_FILE_NAME` constant in `config.rs`
- [x] Unit tests: roundtrip, version mismatch → empty, corrupt →
      empty, clean replace is not dirty, 0600 perms

## Task 3: Cache-backed gather for claude / codex / opencode

- [x] Expose per-provider `ArtifactSource` constructors in
      `sync/sources.rs` (`claude_source` / `codex_source` /
      `opencode_source`); `source_for` delegates to them
- [x] `cmd_share.rs`: shared `collect_with_cache` loop (partition by
      stamp, rebuild hits, scan misses, rebuild ordering, apply
      project filter, return refreshed section)
- [x] Replace `collect_claude` / `collect_codex` / `collect_opencode`
      with cache-aware versions; single-session row builders shared
      between hit and miss paths
- [x] `gather_artifacts`: load cache once, thread sections through the
      scoped-thread fan-out, replace sections + `save_if_dirty` after
      joining
- [x] Pin `$TOOLPATH_CONFIG_DIR` in the existing gather unit tests so
      they stop touching the real `~/.toolpath`

## Task 4: Correctness-gate tests

- [x] Cold gather == warm gather, field-for-field (`ArtifactRow` gains
      `PartialEq`)
- [x] Warm gather actually reads the cache (tampered cached title
      surfaces on a stamp hit)
- [x] Append to a claude session file invalidates and refreshes the
      row (message count grows)
- [x] Claude chain rotation: append lands in a successor segment; the
      chain-head row refreshes (design decision 8)
- [x] Codex rollout append invalidates and refreshes
- [x] Deleted artifact drops its row and its cache entry
- [x] Corrupt listing cache is ignored and gather still returns fresh
      rows
- [x] `project_filter` on a warm gather matches the cold filtered rows

## Task 5: Version bump + changelog

- [x] path-cli 0.16.2 → 0.16.3 in `crates/path-cli/Cargo.toml`, root
      `Cargo.toml` workspace deps, `site/_data/crates.json`
- [x] CHANGELOG.md new H2 at top, dated 2026-08-04

## Task 6: Gates

- [x] `RUSTFLAGS="-D warnings" cargo test -p path-cli`
- [x] `cargo clippy --workspace -- -D warnings`
- [x] `cargo fmt -p path-cli -- --check`
- [x] `scripts/quality_gates.sh format clippy test doc examples plugin`
      (report, don't fix, any format drift in untouched files)
