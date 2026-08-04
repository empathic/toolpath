# Listing cache — instant warm picker gathers

**Status:** Implemented
**Date:** 2026-08-04

## Goal

Issue #158: the pre-picker gather (`path share`, and bare `path resume`
once #154 lands) re-scans every session file end to end on every
invocation just to rebuild the same `ArtifactRow` metadata (title, cwd,
last activity, message count). #156 and #157 got a cold gather to
~2.5 s; this spec makes the *warm* gather — nothing changed since last
time — effectively instant by caching listing metadata keyed by the
sync machinery's existing stat stamps.

`p cache sync` already proves "nothing changed" in milliseconds without
reading session bodies: each `ArtifactRef` carries an mtime+size
fingerprint (claude: whole-chain stamp; codex: rollout file stat;
opencode/cursor: DB row updated-at). The listing cache reuses exactly
that enumeration and stamping, and stores the picker-row fields
alongside the stamp.

## Decisions Locked In

| # | Decision | Choice |
|---|----------|--------|
| 1 | Where the metadata lives | Sidecar file `$CONFIG_DIR/listing-cache.json` (respects `$TOOLPATH_CONFIG_DIR`), **not** extra fields on sync-manifest records. 0600 perms, atomic temp+rename writes. |
| 2 | Failure semantics | It is a CACHE: corrupt / missing / unreadable / wrong version → treated as empty, never an error, never blocks the picker. |
| 3 | Locking | None. Last-writer-wins is fine for a cache — the worst case is a redundant re-scan on the next gather. (The sync manifest keeps its advisory lock; this file deliberately does not copy it.) |
| 4 | Schema | Top-level `version` (bump to invalidate), then per artifact-type a map of artifact id → `{ modified?, size?, row }` where `row` = `{ path?, cwd?, session_id, title, last_activity?, message_count? }`. Stamps serialize the same way the sync manifest's do. |
| 5 | `matches_cwd` | NOT cached — it depends on the caller's cwd. Recomputed per gather from the cached `path`/`cwd` fields with the same canonicalized-path matching the collectors use. |
| 6 | Enumeration | Reuses the `ArtifactSource` machinery from `sync/sources.rs` (per-provider constructors exposed for the collectors); the engine does not grow a second enumeration path. |
| 7 | Stamp match rule | Same as sync's `is_unchanged`: at least one of `modified`/`size` must be `Some`, and both must equal the enumerated ref's. All-`None` stamps never vouch. |
| 8 | Claude keying | Cache key = chain head id; stamp = the same whole-chain stamp `claude_chain_stamp` computes, so an append to *any* segment of a chain invalidates that chain's entry. |
| 9 | Filters | `harness_filter` limits which providers consult the cache at all; `project_filter` is applied AFTER cache reconstruction (the cache is filter-agnostic — a filtered gather still warms it for everyone). |
| 10 | Eviction | Enumeration is authoritative: entries whose artifact no longer appears in the enumeration are dropped from the cache and produce no row — matching sync's self-heal semantics. |
| 11 | Write-back | The refreshed section replaces the provider's old one only when something actually changed; a fully-warm gather performs no write. |
| 12 | Correctness gate | Cached-row reconstruction must produce field-for-field identical picker rows to a fresh scan, in the same order. Tests: cold-vs-warm equality, append-invalidation, chain-rotation invalidation, deletion drop, corrupt-cache tolerance. |
| 13 | Cached providers | claude, codex, opencode — the three expensive scans. gemini / pi / copilot / cursor stay on the direct scan (all sub-second); adding one later is one `collect_*_cached` call site. |
| 14 | Picker behavior | Unchanged: same ranking, same row formatting, same `gather_artifacts` signature. Callers get the cache transparently. |

## Flow per cached provider

1. Enumerate `ArtifactRef`s stat-level (milliseconds, no session bodies).
2. For each ref whose stamp matches its cache entry, rebuild the
   `ArtifactRow` from the cached fields (recomputing `matches_cwd`
   against the canonical cwd).
3. Only new/changed refs get the expensive metadata scan:
   - **claude**: `read_conversation_metadata(project, head)` per missed
     chain (chain-aware, same read the old full scan looped over);
   - **codex**: one lazy `list_rollout_files()` walk builds an id→path
     map on the first miss, then `io().read_metadata(path)` per miss;
   - **opencode**: metadata is one DB pass, so the first miss triggers
     the full `list_session_metadata(None)` scan once and later misses
     read from it.
4. Rows are re-ordered to match the fresh scan exactly: claude sorts by
   descending `last_activity` within each project run (enumeration is
   project-major, like the scan); codex/opencode sort globally by
   descending `last_activity` (stable, so provider listing order breaks
   ties exactly as before).
5. `project_filter` is applied, rows are appended in the usual provider
   order, and the global (matches_cwd, last_activity) sort runs as
   today.
6. The refreshed section is written back only if it differs.

## Ordering caveat

Byte-identical warm-vs-cold ordering relies on the enumeration listing
artifacts in the same relative order as the metadata scan (it does:
both walk the same directory listings / run the same `ORDER BY`).
Rows with *identical* `last_activity` inside one provider are ordered
by that shared listing order, so ties resolve the same on both paths.

## Alternatives considered

Progressive hydration (stream rows into the picker) — rejected in the
issue: the ranking contract needs a global sort, so rows would visibly
reshuffle, and it masks the cost instead of removing it.

Manifest-record extension — rejected: keeps the manifest schema stable,
and the manifest's locked read-merge-save discipline is overkill for
data that can be regenerated by one scan.
