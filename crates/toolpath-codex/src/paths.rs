//! Filesystem layout for Codex CLI state.
//!
//! Sessions live at `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`.
//! Unlike Claude and Gemini, Codex sessions aren't keyed by project
//! path — they're global, bucketed by date. A session is identified
//! by its UUIDv7 (`session_meta.id`) or equivalently by its filename
//! stem (`rollout-<timestamp>-<uuid>`).

use crate::error::{ConvoError, Result};
use std::fs;
use std::path::{Path, PathBuf};

const SESSIONS_SUBDIR: &str = "sessions";
const HISTORY_FILE: &str = "history.jsonl";
const LOG_FILE: &str = "log/codex-tui.log";

/// Builder-style resolver over the `~/.codex/` filesystem.
#[derive(Debug, Clone)]
pub struct PathResolver {
    home_dir: Option<PathBuf>,
    codex_dir: Option<PathBuf>,
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PathResolver {
    pub fn new() -> Self {
        Self {
            home_dir: dirs::home_dir(),
            codex_dir: None,
        }
    }

    pub fn with_home<P: Into<PathBuf>>(mut self, home: P) -> Self {
        self.home_dir = Some(home.into());
        self
    }

    /// Override the codex directory directly (defaults to `~/.codex`).
    pub fn with_codex_dir<P: Into<PathBuf>>(mut self, codex_dir: P) -> Self {
        self.codex_dir = Some(codex_dir.into());
        self
    }

    pub fn home_dir(&self) -> Result<&Path> {
        self.home_dir.as_deref().ok_or(ConvoError::NoHomeDirectory)
    }

    pub fn codex_dir(&self) -> Result<PathBuf> {
        if let Some(d) = &self.codex_dir {
            return Ok(d.clone());
        }
        Ok(self.home_dir()?.join(".codex"))
    }

    pub fn sessions_root(&self) -> Result<PathBuf> {
        Ok(self.codex_dir()?.join(SESSIONS_SUBDIR))
    }

    pub fn history_file(&self) -> Result<PathBuf> {
        Ok(self.codex_dir()?.join(HISTORY_FILE))
    }

    pub fn log_file(&self) -> Result<PathBuf> {
        Ok(self.codex_dir()?.join(LOG_FILE))
    }

    pub fn exists(&self) -> bool {
        self.codex_dir().map(|p| p.exists()).unwrap_or(false)
    }

    /// Enumerate every `rollout-*.jsonl` file under `sessions/`, newest
    /// first by file mtime.
    pub fn list_rollout_files(&self) -> Result<Vec<PathBuf>> {
        let root = self.sessions_root()?;
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        walk_for_rollouts(&root, &mut files)?;
        // Sort newest first by mtime.
        files.sort_by_key(|p| {
            fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| std::cmp::Reverse(d.as_secs()))
                .unwrap_or(std::cmp::Reverse(0))
        });
        Ok(files)
    }

    /// Resolve a session identifier to a rollout file path.
    ///
    /// Accepts:
    /// - A full filename stem: `rollout-2026-04-20T12-43-30-019dabc6-8fef-7681-a054-b5bb75fcb97d`
    ///   (resolved without walking the tree — the stem's date names its
    ///   `YYYY/MM/DD` bucket)
    /// - A bare session UUID (suffix match): `019dabc6-8fef-7681-a054-b5bb75fcb97d`
    /// - A short UUID prefix: `019dabc6` (resolves if unique)
    pub fn find_rollout_file(&self, session_id: &str) -> Result<PathBuf> {
        if let Some(direct) = self.rollout_file_for_stem(session_id)? {
            return Ok(direct);
        }
        let all = self.list_rollout_files()?;
        // Exact filename stem match first.
        for p in &all {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str())
                && stem == session_id
            {
                return Ok(p.clone());
            }
        }
        // UUID suffix match (full or partial prefix).
        let matches: Vec<&PathBuf> = all
            .iter()
            .filter(|p| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.contains(session_id))
                    .unwrap_or(false)
            })
            .collect();
        match matches.len() {
            0 => Err(ConvoError::SessionNotFound(session_id.to_string())),
            1 => Ok(matches[0].clone()),
            _ => Err(ConvoError::SessionNotFound(format!(
                "{} (ambiguous — {} matches)",
                session_id,
                matches.len()
            ))),
        }
    }

    /// Direct lookup for a full filename stem: `rollout-<YYYY-MM-DD>T…`
    /// lives at `sessions/YYYY/MM/DD/<stem>.jsonl`. `None` when the id
    /// isn't stem-shaped or the file isn't at its dated path (the caller
    /// falls back to the tree walk).
    fn rollout_file_for_stem(&self, session_id: &str) -> Result<Option<PathBuf>> {
        let Some(date) = session_id
            .strip_prefix("rollout-")
            .and_then(|r| r.get(..10))
        else {
            return Ok(None);
        };
        let parts: Vec<&str> = date.split('-').collect();
        let [y, m, d] = parts.as_slice() else {
            return Ok(None);
        };
        if y.len() != 4 || m.len() != 2 || d.len() != 2 {
            return Ok(None);
        }
        let candidate = self
            .sessions_root()?
            .join(y)
            .join(m)
            .join(d)
            .join(format!("{session_id}.jsonl"));
        Ok(candidate.is_file().then_some(candidate))
    }
}

/// Recursively collect `rollout-*.jsonl` files under `root`.
fn walk_for_rollouts(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            walk_for_rollouts(&path, out)?;
        } else if ft.is_file()
            && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("rollout-"))
                .unwrap_or(false)
        {
            out.push(path);
        }
    }
    Ok(())
}

mod dirs {
    use std::env;
    use std::path::PathBuf;

    pub fn home_dir() -> Option<PathBuf> {
        env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathResolver) {
        let temp = TempDir::new().unwrap();
        let codex = temp.path().join(".codex");
        fs::create_dir_all(&codex).unwrap();
        let resolver = PathResolver::new()
            .with_home(temp.path())
            .with_codex_dir(&codex);
        (temp, resolver)
    }

    #[test]
    fn codex_dir_defaults_to_home() {
        let temp = TempDir::new().unwrap();
        let r = PathResolver::new().with_home(temp.path());
        assert_eq!(r.codex_dir().unwrap(), temp.path().join(".codex"));
    }

    #[test]
    fn sessions_root_under_codex_dir() {
        let (_t, r) = setup();
        assert!(r.sessions_root().unwrap().ends_with(".codex/sessions"));
    }

    #[test]
    fn list_rollouts_walks_date_tree() {
        let (_t, r) = setup();
        let day = r.sessions_root().unwrap().join("2026/04/20");
        fs::create_dir_all(&day).unwrap();
        fs::write(day.join("rollout-2026-04-20T10-00-00-aaa.jsonl"), "{}").unwrap();
        fs::write(day.join("rollout-2026-04-20T11-00-00-bbb.jsonl"), "{}").unwrap();
        // Non-rollout file is ignored.
        fs::write(day.join("other.jsonl"), "{}").unwrap();
        // Non-jsonl rollout file is ignored.
        fs::write(day.join("rollout-2026-04-20T12-00-00-ccc.txt"), "{}").unwrap();

        let files = r.list_rollout_files().unwrap();
        assert_eq!(files.len(), 2);
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.iter().any(|n| n.contains("aaa")));
        assert!(names.iter().any(|n| n.contains("bbb")));
    }

    #[test]
    fn list_rollouts_empty_when_no_sessions() {
        let (_t, r) = setup();
        assert!(r.list_rollout_files().unwrap().is_empty());
    }

    #[test]
    fn find_rollout_by_full_stem() {
        let (_t, r) = setup();
        let day = r.sessions_root().unwrap().join("2026/04/20");
        fs::create_dir_all(&day).unwrap();
        let stem = "rollout-2026-04-20T10-00-00-abc-xyz";
        fs::write(day.join(format!("{}.jsonl", stem)), "{}").unwrap();
        let p = r.find_rollout_file(stem).unwrap();
        assert_eq!(p.file_stem().unwrap(), stem);
    }

    #[test]
    fn find_rollout_by_uuid_suffix() {
        let (_t, r) = setup();
        let day = r.sessions_root().unwrap().join("2026/04/20");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("rollout-2026-04-20T10-00-00-019dabc6-8fef-7681-a054-b5bb75fcb97d.jsonl"),
            "{}",
        )
        .unwrap();
        let p = r
            .find_rollout_file("019dabc6-8fef-7681-a054-b5bb75fcb97d")
            .unwrap();
        assert!(
            p.to_string_lossy()
                .contains("019dabc6-8fef-7681-a054-b5bb75fcb97d")
        );
    }

    #[test]
    fn find_rollout_by_short_prefix() {
        let (_t, r) = setup();
        let day = r.sessions_root().unwrap().join("2026/04/20");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("rollout-2026-04-20T10-00-00-019dabc6-unique.jsonl"),
            "{}",
        )
        .unwrap();
        let p = r.find_rollout_file("019dabc6-unique").unwrap();
        assert!(p.exists());
    }

    /// A stem whose date doesn't match the directory it sits in misses
    /// the direct dated-path lookup and must still resolve via the walk.
    #[test]
    fn find_rollout_stem_in_mismatched_date_dir_falls_back_to_walk() {
        let (_t, r) = setup();
        let day = r.sessions_root().unwrap().join("2026/04/21");
        fs::create_dir_all(&day).unwrap();
        let stem = "rollout-2026-04-20T10-00-00-abc-xyz";
        fs::write(day.join(format!("{}.jsonl", stem)), "{}").unwrap();
        let p = r.find_rollout_file(stem).unwrap();
        assert_eq!(p.file_stem().unwrap(), stem);
    }

    #[test]
    fn find_rollout_missing_errors() {
        let (_t, r) = setup();
        let err = r.find_rollout_file("does-not-exist").unwrap_err();
        assert!(matches!(err, ConvoError::SessionNotFound(_)));
    }

    #[test]
    fn find_rollout_ambiguous_prefix_errors() {
        let (_t, r) = setup();
        let day = r.sessions_root().unwrap().join("2026/04/20");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("rollout-2026-04-20T10-00-00-019dabc6-a.jsonl"),
            "{}",
        )
        .unwrap();
        fs::write(
            day.join("rollout-2026-04-20T11-00-00-019dabc6-b.jsonl"),
            "{}",
        )
        .unwrap();
        let err = r.find_rollout_file("019dabc6").unwrap_err();
        assert!(matches!(err, ConvoError::SessionNotFound(_)));
    }

    #[test]
    fn history_and_log_file_paths() {
        let (t, r) = setup();
        assert_eq!(
            r.history_file().unwrap(),
            t.path().join(".codex/history.jsonl")
        );
        assert_eq!(
            r.log_file().unwrap(),
            t.path().join(".codex/log/codex-tui.log")
        );
    }

    #[test]
    fn exists_reflects_codex_dir() {
        let (_t, r) = setup();
        assert!(r.exists());
        let missing = PathResolver::new().with_codex_dir("/never/exists");
        assert!(!missing.exists());
    }
}
