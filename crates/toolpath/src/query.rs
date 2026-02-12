//! Graph traversal and query operations for Toolpath documents.

use crate::types::Step;
use std::collections::{HashMap, HashSet};

/// Walk the parent chain from `head_id`, returning all ancestor step IDs (inclusive).
///
/// # Examples
///
/// ```
/// use toolpath::v1::{Step, query};
///
/// let s1 = Step::new("s1", "human:alex", "2026-01-29T10:00:00Z")
///     .with_raw_change("f.rs", "@@");
/// let s2 = Step::new("s2", "agent:claude", "2026-01-29T10:01:00Z")
///     .with_parent("s1")
///     .with_raw_change("f.rs", "@@");
/// let s3 = Step::new("s3", "human:alex", "2026-01-29T10:02:00Z")
///     .with_parent("s2")
///     .with_raw_change("f.rs", "@@");
///
/// let anc = query::ancestors(&[s1, s2, s3], "s3");
/// assert_eq!(anc.len(), 3);
/// assert!(anc.contains("s1"));
/// assert!(anc.contains("s2"));
/// assert!(anc.contains("s3"));
/// ```
pub fn ancestors(steps: &[Step], head_id: &str) -> HashSet<String> {
    let step_map: HashMap<&str, &Step> = steps.iter().map(|s| (s.step.id.as_str(), s)).collect();
    let mut result = HashSet::new();
    let mut stack = vec![head_id];

    while let Some(id) = stack.pop() {
        if result.insert(id.to_string())
            && let Some(step) = step_map.get(id)
        {
            for parent in &step.step.parents {
                stack.push(parent);
            }
        }
    }

    result
}

/// Steps not on the path to `head_id` — abandoned branches.
///
/// # Examples
///
/// ```
/// use toolpath::v1::{Step, query};
///
/// let s1 = Step::new("s1", "human:alex", "2026-01-29T10:00:00Z")
///     .with_raw_change("f.rs", "@@");
/// // Two competing branches off s1:
/// let s2 = Step::new("s2", "agent:claude", "2026-01-29T10:01:00Z")
///     .with_parent("s1")
///     .with_raw_change("f.rs", "@@");
/// let abandoned = Step::new("s2a", "agent:claude", "2026-01-29T10:01:30Z")
///     .with_parent("s1")
///     .with_raw_change("f.rs", "@@");
/// let s3 = Step::new("s3", "human:alex", "2026-01-29T10:02:00Z")
///     .with_parent("s2")
///     .with_raw_change("f.rs", "@@");
///
/// let steps = vec![s1, s2, abandoned, s3];
/// let dead = query::dead_ends(&steps, "s3");
/// assert_eq!(dead.len(), 1);
/// assert_eq!(dead[0].step.id, "s2a");
/// ```
pub fn dead_ends<'a>(steps: &'a [Step], head_id: &str) -> Vec<&'a Step> {
    let active = ancestors(steps, head_id);
    steps
        .iter()
        .filter(|s| !active.contains(&s.step.id))
        .collect()
}

/// Filter steps by actor prefix (e.g., `"human:"`, `"agent:claude"`).
///
/// # Examples
///
/// ```
/// use toolpath::v1::{Step, query};
///
/// let steps = vec![
///     Step::new("s1", "human:alex", "2026-01-29T10:00:00Z"),
///     Step::new("s2", "agent:claude", "2026-01-29T10:01:00Z"),
///     Step::new("s3", "tool:rustfmt", "2026-01-29T10:02:00Z"),
///     Step::new("s4", "human:bob", "2026-01-29T10:03:00Z"),
/// ];
///
/// let humans = query::filter_by_actor(&steps, "human:");
/// assert_eq!(humans.len(), 2);
/// assert_eq!(humans[0].step.id, "s1");
/// assert_eq!(humans[1].step.id, "s4");
/// ```
pub fn filter_by_actor<'a>(steps: &'a [Step], prefix: &str) -> Vec<&'a Step> {
    steps
        .iter()
        .filter(|s| s.step.actor.starts_with(prefix))
        .collect()
}

/// Steps that touch a given artifact (by key in the change map).
pub fn filter_by_artifact<'a>(steps: &'a [Step], artifact: &str) -> Vec<&'a Step> {
    steps
        .iter()
        .filter(|s| s.change.contains_key(artifact))
        .collect()
}

/// Steps whose timestamp falls within [start, end] (ISO 8601 string comparison).
pub fn filter_by_time_range<'a>(steps: &'a [Step], start: &str, end: &str) -> Vec<&'a Step> {
    steps
        .iter()
        .filter(|s| {
            let ts = s.step.timestamp.as_str();
            ts >= start && ts <= end
        })
        .collect()
}

/// All artifact URLs mentioned across all steps.
pub fn all_artifacts(steps: &[Step]) -> HashSet<&str> {
    steps
        .iter()
        .flat_map(|s| s.change.keys().map(|k| k.as_str()))
        .collect()
}

/// All actor strings across all steps.
pub fn all_actors(steps: &[Step]) -> HashSet<&str> {
    steps.iter().map(|s| s.step.actor.as_str()).collect()
}

/// Build an ID → Step lookup map.
pub fn step_index(steps: &[Step]) -> HashMap<&str, &Step> {
    steps.iter().map(|s| (s.step.id.as_str(), s)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Step;

    fn make_step(id: &str, actor: &str, parents: &[&str], artifacts: &[&str]) -> Step {
        let mut step = Step::new(id, actor, "2026-01-29T10:00:00Z");
        for p in parents {
            step = step.with_parent(*p);
        }
        for a in artifacts {
            step = step.with_raw_change(*a, "@@");
        }
        step
    }

    #[test]
    fn test_ancestors_linear() {
        let steps = vec![
            make_step("s1", "human:a", &[], &["f.rs"]),
            make_step("s2", "agent:b", &["s1"], &["f.rs"]),
            make_step("s3", "human:a", &["s2"], &["g.rs"]),
        ];

        let anc = ancestors(&steps, "s3");
        assert_eq!(anc.len(), 3);
        assert!(anc.contains("s1"));
        assert!(anc.contains("s2"));
        assert!(anc.contains("s3"));
    }

    #[test]
    fn test_ancestors_with_fork() {
        let steps = vec![
            make_step("s1", "human:a", &[], &["f.rs"]),
            make_step("s2", "agent:b", &["s1"], &["f.rs"]),
            make_step("s2a", "agent:b", &["s1"], &["f.rs"]), // dead end
            make_step("s3", "human:a", &["s2"], &["g.rs"]),
        ];

        let anc = ancestors(&steps, "s3");
        assert_eq!(anc.len(), 3);
        assert!(!anc.contains("s2a"));
    }

    #[test]
    fn test_dead_ends() {
        let steps = vec![
            make_step("s1", "human:a", &[], &["f.rs"]),
            make_step("s2", "agent:b", &["s1"], &["f.rs"]),
            make_step("s2a", "agent:b", &["s1"], &["f.rs"]), // dead end
            make_step("s3", "human:a", &["s2"], &["g.rs"]),
        ];

        let dead = dead_ends(&steps, "s3");
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].step.id, "s2a");
    }

    #[test]
    fn test_filter_by_actor() {
        let steps = vec![
            make_step("s1", "human:alex", &[], &["f.rs"]),
            make_step("s2", "agent:claude", &["s1"], &["f.rs"]),
            make_step("s3", "tool:rustfmt", &["s2"], &["f.rs"]),
            make_step("s4", "human:bob", &["s3"], &["f.rs"]),
        ];

        assert_eq!(filter_by_actor(&steps, "human:").len(), 2);
        assert_eq!(filter_by_actor(&steps, "agent:").len(), 1);
        assert_eq!(filter_by_actor(&steps, "tool:").len(), 1);
    }

    #[test]
    fn test_filter_by_artifact() {
        let steps = vec![
            make_step("s1", "human:a", &[], &["src/main.rs"]),
            make_step("s2", "agent:b", &["s1"], &["src/lib.rs"]),
            make_step("s3", "human:a", &["s2"], &["src/main.rs", "src/lib.rs"]),
        ];

        assert_eq!(filter_by_artifact(&steps, "src/main.rs").len(), 2);
        assert_eq!(filter_by_artifact(&steps, "src/lib.rs").len(), 2);
    }

    #[test]
    fn test_filter_by_time_range() {
        let steps = vec![
            Step::new("s1", "human:a", "2026-01-29T10:00:00Z"),
            Step::new("s2", "human:a", "2026-01-29T12:00:00Z"),
            Step::new("s3", "human:a", "2026-01-29T14:00:00Z"),
        ];

        let filtered = filter_by_time_range(&steps, "2026-01-29T11:00:00Z", "2026-01-29T13:00:00Z");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].step.id, "s2");
    }

    #[test]
    fn test_all_artifacts() {
        let steps = vec![
            make_step("s1", "human:a", &[], &["a.rs", "b.rs"]),
            make_step("s2", "human:a", &["s1"], &["b.rs", "c.rs"]),
        ];

        let arts = all_artifacts(&steps);
        assert_eq!(arts.len(), 3);
        assert!(arts.contains("a.rs"));
        assert!(arts.contains("b.rs"));
        assert!(arts.contains("c.rs"));
    }

    #[test]
    fn test_all_actors() {
        let steps = vec![
            make_step("s1", "human:alex", &[], &["f.rs"]),
            make_step("s2", "agent:claude", &["s1"], &["f.rs"]),
            make_step("s3", "human:alex", &["s2"], &["f.rs"]),
        ];

        let actors = all_actors(&steps);
        assert_eq!(actors.len(), 2);
        assert!(actors.contains("human:alex"));
        assert!(actors.contains("agent:claude"));
    }

    #[test]
    fn test_step_index() {
        let steps = vec![
            make_step("s1", "human:a", &[], &["f.rs"]),
            make_step("s2", "human:a", &["s1"], &["f.rs"]),
        ];

        let idx = step_index(&steps);
        assert_eq!(idx.len(), 2);
        assert_eq!(idx["s1"].step.actor, "human:a");
    }
}
