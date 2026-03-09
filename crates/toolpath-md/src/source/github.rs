use std::fmt::Write;

use toolpath::v1::Path;

use super::SourceContext;
use crate::friendly_date_range;

/// Extract GitHub PR context from path metadata, if present.
///
/// Returns `None` if the path has no `extra["github"]` with a `number` field.
pub(super) fn from_path(path: &Path) -> Option<SourceContext> {
    let gh = path
        .meta
        .as_ref()?
        .extra
        .get("github")?
        .as_object()?;

    let identity_line = build_identity_line(gh, &path.steps)?;
    let diffstat = extract_diffstat(gh);

    Some(SourceContext {
        identity_line: Some(identity_line),
        diffstat,
    })
}

/// Build the PR identity line: "**PR #42** by alice · merged · Feb 26–27, 2026"
fn build_identity_line(
    gh: &serde_json::Map<String, serde_json::Value>,
    steps: &[toolpath::v1::Step],
) -> Option<String> {
    let number = gh.get("number")?.as_u64()?;
    let author = gh.get("author").and_then(serde_json::Value::as_str);
    let state = gh.get("state").and_then(serde_json::Value::as_str);
    let merged = gh.get("merged").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let draft = gh.get("draft").and_then(serde_json::Value::as_bool).unwrap_or(false);

    let mut line = format!("**PR #{number}**");
    if let Some(a) = author {
        write!(line, " by {a}").unwrap();
    }
    let status = if merged {
        "merged"
    } else if draft {
        "draft"
    } else {
        state.unwrap_or("open")
    };
    write!(line, " \u{00b7} {status}").unwrap();

    let date_range = friendly_date_range(steps);
    if !date_range.is_empty() {
        write!(line, " \u{00b7} {date_range}").unwrap();
    }

    Some(line)
}

/// Extract diffstat from GitHub metadata fields.
fn extract_diffstat(
    gh: &serde_json::Map<String, serde_json::Value>,
) -> Option<(u64, u64, Option<u64>)> {
    let a = gh.get("additions").and_then(serde_json::Value::as_u64);
    let d = gh.get("deletions").and_then(serde_json::Value::as_u64);
    if a.is_some() || d.is_some() {
        let f = gh.get("changed_files").and_then(serde_json::Value::as_u64);
        Some((a.unwrap_or(0), d.unwrap_or(0), f))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath::v1::{Path, PathIdentity, PathMeta, Step};

    fn make_github_path(github_json: serde_json::Value) -> Path {
        let s1 = Step::new("s1", "human:alice", "2026-02-26T10:00:00Z");
        let s2 = Step::new("s2", "human:alice", "2026-02-27T14:00:00Z")
            .with_parent("s1");
        let mut extra = std::collections::HashMap::new();
        extra.insert("github".to_string(), github_json);
        Path {
            path: PathIdentity {
                id: "pr-42".into(),
                base: None,
                head: "s2".into(),
            },
            steps: vec![s1, s2],
            meta: Some(PathMeta {
                title: Some("Add feature".into()),
                extra,
                ..Default::default()
            }),
        }
    }

    #[test]
    fn from_path_with_full_meta() {
        let path = make_github_path(serde_json::json!({
            "number": 42,
            "author": "alice",
            "state": "open",
            "draft": false,
            "merged": false,
            "additions": 150,
            "deletions": 30,
            "changed_files": 5
        }));
        let ctx = from_path(&path).unwrap();

        let line = ctx.identity_line.unwrap();
        assert!(line.contains("**PR #42**"));
        assert!(line.contains("by alice"));
        assert!(line.contains("open"));
        assert!(line.contains("Feb 26\u{2013}27, 2026"));

        let (add, del, files) = ctx.diffstat.unwrap();
        assert_eq!(add, 150);
        assert_eq!(del, 30);
        assert_eq!(files, Some(5));
    }

    #[test]
    fn merged_overrides_state() {
        let path = make_github_path(serde_json::json!({
            "number": 7,
            "author": "alice",
            "state": "closed",
            "merged": true
        }));
        let ctx = from_path(&path).unwrap();
        let line = ctx.identity_line.unwrap();
        assert!(line.contains("merged"));
        assert!(!line.contains("closed"));
    }

    #[test]
    fn missing_number_returns_none() {
        let path = make_github_path(serde_json::json!({
            "author": "alice",
            "state": "open"
        }));
        assert!(from_path(&path).is_none());
    }

    #[test]
    fn partial_diffstat_additions_only() {
        let path = make_github_path(serde_json::json!({
            "number": 1,
            "additions": 10
        }));
        let ctx = from_path(&path).unwrap();
        let (add, del, files) = ctx.diffstat.unwrap();
        assert_eq!(add, 10);
        assert_eq!(del, 0);
        assert_eq!(files, None);
    }

    #[test]
    fn no_diffstat_without_additions_or_deletions() {
        let path = make_github_path(serde_json::json!({
            "number": 1,
            "changed_files": 3
        }));
        let ctx = from_path(&path).unwrap();
        assert!(ctx.diffstat.is_none());
    }

    #[test]
    fn draft_status() {
        let path = make_github_path(serde_json::json!({
            "number": 99,
            "draft": true,
            "state": "open"
        }));
        let ctx = from_path(&path).unwrap();
        let line = ctx.identity_line.unwrap();
        assert!(line.contains("draft"));
        assert!(!line.contains("open"));
    }
}
