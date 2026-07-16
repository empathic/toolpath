//! `path p cache sync` — incremental ingestion of artifacts into the
//! document cache — and [`ArtifactType`], the single enum naming the
//! artifact sources the CLI operates over.
//!
//! Sync enumerates artifacts across the requested types (today all six
//! are agent-session providers), compares each against the sync
//! manifest at `$CONFIG_DIR/sync.json`, and derives + caches only what
//! is new or changed. Change detection is stat-level: the fingerprint
//! is the source file's mtime + size (or the database row's updated-at
//! for the SQLite-backed providers), so deciding "nothing changed"
//! never reads session bodies. Artifacts deleted upstream keep both
//! their cache document and their manifest record.

/// The kind of artifact an operation ranges over. One enum, used
/// everywhere a command names artifact sources (`p cache sync` types,
/// `share`/`resume` `--harness`, import cache-id prefixes); `name()`
/// doubles as the manifest key and cache-id prefix. Git artifacts are
/// recorded in the manifest when imported but are not *discoverable* —
/// there is no machine-wide registry of repos to enumerate — so sync
/// never re-derives them. Github and pathbase are absent on purpose:
/// they are remote services, not local artifact sources.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum ArtifactType {
    Claude,
    Gemini,
    Codex,
    Opencode,
    Cursor,
    Pi,
    Copilot,
    Git,
}

impl ArtifactType {
    /// Every artifact type, in presentation order.
    pub(crate) const ALL: [ArtifactType; 8] = [
        ArtifactType::Claude,
        ArtifactType::Gemini,
        ArtifactType::Codex,
        ArtifactType::Opencode,
        ArtifactType::Cursor,
        ArtifactType::Pi,
        ArtifactType::Copilot,
        ArtifactType::Git,
    ];

    pub(crate) fn name(&self) -> &'static str {
        match self {
            ArtifactType::Claude => "claude",
            ArtifactType::Gemini => "gemini",
            ArtifactType::Codex => "codex",
            ArtifactType::Opencode => "opencode",
            ArtifactType::Cursor => "cursor",
            ArtifactType::Pi => "pi",
            ArtifactType::Copilot => "copilot",
            ArtifactType::Git => "git",
        }
    }

    /// Padded so all symbols line up in the fzf column. Longest is
    /// "opencode" (8); pad shorter names to match.
    pub(crate) fn symbol(&self) -> &'static str {
        match self {
            ArtifactType::Claude => "claude  ",
            ArtifactType::Gemini => "gemini  ",
            ArtifactType::Codex => "codex   ",
            ArtifactType::Opencode => "opencode",
            ArtifactType::Cursor => "cursor  ",
            ArtifactType::Pi => "pi      ",
            ArtifactType::Copilot => "copilot ",
            ArtifactType::Git => "git     ",
        }
    }

    /// True when the underlying provider keys artifacts by a filesystem
    /// path (the project directory). claude/gemini/pi: true.
    /// codex/opencode/cursor: false (sessions store cwd per-row, not as
    /// a directory key — cursor stores it as
    /// `workspaceIdentifier.uri.fsPath` on each composer).
    pub(crate) fn path_keyed(&self) -> bool {
        matches!(
            self,
            ArtifactType::Claude | ArtifactType::Gemini | ArtifactType::Pi | ArtifactType::Git
        )
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        <Self as clap::ValueEnum>::from_str(s, false).ok()
    }
}

/// An artifact's identity plus the stat-level fingerprint of its
/// source. Sync enumerates these for change detection (producing one
/// never parses session bodies), and `p import`/`share` fill one as
/// the provenance of each derived document so the write can be
/// recorded in the manifest.
#[derive(Debug, Clone)]
pub(crate) struct ArtifactRef {
    pub(crate) artifact_type: ArtifactType,
    pub(crate) id: String,
    /// Filesystem path the artifact is keyed under, for path-keyed
    /// providers (the project directory; the repo for git).
    pub(crate) path: Option<String>,
    /// Source mtime (file providers) or updated-at (DB providers).
    pub(crate) modified: Option<chrono::DateTime<chrono::Utc>>,
    /// Source file size; `None` for DB-backed providers.
    pub(crate) size: Option<u64>,
}

/// (mtime, size) of a file, both `None` when the stat fails.
pub(crate) fn stat_stamp(
    path: &std::path::Path,
) -> (Option<chrono::DateTime<chrono::Utc>>, Option<u64>) {
    match std::fs::metadata(path) {
        Ok(md) => (
            md.modified()
                .ok()
                .map(chrono::DateTime::<chrono::Utc>::from),
            Some(md.len()),
        ),
        Err(_) => (None, None),
    }
}

/// Stat-level fingerprint of a whole claude session chain: max mtime
/// across the chain's segment files plus the sum of their sizes. Claude
/// Code rotates to a new file on continuation (plan-mode exit, resume,
/// fork) while the chain keeps the *first* segment's id — appends land
/// in the newest segment, so statting the head file alone would freeze
/// the fingerprint at the first rotation and sync would never see the
/// later turns. The chain here is exactly the set of files
/// `read_conversation` merges, so the fingerprint and the derived doc
/// move in lockstep. The chain index is already built (and cached) by
/// the `list_conversations` call every caller makes first.
pub(crate) fn claude_chain_stamp(
    mgr: &toolpath_claude::ClaudeConvo,
    project: &str,
    session: &str,
) -> (Option<chrono::DateTime<chrono::Utc>>, Option<u64>) {
    let segments = match mgr.session_chain(project, session) {
        Ok(segments) if !segments.is_empty() => segments,
        _ => vec![session.to_string()],
    };
    let mut modified: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut size: Option<u64> = None;
    for segment in &segments {
        let Ok(file) = mgr.resolver().conversation_file(project, segment) else {
            continue;
        };
        let (m, s) = stat_stamp(&file);
        if let Some(m) = m {
            modified = Some(modified.map_or(m, |cur| cur.max(m)));
        }
        if let Some(s) = s {
            size = Some(size.unwrap_or(0) + s);
        }
    }
    (modified, size)
}

/// The trailing UUID of a codex rollout filename stem
/// (`rollout-<timestamp>-<uuid>`), or the whole stem when it doesn't end
/// in one. Codex's `read_session` resolves either form.
pub(crate) fn codex_artifact_id(stem: &str) -> &str {
    stem.len()
        .checked_sub(36)
        .and_then(|at| stem.get(at..))
        .filter(|tail| tail.bytes().filter(|&b| b == b'-').count() == 4)
        .unwrap_or(stem)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) use engine::*;

#[cfg(not(target_os = "emscripten"))]
mod engine;

#[cfg(not(target_os = "emscripten"))]
pub(crate) mod sources;

#[cfg(test)]
mod type_tests {
    use super::ArtifactType;

    #[test]
    fn names_and_symbols_are_distinct() {
        let names: std::collections::HashSet<&str> =
            ArtifactType::ALL.iter().map(|t| t.name()).collect();
        let symbols: std::collections::HashSet<&str> =
            ArtifactType::ALL.iter().map(|t| t.symbol()).collect();
        assert_eq!(names.len(), ArtifactType::ALL.len());
        assert_eq!(symbols.len(), ArtifactType::ALL.len());
    }

    #[test]
    fn path_keyed_matches_design() {
        assert!(ArtifactType::Claude.path_keyed());
        assert!(ArtifactType::Gemini.path_keyed());
        assert!(ArtifactType::Pi.path_keyed());
        assert!(!ArtifactType::Codex.path_keyed());
        assert!(!ArtifactType::Opencode.path_keyed());
        assert!(!ArtifactType::Cursor.path_keyed());
        assert!(ArtifactType::Git.path_keyed());
    }

    #[test]
    fn parse_roundtrips_every_name() {
        for t in ArtifactType::ALL {
            assert_eq!(ArtifactType::parse(t.name()), Some(t));
        }
        assert_eq!(ArtifactType::parse("frobnicate"), None);
    }
}
