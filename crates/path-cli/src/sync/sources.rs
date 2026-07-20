//! Per-provider artifact sources. [`ArtifactSource`] is the seam
//! between the sync engine and the providers: everything the engine
//! needs from one provider — enumerate its artifacts with stat-level
//! fingerprints, stat one artifact directly, derive one into a
//! document, peek at where one lives, compare directories in its key
//! space — sits behind the trait, one impl per provider. The engine
//! never matches on [`ArtifactType`]; provider details changing means
//! editing that provider's impl here, nothing else.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

use crate::artifact::{
    ArtifactRef, ArtifactType, claude_chain_stamp, codex_artifact_id, stat_stamp,
};
use crate::derive::{self, DerivedDoc};
use crate::harness::{
    HarnessBundle, is_not_found_claude, is_not_found_codex, is_not_found_cursor,
    is_not_found_gemini, is_not_found_opencode, is_not_found_pi,
};

/// A source's stat-level fingerprint for one artifact: mtime (file
/// providers) or updated-at (DB providers), plus file size — each
/// `None` when unavailable.
pub(crate) type Stamp = (Option<DateTime<Utc>>, Option<u64>);

/// One provider, as the sync engine sees it.
pub(crate) trait ArtifactSource {
    /// Enumerate this provider's artifacts with stat-level
    /// fingerprints, pruning to `parent_dir` when the provider's
    /// listing is path-keyed. Never reads session bodies. Listing
    /// errors warn and skip so one broken provider can't block a run.
    fn enumerate(&self, parent_dir: Option<&Path>) -> Vec<ArtifactRef>;

    /// Stat-level fingerprint for a single artifact, resolved
    /// directly — the same stat targets as [`Self::enumerate`], so the
    /// result compares against manifest records. `project` is required
    /// by the path-keyed providers (claude/gemini/pi).
    fn stamp(&self, project: Option<&str>, id: &str) -> Option<Stamp>;

    /// Derive one enumerated artifact into a cacheable document,
    /// through the same manager it was enumerated from, so listing and
    /// derivation always agree on provider roots.
    fn derive(&self, artifact: &ArtifactRef) -> Result<DerivedDoc>;

    /// Bounded peek at the directory an artifact lives in, for
    /// providers whose cheap listing doesn't carry it (the cwd lives
    /// inside the session file). The engine memoizes the answer into
    /// the manifest record, so this runs at most once per artifact.
    fn peek_dir(&self, _id: &str) -> Option<String> {
        None
    }

    /// Whether `dir` lies under `parent_dir` in this provider's key
    /// space. Providers whose keys are lossy dir encodings (claude,
    /// pi) compare in the encoded space instead of as real paths.
    fn in_scope(&self, dir: &str, parent_dir: &Path) -> bool {
        dir_in_scope(dir, parent_dir)
    }
}

/// The source for `t`'s artifacts, borrowing its manager from
/// `bundle`. `None` when the provider isn't in the bundle — and for
/// git, which is recorded by `p import` but never discovered: there is
/// no machine-wide registry of repos to enumerate.
pub(crate) fn source_for<'a>(
    bundle: &'a HarnessBundle,
    t: ArtifactType,
) -> Option<Box<dyn ArtifactSource + 'a>> {
    match t {
        ArtifactType::Claude => Some(Box::new(ClaudeSource(bundle.claude.as_ref()?))),
        ArtifactType::Gemini => Some(Box::new(GeminiSource(bundle.gemini.as_ref()?))),
        ArtifactType::Codex => Some(Box::new(CodexSource(bundle.codex.as_ref()?))),
        ArtifactType::Opencode => Some(Box::new(OpencodeSource(bundle.opencode.as_ref()?))),
        ArtifactType::Cursor => Some(Box::new(CursorSource(bundle.cursor.as_ref()?))),
        ArtifactType::Pi => Some(Box::new(PiSource(bundle.pi.as_ref()?))),
        ArtifactType::Copilot => Some(Box::new(CopilotSource(bundle.copilot.as_ref()?))),
        ArtifactType::Git => None,
    }
}

/// The project path a path-keyed artifact was enumerated under.
fn require_path(artifact: &ArtifactRef) -> Result<&str> {
    artifact
        .path
        .as_deref()
        .ok_or_else(|| anyhow!("artifact {} has no path", artifact.id))
}

fn canonicalize_or_self(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Subtree check for a real filesystem path. Canonicalizes both
/// sides, but also accepts the raw parent so a not-yet-resolvable
/// constraint (or an unresolvable dir) still matches literally.
fn dir_in_scope(dir: &str, parent_dir: &Path) -> bool {
    let d = canonicalize_or_self(Path::new(dir));
    d.starts_with(canonicalize_or_self(parent_dir)) || d.starts_with(parent_dir)
}

// ── claude ─────────────────────────────────────────────────────────

struct ClaudeSource<'a>(&'a toolpath_claude::ClaudeConvo);

impl ArtifactSource for ClaudeSource<'_> {
    /// Chain heads via `list_conversations` (bounded first-lines peek
    /// per file, no full parse). The head is the chain's *oldest*
    /// segment and its id is rotation-stable; the fingerprint covers
    /// the whole chain (see `claude_chain_stamp`) because appends land
    /// in the newest segment, not the head file.
    fn enumerate(&self, parent_dir: Option<&Path>) -> Vec<ArtifactRef> {
        let mut out = Vec::new();
        let projects = match self.0.list_projects() {
            Ok(ps) => ps,
            Err(e) if is_not_found_claude(&e) => return out,
            Err(e) => {
                eprintln!("warning: claude enumeration failed: {e}");
                return out;
            }
        };
        for project in projects {
            if let Some(parent_dir) = parent_dir
                && !claude_project_in_scope(&project, parent_dir)
            {
                continue;
            }
            let heads = match self.0.list_conversations(&project) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("warning: claude project {project} failed: {e}");
                    continue;
                }
            };
            for head in heads {
                let (modified, size) = claude_chain_stamp(self.0, &project, &head);
                out.push(ArtifactRef {
                    artifact_type: ArtifactType::Claude,
                    id: head,
                    path: Some(project.clone()),
                    modified,
                    size,
                });
            }
        }
        out
    }

    fn stamp(&self, project: Option<&str>, id: &str) -> Option<Stamp> {
        Some(claude_chain_stamp(self.0, project?, id))
    }

    fn derive(&self, artifact: &ArtifactRef) -> Result<DerivedDoc> {
        derive::derive_claude_session_with(self.0, require_path(artifact)?, &artifact.id)
    }

    fn in_scope(&self, dir: &str, parent_dir: &Path) -> bool {
        claude_project_in_scope(dir, parent_dir)
    }
}

/// Claude's project-dir slugs are lossy — '/', '_', and '.' all
/// became '-', and un-sanitizing only restores '/'. Comparing real
/// paths therefore misfilters any project containing '.' or '_';
/// compare in slug space instead, where '/' boundaries are '-'.
fn claude_project_in_scope(project: &str, parent_dir: &Path) -> bool {
    fn slug(s: &str) -> String {
        s.replace(['/', '_', '.'], "-")
    }
    let p = slug(project);
    [
        slug(&parent_dir.to_string_lossy()),
        slug(&canonicalize_or_self(parent_dir).to_string_lossy()),
    ]
    .iter()
    .any(|parent| p == *parent || p.starts_with(&format!("{parent}-")))
}

// ── gemini ─────────────────────────────────────────────────────────

struct GeminiSource<'a>(&'a toolpath_gemini::GeminiConvo);

impl ArtifactSource for GeminiSource<'_> {
    /// Session entries via a bounded identity peek (`toolpath-gemini`
    /// reads at most the first 4 KiB of a main file); the fingerprint
    /// stats the main file (or the orphan sub-agent directory).
    fn enumerate(&self, parent_dir: Option<&Path>) -> Vec<ArtifactRef> {
        let mut out = Vec::new();
        let projects = match self.0.list_projects() {
            Ok(ps) => ps,
            Err(e) if is_not_found_gemini(&e) => return out,
            Err(e) => {
                eprintln!("warning: gemini enumeration failed: {e}");
                return out;
            }
        };
        for project in projects {
            if let Some(parent_dir) = parent_dir
                && !dir_in_scope(&project, parent_dir)
            {
                continue;
            }
            let entries = match self.0.resolver().list_session_entries(&project) {
                Ok(entries) => entries,
                Err(e) => {
                    eprintln!("warning: gemini project {project} failed: {e}");
                    continue;
                }
            };
            for entry in entries {
                let (modified, size) = stat_stamp(&entry.path);
                out.push(ArtifactRef {
                    artifact_type: ArtifactType::Gemini,
                    id: entry.session_uuid.unwrap_or(entry.id),
                    path: Some(project.clone()),
                    modified,
                    size,
                });
            }
        }
        out
    }

    fn stamp(&self, project: Option<&str>, id: &str) -> Option<Stamp> {
        let entries = self.0.resolver().list_session_entries(project?).ok()?;
        let entry = entries
            .into_iter()
            .find(|e| e.id == id || e.session_uuid.as_deref() == Some(id))?;
        Some(stat_stamp(&entry.path))
    }

    fn derive(&self, artifact: &ArtifactRef) -> Result<DerivedDoc> {
        derive::derive_gemini_session_with(self.0, require_path(artifact)?, &artifact.id)
    }
}

// ── codex ──────────────────────────────────────────────────────────

struct CodexSource<'a>(&'a toolpath_codex::CodexConvo);

impl ArtifactSource for CodexSource<'_> {
    /// Rollout files, stat-only. The artifact id is the trailing UUID of
    /// the filename stem (`rollout-<timestamp>-<uuid>`); `read_session`
    /// accepts either the UUID or the full stem, so the fallback is safe.
    fn enumerate(&self, _parent_dir: Option<&Path>) -> Vec<ArtifactRef> {
        let mut out = Vec::new();
        let files = match self.0.io().list_rollout_files() {
            Ok(f) => f,
            Err(e) if is_not_found_codex(&e) => return out,
            Err(e) => {
                eprintln!("warning: codex enumeration failed: {e}");
                return out;
            }
        };
        for file in files {
            let Some(stem) = file.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let id = codex_artifact_id(stem).to_string();
            let (modified, size) = stat_stamp(&file);
            out.push(ArtifactRef {
                artifact_type: ArtifactType::Codex,
                id,
                path: None,
                modified,
                size,
            });
        }
        out
    }

    fn stamp(&self, _project: Option<&str>, id: &str) -> Option<Stamp> {
        let file = self.0.resolver().find_rollout_file(id).ok()?;
        Some(stat_stamp(&file))
    }

    fn derive(&self, artifact: &ArtifactRef) -> Result<DerivedDoc> {
        derive::derive_codex_session_with(self.0, &artifact.id)
    }

    /// The rollout's first line is `session_meta`; its payload carries
    /// cwd directly — one bounded read.
    fn peek_dir(&self, id: &str) -> Option<String> {
        use std::io::{BufRead, BufReader};
        let file = self.0.resolver().find_rollout_file(id).ok()?;
        let f = std::fs::File::open(file).ok()?;
        let mut line = String::new();
        BufReader::new(f).read_line(&mut line).ok()?;
        let v: serde_json::Value = serde_json::from_str(&line).ok()?;
        Some(v.get("payload")?.get("cwd")?.as_str()?.to_string())
    }
}

// ── opencode ───────────────────────────────────────────────────────

struct OpencodeSource<'a>(&'a toolpath_opencode::OpencodeConvo);

impl ArtifactSource for OpencodeSource<'_> {
    /// One header-only `SELECT` — `time_updated` is the fingerprint; no
    /// message bodies are loaded.
    fn enumerate(&self, _parent_dir: Option<&Path>) -> Vec<ArtifactRef> {
        let mut out = Vec::new();
        let sessions = match self.0.io().list_sessions(None) {
            Ok(s) => s,
            Err(e) if is_not_found_opencode(&e) => return out,
            Err(e) => {
                eprintln!("warning: opencode enumeration failed: {e}");
                return out;
            }
        };
        for s in sessions {
            out.push(ArtifactRef {
                artifact_type: ArtifactType::Opencode,
                modified: s.last_activity(),
                path: Some(s.directory.to_string_lossy().into_owned()),
                id: s.id,
                size: None,
            });
        }
        out
    }

    fn stamp(&self, _project: Option<&str>, id: &str) -> Option<Stamp> {
        let sessions = self.0.io().list_sessions(None).ok()?;
        let session = sessions.into_iter().find(|s| s.id == id)?;
        Some((session.last_activity(), None))
    }

    fn derive(&self, artifact: &ArtifactRef) -> Result<DerivedDoc> {
        derive::derive_opencode_session_with(self.0, &artifact.id, false)
    }
}

// ── cursor ─────────────────────────────────────────────────────────

struct CursorSource<'a>(&'a toolpath_cursor::CursorConvo);

impl ArtifactSource for CursorSource<'_> {
    /// Composer headers (one `SELECT` plus a per-composer bubble-count
    /// check) — `lastUpdatedAt` is the fingerprint. Bubble-less drafts
    /// are skipped; unlike `share`, composers without a workspace are
    /// included, since sync doesn't need to rank them by project.
    fn enumerate(&self, _parent_dir: Option<&Path>) -> Vec<ArtifactRef> {
        let mut out = Vec::new();
        let listings = match self.0.io().list_composers() {
            Ok(l) => l,
            Err(e) if is_not_found_cursor(&e) => return out,
            Err(e) => {
                eprintln!("warning: cursor enumeration failed: {e}");
                return out;
            }
        };
        for l in listings.into_iter().filter(|l| l.has_bubbles) {
            out.push(ArtifactRef {
                artifact_type: ArtifactType::Cursor,
                modified: l.head.last_updated_at_utc(),
                path: l
                    .head
                    .workspace_path()
                    .map(|p| p.to_string_lossy().into_owned()),
                id: l.head.composer_id,
                size: None,
            });
        }
        out
    }

    fn stamp(&self, _project: Option<&str>, id: &str) -> Option<Stamp> {
        let headers = self.0.io().read_composer_headers().ok()?;
        let composer = headers
            .all_composers
            .into_iter()
            .find(|c| c.composer_id == id)?;
        Some((composer.last_updated_at_utc(), None))
    }

    fn derive(&self, artifact: &ArtifactRef) -> Result<DerivedDoc> {
        derive::derive_cursor_session_with(self.0, &artifact.id)
    }
}

// ── pi ─────────────────────────────────────────────────────────────

struct PiSource<'a>(&'a toolpath_pi::PiConvo);

impl ArtifactSource for PiSource<'_> {
    /// Session files stat-only; the id comes from a one-line header
    /// peek, falling back to the filename stem's `<timestamp>_<id>`
    /// shape — the same resolution `read_session` accepts.
    fn enumerate(&self, parent_dir: Option<&Path>) -> Vec<ArtifactRef> {
        let mut out = Vec::new();
        let projects = match self.0.list_projects() {
            Ok(ps) => ps,
            Err(e) if is_not_found_pi(&e) => return out,
            Err(e) => {
                eprintln!("warning: pi enumeration failed: {e}");
                return out;
            }
        };
        for project in projects {
            if let Some(parent_dir) = parent_dir
                && !pi_project_in_scope(&project, parent_dir)
            {
                continue;
            }
            let files = match toolpath_pi::reader::list_session_files(self.0.resolver(), &project) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("warning: pi project {project} failed: {e}");
                    continue;
                }
            };
            for file in files {
                let header_id = toolpath_pi::reader::peek_header(&file)
                    .ok()
                    .map(|h| h.id)
                    .filter(|id| !id.is_empty());
                let stem_id = file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.split_once('_'))
                    .map(|(_, rest)| rest.to_string());
                let Some(id) = header_id.or(stem_id) else {
                    continue;
                };
                let (modified, size) = stat_stamp(&file);
                out.push(ArtifactRef {
                    artifact_type: ArtifactType::Pi,
                    id,
                    path: Some(project.clone()),
                    modified,
                    size,
                });
            }
        }
        out
    }

    fn stamp(&self, project: Option<&str>, id: &str) -> Option<Stamp> {
        let files = toolpath_pi::reader::list_session_files(self.0.resolver(), project?).ok()?;
        let file = files.into_iter().find(|f| {
            toolpath_pi::reader::peek_header(f).is_ok_and(|h| h.id == id)
                || f.file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| stem.split_once('_'))
                    .is_some_and(|(_, rest)| rest == id)
        })?;
        Some(stat_stamp(&file))
    }

    fn derive(&self, artifact: &ArtifactRef) -> Result<DerivedDoc> {
        derive::derive_pi_session_with(self.0, require_path(artifact)?, &artifact.id)
    }

    fn in_scope(&self, dir: &str, parent_dir: &Path) -> bool {
        pi_project_in_scope(dir, parent_dir)
    }
}

/// Pi's session-dir names encode '/' as '-', and decoding restores
/// every '-' to '/', so a decoded project string is wrong for any
/// real path containing a hyphen. Compare in the encoded space,
/// where '/' boundaries are '-'.
fn pi_project_in_scope(project: &str, parent_dir: &Path) -> bool {
    fn enc(s: &str) -> String {
        s.replace('/', "-")
    }
    let p = enc(project);
    [
        enc(&parent_dir.to_string_lossy()),
        enc(&canonicalize_or_self(parent_dir).to_string_lossy()),
    ]
    .iter()
    .any(|parent| p == *parent || p.starts_with(&format!("{parent}-")))
}

// ── copilot ────────────────────────────────────────────────────────

struct CopilotSource<'a>(&'a toolpath_copilot::CopilotConvo);

impl ArtifactSource for CopilotSource<'_> {
    /// Session-state directories, stat-only: each session is a
    /// `<id>/events.jsonl` under `session-state/` (or its legacy
    /// sibling); the directory name is the id and the events file is
    /// the fingerprint target.
    fn enumerate(&self, _parent_dir: Option<&Path>) -> Vec<ArtifactRef> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let dirs = [
            self.0.resolver().session_state_dir(),
            self.0.resolver().legacy_session_state_dir(),
        ];
        for dir in dirs.into_iter().flatten() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(id) = entry.file_name().to_str().map(String::from) else {
                    continue;
                };
                let events = entry.path().join("events.jsonl");
                if !events.exists() || !seen.insert(id.clone()) {
                    continue;
                }
                let (modified, size) = stat_stamp(&events);
                out.push(ArtifactRef {
                    artifact_type: ArtifactType::Copilot,
                    id,
                    path: None,
                    modified,
                    size,
                });
            }
        }
        out
    }

    fn stamp(&self, _project: Option<&str>, id: &str) -> Option<Stamp> {
        let file = self.0.resolver().events_file(id).ok()?;
        Some(stat_stamp(&file))
    }

    fn derive(&self, artifact: &ArtifactRef) -> Result<DerivedDoc> {
        derive::derive_copilot_session_with(self.0, &artifact.id)
    }

    /// `session.start` is usually first but the format tolerates it
    /// appearing later, and older CLIs store cwd at the top of the
    /// payload instead of under `context` — scan a bounded number of
    /// lines and accept both shapes, mirroring toolpath-copilot's own
    /// tolerance.
    fn peek_dir(&self, id: &str) -> Option<String> {
        use std::io::{BufRead, BufReader};
        let file = self.0.resolver().events_file(id).ok()?;
        let f = std::fs::File::open(file).ok()?;
        BufReader::new(f).lines().take(10).find_map(|line| {
            let v: serde_json::Value = serde_json::from_str(&line.ok()?).ok()?;
            ["data", "payload"].iter().find_map(|env| {
                let payload = v.get(env)?;
                [payload.get("context").unwrap_or(payload), payload]
                    .iter()
                    .find_map(|scope| {
                        ["cwd", "workingDirectory", "working_dir"]
                            .iter()
                            .find_map(|k| Some(scope.get(k)?.as_str()?.to_string()))
                    })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_for_missing_provider_or_git_is_none() {
        let empty = HarnessBundle::default();
        assert!(source_for(&empty, ArtifactType::Claude).is_none());
        assert!(source_for(&empty, ArtifactType::Git).is_none());

        let with_claude = HarnessBundle {
            claude: Some(toolpath_claude::ClaudeConvo::new()),
            ..Default::default()
        };
        assert!(source_for(&with_claude, ArtifactType::Claude).is_some());
        assert!(
            source_for(&with_claude, ArtifactType::Git).is_none(),
            "git is recorded by `p import`, never enumerated or derived by sync"
        );
    }

    #[test]
    fn pi_scope_matching_survives_hyphenated_paths() {
        // decode_project turns the dir-encoded '-' back into '/', so a
        // real path /Users/b/my-repo arrives as /Users/b/my/repo; the
        // comparator must match it against the real constraint anyway.
        assert!(pi_project_in_scope(
            "/Users/b/my/repo",
            Path::new("/Users/b/my-repo")
        ));
        assert!(pi_project_in_scope(
            "/Users/b/my/repo/sub",
            Path::new("/Users/b/my-repo")
        ));
        assert!(!pi_project_in_scope(
            "/Users/b/other",
            Path::new("/Users/b/my-repo")
        ));
    }
}
