mod github;

use toolpath::v1::Path;

/// Source-specific context extracted from path metadata.
///
/// The generic renderer reads these fields without knowing which source produced them.
#[derive(Debug, Default)]
pub(crate) struct SourceContext {
    /// Source-specific identity line (e.g., "**PR #42** by alice · merged").
    /// Replaces the generic **Head:** line when present.
    pub identity_line: Option<String>,

    /// Authoritative diffstat from source metadata, bypassing diff counting.
    /// `(additions, deletions, changed_files)`.
    pub diffstat: Option<(u64, u64, Option<u64>)>,
}

/// Detect the source of a path and extract source-specific context.
pub(crate) fn detect(path: &Path) -> SourceContext {
    if let Some(ctx) = github::from_path(path) {
        return ctx;
    }
    // Future: git::from_path(path), claude::from_path(path)
    SourceContext::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath::v1::{Path, PathIdentity, Step};

    #[test]
    fn detect_returns_default_for_plain_path() {
        let s1 = Step::new("s1", "human:alex", "2026-01-29T10:00:00Z");
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
            },
            steps: vec![s1],
            meta: None,
        };
        let ctx = detect(&path);
        assert!(ctx.identity_line.is_none());
        assert!(ctx.diffstat.is_none());
    }
}
