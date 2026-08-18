//! [`ArtifactType`], the single enum naming the artifact sources the
//! CLI operates over, plus `ArtifactRef` — an artifact's identity
//! and stat-level fingerprint — and the stamp helpers that produce
//! those fingerprints for sync and import provenance.

/// The kind of artifact an operation ranges over. One enum, used
/// everywhere a command names artifact sources (`p cache sync` types,
/// `share`/`resume` `--harness` via the
/// [`Harness`](crate::harness::Harness) layer, import cache-id
/// prefixes); `name()` doubles as the manifest key and cache-id
/// prefix. Git artifacts are recorded in the manifest when imported
/// but are not *discoverable* — there is no machine-wide registry of
/// repos to enumerate — so sync never re-derives them. Github and
/// pathbase are absent on purpose: they are remote services, not
/// local artifact sources.
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

    /// Width of the provider-name column in picker rows: the length of
    /// the longest `name()` ("opencode"). A test asserts they stay in
    /// sync.
    pub(crate) const NAME_COLUMN_WIDTH: usize = 8;

    /// `name()` left-justified to [`Self::NAME_COLUMN_WIDTH`], so the
    /// text after it starts at the same column on every picker row.
    pub(crate) fn padded_name(&self) -> String {
        format!("{:<width$}", self.name(), width = Self::NAME_COLUMN_WIDTH)
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
        let file = mgr.resolver().conversation_file(project, segment);
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

#[cfg(test)]
mod type_tests {
    use super::ArtifactType;

    #[test]
    fn names_are_distinct() {
        let names: std::collections::HashSet<&str> =
            ArtifactType::ALL.iter().map(|t| t.name()).collect();
        assert_eq!(names.len(), ArtifactType::ALL.len());
    }

    #[test]
    fn name_column_width_is_the_longest_name() {
        let longest = ArtifactType::ALL
            .iter()
            .map(|t| t.name().len())
            .max()
            .unwrap();
        assert_eq!(ArtifactType::NAME_COLUMN_WIDTH, longest);
        for t in ArtifactType::ALL {
            assert_eq!(t.padded_name().len(), ArtifactType::NAME_COLUMN_WIDTH);
        }
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
