//! The harness layer over [`ArtifactType`]: [`Harness`] names the
//! agent runtimes sessions can be shared from and resumed into, and
//! `HarnessBundle` carries one provider manager per installed harness
//! for commands that aggregate across them.

use crate::artifact::ArtifactType;

/// An installed agent harness — a runtime sessions can be shared from
/// and resumed into. Deliberately parallel to [`ArtifactType`]: every
/// harness maps onto an artifact type, but not every artifact type is
/// a harness once non-session kinds (git, github) join `ArtifactType`
/// — you can't resume into a git repo. Harness-facing surfaces
/// (`share`/`resume` `--harness`) take this type so non-harness values
/// stay unrepresentable.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum Harness {
    Claude,
    Gemini,
    Codex,
    Opencode,
    Cursor,
    Pi,
    Copilot,
}

impl Harness {
    /// Every harness, in presentation order.
    pub(crate) const ALL: [Harness; 7] = [
        Harness::Claude,
        Harness::Gemini,
        Harness::Codex,
        Harness::Opencode,
        Harness::Cursor,
        Harness::Pi,
        Harness::Copilot,
    ];

    /// The artifact type this harness's sessions ingest as.
    pub(crate) fn artifact_type(&self) -> ArtifactType {
        match self {
            Harness::Claude => ArtifactType::Claude,
            Harness::Gemini => ArtifactType::Gemini,
            Harness::Codex => ArtifactType::Codex,
            Harness::Opencode => ArtifactType::Opencode,
            Harness::Cursor => ArtifactType::Cursor,
            Harness::Pi => ArtifactType::Pi,
            Harness::Copilot => ArtifactType::Copilot,
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        self.artifact_type().name()
    }

    pub(crate) fn padded_name(&self) -> String {
        self.artifact_type().padded_name()
    }
}

impl ArtifactType {
    /// The harness this artifact type's sessions come from — `None`
    /// for artifact types that aren't agent harnesses (none yet).
    pub(crate) fn harness(&self) -> Option<Harness> {
        match self {
            ArtifactType::Claude => Some(Harness::Claude),
            ArtifactType::Gemini => Some(Harness::Gemini),
            ArtifactType::Codex => Some(Harness::Codex),
            ArtifactType::Opencode => Some(Harness::Opencode),
            ArtifactType::Cursor => Some(Harness::Cursor),
            ArtifactType::Pi => Some(Harness::Pi),
            ArtifactType::Copilot => Some(Harness::Copilot),
            ArtifactType::Git => None,
        }
    }
}

/// Bundle of provider managers used during aggregation. Production
/// code builds this with `providers::harness_bundle`; tests construct
/// it directly with provider-specific resolvers.
#[derive(Default)]
pub(crate) struct HarnessBundle {
    pub(crate) claude: Option<toolpath_claude::ClaudeConvo>,
    pub(crate) gemini: Option<toolpath_gemini::GeminiConvo>,
    pub(crate) codex: Option<toolpath_codex::CodexConvo>,
    pub(crate) copilot: Option<toolpath_copilot::CopilotConvo>,
    pub(crate) opencode: Option<toolpath_opencode::OpencodeConvo>,
    pub(crate) cursor: Option<toolpath_cursor::CursorConvo>,
    pub(crate) pi: Option<toolpath_pi::PiConvo>,
}

pub(crate) fn is_not_found_claude(err: &toolpath_claude::ConvoError) -> bool {
    use toolpath_claude::ConvoError;
    matches!(err, ConvoError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, ConvoError::NoHomeDirectory)
        || matches!(err, ConvoError::ClaudeDirectoryNotFound(_))
}

pub(crate) fn is_not_found_gemini(err: &toolpath_gemini::ConvoError) -> bool {
    use toolpath_gemini::ConvoError;
    matches!(err, ConvoError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, ConvoError::NoHomeDirectory)
        || matches!(err, ConvoError::GeminiDirectoryNotFound(_))
}

pub(crate) fn is_not_found_pi(err: &toolpath_pi::PiError) -> bool {
    use toolpath_pi::PiError;
    matches!(err, PiError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, PiError::ProjectNotFound(_))
}

pub(crate) fn is_not_found_codex(err: &toolpath_codex::ConvoError) -> bool {
    use toolpath_codex::ConvoError;
    matches!(err, ConvoError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, ConvoError::NoHomeDirectory)
        || matches!(err, ConvoError::CodexDirectoryNotFound(_))
}

pub(crate) fn is_not_found_copilot(err: &toolpath_copilot::ConvoError) -> bool {
    use toolpath_copilot::ConvoError;
    matches!(err, ConvoError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, ConvoError::NoHomeDirectory)
        || matches!(err, ConvoError::CopilotDirectoryNotFound(_))
}

pub(crate) fn is_not_found_opencode(err: &toolpath_opencode::ConvoError) -> bool {
    use toolpath_opencode::ConvoError;
    matches!(err, ConvoError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, ConvoError::NoHomeDirectory)
        || matches!(err, ConvoError::OpencodeDirectoryNotFound(_))
        || matches!(err, ConvoError::DatabaseNotFound(_))
}

pub(crate) fn is_not_found_cursor(err: &toolpath_cursor::CursorError) -> bool {
    use toolpath_cursor::CursorError;
    matches!(err, CursorError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, CursorError::NoHomeDirectory)
        || matches!(err, CursorError::CursorDataDirectoryNotFound(_))
        || matches!(err, CursorError::DatabaseNotFound(_))
}
