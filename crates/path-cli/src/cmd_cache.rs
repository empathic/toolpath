//! `path p cache ls | rm | sync` — make the document cache legible.
//! The store itself lives in [`crate::cache`]; the sync engine in
//! [`crate::sync`]. Sync UI (the live progress line, the summary)
//! lives here: the engine reports through [`SyncObserver`], the
//! binary draws.

use anyhow::Result;
use clap::Subcommand;

use crate::cache::{list_cached, remove_cached};
#[cfg(not(target_os = "emscripten"))]
use crate::{
    artifact::{ArtifactRef, ArtifactType},
    harness::HarnessBundle,
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
    },
}

pub fn run(op: CacheOp) -> Result<()> {
    match op {
        CacheOp::Ls => run_ls(),
        CacheOp::Rm { id } => run_rm(&id),
        #[cfg(not(target_os = "emscripten"))]
        CacheOp::Sync { types } => run_sync(types),
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
    // The key exists only to keep this document's fingerprints stable
    // across re-redactions; with the document gone nothing can use it.
    if let Err(e) = crate::cache::remove_redact_key(id) {
        eprintln!("warning: redaction key not removed: {e}");
    }
    // The artifact is still real — downgrade its manifest record to
    // "known, not cached" so the next sync can re-materialize it.
    #[cfg(not(target_os = "emscripten"))]
    if let Err(e) = crate::sync::evict_cache_id(id) {
        eprintln!("warning: sync manifest not updated: {e}");
    }
    eprintln!("Removed {id}");
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn run_sync(types: Vec<ArtifactType>) -> Result<()> {
    let explicit = !types.is_empty();
    let types = resolve_types(&types);
    let bundle = HarnessBundle::from_environment();
    let outcomes = sync_bundle(&bundle, &types, &mut Progress::new())?;
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
        s.push('\n');
    }
    s.push_str(&render_replay(outcomes));
    if s.is_empty() {
        s.push_str("nothing to sync\n");
    }
    s
}

/// Redaction replay, across every type in the run. Reappeared skips get
/// their own clause because they are the one way a replayed document
/// differs from what the user approved: a hand-picked skip cannot be
/// replayed against content that has moved, so it comes back.
#[cfg(not(target_os = "emscripten"))]
fn render_replay(outcomes: &[(ArtifactType, SyncOutcome)]) -> String {
    let documents: usize = outcomes.iter().map(|(_, o)| o.re_redacted).sum();
    if documents == 0 {
        return String::new();
    }
    let reappeared: usize = outcomes.iter().map(|(_, o)| o.reappeared_skips).sum();
    let mut line = format!("re-redacted {documents} {}", plural(documents, "document"));
    if reappeared > 0 {
        line.push_str(&format!(
            "; {reappeared} previously-skipped {} reappeared",
            plural(reappeared, "finding")
        ));
    }
    line.push('\n');
    line
}

#[cfg(not(target_os = "emscripten"))]
fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        noun.to_string()
    } else {
        format!("{noun}s")
    }
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
                    ..Default::default()
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
                unchanged: 1,
                failed: 2,
                ..Default::default()
            },
        )];
        let s = render_summary(&outcomes, false);
        assert!(s.contains("2 failed"));

        assert_eq!(render_summary(&[], false), "nothing to sync\n");
    }

    #[test]
    fn render_summary_reports_replay_across_types() {
        let outcomes = vec![
            (
                ArtifactType::Claude,
                SyncOutcome {
                    updated: 2,
                    re_redacted: 2,
                    reappeared_skips: 2,
                    ..Default::default()
                },
            ),
            (
                ArtifactType::Codex,
                SyncOutcome {
                    updated: 1,
                    re_redacted: 1,
                    ..Default::default()
                },
            ),
        ];
        assert!(
            render_summary(&outcomes, false)
                .ends_with("re-redacted 3 documents; 2 previously-skipped findings reappeared\n")
        );
    }

    #[test]
    fn render_replay_is_silent_without_replay_and_singular_for_one() {
        assert_eq!(
            render_replay(&[(ArtifactType::Claude, SyncOutcome::default())]),
            ""
        );
        let one = vec![(
            ArtifactType::Claude,
            SyncOutcome {
                re_redacted: 1,
                reappeared_skips: 1,
                ..Default::default()
            },
        )];
        assert_eq!(
            render_replay(&one),
            "re-redacted 1 document; 1 previously-skipped finding reappeared\n"
        );
    }
}
