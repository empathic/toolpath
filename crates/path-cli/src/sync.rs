//! `path p cache sync` — incremental ingestion of artifacts into the
//! document cache.
//!
//! Sync enumerates artifacts across the requested types (today all six
//! are agent-session providers), compares each against the sync
//! manifest at `$CONFIG_DIR/sync.json`, and derives + caches only what
//! is new or changed. Change detection is stat-level: the fingerprint
//! is the source file's mtime + size (or the database row's updated-at
//! for the SQLite-backed providers), so deciding "nothing changed"
//! never reads session bodies. Artifacts deleted upstream keep both
//! their cache document and their manifest record.

#[cfg(not(target_os = "emscripten"))]
pub(crate) use engine::*;

#[cfg(not(target_os = "emscripten"))]
mod engine;

#[cfg(not(target_os = "emscripten"))]
pub(crate) mod sources;
