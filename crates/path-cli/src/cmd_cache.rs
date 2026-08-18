//! `path p cache ls | rm | sync` — make the document cache legible.
//! The store itself lives in [`crate::cache`]; the sync engine in
//! [`crate::sync`]. Sync UI (the live progress line, the summary)
//! lives here: the engine reports through [`SyncObserver`], the
//! binary draws.

use anyhow::Result;
use clap::Subcommand;
#[cfg(not(target_os = "emscripten"))]
use std::path::PathBuf;

use crate::cache::{list_cached, remove_cached};
use crate::config::Config;
#[cfg(not(target_os = "emscripten"))]
use crate::{
    artifact::{ArtifactRef, ArtifactType},
    providers,
    sync::{SyncObserver, SyncOutcome, sync_bundle},
};

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
    /// or changed since the last sync (tracked in `$CONFIG_DIR/manifest.json`)
    #[cfg(not(target_os = "emscripten"))]
    Sync {
        /// Artifact types to sync (default: every agent harness)
        #[arg(value_enum)]
        types: Vec<crate::artifact::ArtifactType>,

        /// Only ingest sessions whose project directory (recorded
        /// working directory) lies under this directory (subtree
        /// match). Out-of-scope artifacts are noted in the manifest but
        /// not derived.
        #[arg(long)]
        project_under: Option<PathBuf>,
    },
}

pub fn run(op: CacheOp, config: &Config) -> Result<()> {
    match op {
        CacheOp::Ls => run_ls(config),
        CacheOp::Rm { id } => run_rm(&id, config),
        #[cfg(not(target_os = "emscripten"))]
        CacheOp::Sync {
            types,
            project_under,
        } => run_sync(types, project_under, config),
    }
}

fn run_ls(config: &Config) -> Result<()> {
    let entries = list_cached(config)?;
    if entries.is_empty() {
        eprintln!("No cached documents. Run `path import <source>` to create one.");
        return Ok(());
    }
    for e in entries {
        println!("{}\t{}\t{}", e.id, e.bytes, e.path.display());
    }
    Ok(())
}

fn run_rm(id: &str, config: &Config) -> Result<()> {
    remove_cached(config, id)?;
    // The artifact is still real — downgrade its manifest record to
    // "known, not cached" so the next sync can re-materialize it.
    #[cfg(not(target_os = "emscripten"))]
    if let Err(e) = config
        .config_dir()
        .and_then(|dir| crate::sync::evict_cache_id(&dir, id))
    {
        eprintln!("warning: sync manifest not updated: {e}");
    }
    eprintln!("Removed {id}");
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn run_sync(
    types: Vec<ArtifactType>,
    project_under: Option<PathBuf>,
    config: &Config,
) -> Result<()> {
    let explicit = !types.is_empty();
    let types = resolve_types(&types);
    let bundle = providers::harness_bundle(config);
    let outcomes = sync_bundle(
        config,
        &bundle,
        &types,
        project_under.as_deref(),
        &mut Progress::new(),
    )?;
    eprint!("{}", render_summary(&outcomes, explicit));
    Ok(())
}

/// Explicit args → dedup'd type list; no args → every type.
#[cfg(not(target_os = "emscripten"))]
fn resolve_types(args: &[ArtifactType]) -> Vec<ArtifactType> {
    if args.is_empty() {
        return ArtifactType::ALL.to_vec();
    }
    let mut out: Vec<ArtifactType> = Vec::with_capacity(args.len());
    for &t in args {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Live sync progress on stderr: a `\r`-updating `<type> done/total`
/// line on a terminal, a plain line every 25 items otherwise. Only
/// artifacts needing work count toward the total — a no-op sync
/// draws nothing.
#[cfg(not(target_os = "emscripten"))]
struct Progress {
    label: String,
    total: usize,
    done: usize,
    tty: bool,
}

#[cfg(not(target_os = "emscripten"))]
impl Progress {
    fn new() -> Self {
        use std::io::IsTerminal;
        Self {
            label: String::new(),
            total: 0,
            done: 0,
            tty: std::io::stderr().is_terminal(),
        }
    }

    fn line(&self) -> String {
        format!("{} {}/{}", self.label, self.done, self.total)
    }

    fn draw(&self) {
        if self.total > 0 && self.tty {
            eprint!("\r{}", self.line());
        }
    }

    /// Clear the live line so a warning or summary prints clean;
    /// the next `tick` redraws in full.
    fn interrupt(&self) {
        if self.total > 0 && self.tty {
            eprint!("\r\x1b[2K");
        }
    }
}

#[cfg(not(target_os = "emscripten"))]
impl SyncObserver for Progress {
    fn begin(&mut self, artifact_type: ArtifactType, pending: usize) {
        self.label = artifact_type.padded_name();
        self.total = pending;
        self.done = 0;
        self.draw();
    }

    fn tick(&mut self) {
        if self.total == 0 {
            return;
        }
        self.done += 1;
        if self.tty {
            self.draw();
        } else if self.done.is_multiple_of(25) {
            eprintln!("{}", self.line());
        }
    }

    fn failed(&mut self, artifact: &ArtifactRef, error: &anyhow::Error) {
        self.interrupt();
        eprintln!(
            "warning: sync {}: {}: {error}",
            artifact.artifact_type.name(),
            artifact.id
        );
    }

    fn end(&mut self) {
        self.interrupt();
    }
}

/// One stderr line per artifact type. Types the user didn't name
/// are shown only when they had artifacts, so a default run doesn't
/// list every uninstalled provider.
#[cfg(not(target_os = "emscripten"))]
fn render_summary(outcomes: &[(ArtifactType, SyncOutcome)], explicit: bool) -> String {
    let mut s = String::new();
    for (artifact_type, o) in outcomes {
        if o.total() == 0 && !explicit {
            continue;
        }
        s.push_str(&format!(
            "{} {} new, {} updated, {} unchanged",
            artifact_type.padded_name(),
            o.new,
            o.updated,
            o.unchanged
        ));
        if o.failed > 0 {
            s.push_str(&format!(", {} failed", o.failed));
        }
        if o.out_of_scope > 0 {
            s.push_str(&format!(", {} out of scope", o.out_of_scope));
        }
        s.push('\n');
    }
    if s.is_empty() {
        s.push_str("nothing to sync\n");
    }
    s
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::*;

    #[test]
    fn progress_line_counts_only_pending_work() {
        let mut progress = Progress {
            label: ArtifactType::Claude.padded_name(),
            total: 3,
            done: 0,
            tty: false,
        };
        progress.tick();
        progress.tick();
        assert_eq!(progress.line(), "claude   2/3");
    }

    #[test]
    fn resolve_types_defaults_to_all_and_dedups() {
        assert_eq!(resolve_types(&[]), ArtifactType::ALL.to_vec());
        assert_eq!(
            resolve_types(&[
                ArtifactType::Codex,
                ArtifactType::Claude,
                ArtifactType::Codex
            ]),
            vec![ArtifactType::Codex, ArtifactType::Claude]
        );
    }

    #[test]
    fn render_summary_hides_empty_types_unless_explicit() {
        let outcomes = vec![
            (
                ArtifactType::Claude,
                SyncOutcome {
                    new: 2,
                    updated: 1,
                    unchanged: 3,
                    failed: 0,
                    out_of_scope: 0,
                },
            ),
            (ArtifactType::Cursor, SyncOutcome::default()),
        ];
        let default_run = render_summary(&outcomes, false);
        assert!(default_run.contains("claude"));
        assert!(!default_run.contains("cursor"));

        let explicit_run = render_summary(&outcomes, true);
        assert!(explicit_run.contains("cursor"));
    }

    #[test]
    fn render_summary_shows_failures_and_empty_case() {
        let outcomes = vec![(
            ArtifactType::Codex,
            SyncOutcome {
                new: 0,
                updated: 0,
                unchanged: 1,
                failed: 2,
                out_of_scope: 0,
            },
        )];
        let s = render_summary(&outcomes, false);
        assert!(s.contains("2 failed"));

        assert_eq!(render_summary(&[], false), "nothing to sync\n");
    }
}
