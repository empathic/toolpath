//! `path p cache ls | rm` — make the document cache legible. The
//! store itself lives in [`crate::cache`].

use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;

use crate::cache::{list_cached, remove_cached};

#[derive(Subcommand, Debug)]
pub enum CacheOp {
    /// List cached documents (newest first)
    Ls,
    /// Remove a cached document by id
    Rm {
        /// Cache id (filename without `.json`)
        id: String,
    },
    /// Ingest agent sessions into the cache, deriving only what is new
    /// or changed since the last sync (tracked in `$CONFIG_DIR/sync.json`)
    #[cfg(not(target_os = "emscripten"))]
    Sync {
        /// Artifact types to sync (default: every agent harness)
        #[arg(value_enum)]
        types: Vec<crate::artifact::ArtifactType>,

        /// Only ingest artifacts living under this directory (subtree
        /// match). Out-of-scope artifacts are noted in the manifest but
        /// not derived.
        #[arg(long, short = 'd')]
        parent_dir: Option<PathBuf>,
    },
}

pub fn run(op: CacheOp) -> Result<()> {
    match op {
        CacheOp::Ls => run_ls(),
        CacheOp::Rm { id } => run_rm(&id),
        #[cfg(not(target_os = "emscripten"))]
        CacheOp::Sync { types, parent_dir } => crate::sync::run(types, parent_dir),
    }
}

fn run_ls() -> Result<()> {
    let entries = list_cached()?;
    if entries.is_empty() {
        eprintln!("No cached documents. Run `path import <source>` to create one.");
        return Ok(());
    }
    for e in entries {
        println!("{}\t{}\t{}", e.id, e.bytes, e.path.display());
    }
    Ok(())
}

fn run_rm(id: &str) -> Result<()> {
    remove_cached(id)?;
    // The artifact is still real — downgrade its manifest record to
    // "known, not cached" so the next sync can re-materialize it.
    #[cfg(not(target_os = "emscripten"))]
    if let Err(e) = crate::sync::evict_cache_id(id) {
        eprintln!("warning: sync manifest not updated: {e}");
    }
    eprintln!("Removed {id}");
    Ok(())
}
