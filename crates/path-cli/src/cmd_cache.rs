//! `path p cache ls | rm` — make the document cache legible. The
//! store itself lives in [`crate::cache`].

use anyhow::Result;
use clap::Subcommand;

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
}

pub fn run(op: CacheOp) -> Result<()> {
    match op {
        CacheOp::Ls => run_ls(),
        CacheOp::Rm { id } => run_rm(&id),
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
    eprintln!("Removed {id}");
    Ok(())
}
