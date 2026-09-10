//! Split a live derive into the steps the current graph may own.
//!
//! Frozen history is authoritative: steps the server already froze are
//! dropped from the upload whatever the derive now says about them, and
//! the steps that remain must hang off the frozen head.

use std::collections::HashSet;

use toolpath::v1::{Base, BaseReference, Graph, Path, PathIdentity, PathOrRef, Step};

/// The server's frozen view of the path a session continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrozenBoundary {
    /// Stored document URL of the frozen graph, the `base.from` document.
    pub(crate) document_url: String,
    pub(crate) path_id: String,
    pub(crate) head: String,
    /// Every step id the frozen graph owns for that path, dead ends included.
    pub(crate) step_ids: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegmentKind {
    /// No frozen ancestry: the whole derive is owned.
    Independent,
    /// The current mutable graph owns these steps already; replace them.
    OwnedUpdate,
    /// New steps beyond a frozen head; create a continuation.
    Continuation,
}

#[derive(Debug)]
pub(crate) enum Segmentation {
    /// Everything in the derive is frozen; nothing to upload.
    NoNewSteps,
    Document {
        kind: SegmentKind,
        doc: Box<Graph>,
        owned_ids: Vec<String>,
    },
    /// Steps the current graph was acknowledged to own are gone from the source.
    SourceRegression {
        missing: Vec<String>,
    },
    Unsupported(String),
}

/// `owned_baseline` is the acknowledged owned step set of the current
/// mutable graph, `None` when there is none. Inherited steps are never
/// part of it, so their absence from a tail-only source is not a regression.
pub(crate) fn segment(
    live: &Graph,
    boundary: Option<&FrozenBoundary>,
    owned_baseline: Option<&HashSet<String>>,
) -> Segmentation {
    let Some(path) = live.single_path() else {
        return Segmentation::Unsupported(format!(
            "sync uploads single-path documents; this derive has {} paths",
            live.paths.len()
        ));
    };
    let Some(boundary) = boundary else {
        let owned_ids: Vec<String> = path.steps.iter().map(|s| s.step.id.clone()).collect();
        if let Some(missing) = regression(owned_baseline, &owned_ids) {
            return Segmentation::SourceRegression { missing };
        }
        let kind = if owned_baseline.is_some() {
            SegmentKind::OwnedUpdate
        } else {
            SegmentKind::Independent
        };
        return Segmentation::Document {
            kind,
            doc: Box::new(live.clone()),
            owned_ids,
        };
    };
    if boundary.path_id != path.path.id {
        return Segmentation::Unsupported(format!(
            "derive has path {:?} but the frozen graph froze path {:?}",
            path.path.id, boundary.path_id
        ));
    }
    let new_steps: Vec<&Step> = path
        .steps
        .iter()
        .filter(|s| !boundary.step_ids.contains(&s.step.id))
        .collect();
    if new_steps.is_empty() {
        return Segmentation::NoNewSteps;
    }
    let new_ids: HashSet<&str> = new_steps.iter().map(|s| s.step.id.as_str()).collect();
    if !new_ids.contains(path.path.head.as_str()) {
        return Segmentation::Unsupported(format!(
            "head {:?} is a frozen step while {} new steps exist",
            path.path.head,
            new_steps.len()
        ));
    }
    let mut owned = Vec::with_capacity(new_steps.len());
    for step in new_steps {
        let id = &step.step.id;
        let mut kept = Vec::new();
        let mut frozen_parents = 0;
        for parent in &step.step.parents {
            if boundary.step_ids.contains(parent) {
                if *parent != boundary.head {
                    return Segmentation::Unsupported(format!(
                        "step {id:?} branches from frozen step {parent:?}, not the frozen head {:?}; forking is not supported yet",
                        boundary.head
                    ));
                }
                frozen_parents += 1;
            } else if new_ids.contains(parent.as_str()) {
                kept.push(parent.clone());
            } else {
                return Segmentation::Unsupported(format!(
                    "step {id:?} references unknown parent {parent:?}"
                ));
            }
        }
        if kept.is_empty() && frozen_parents == 0 {
            return Segmentation::Unsupported(format!(
                "step {id:?} is not connected to the frozen head {:?}",
                boundary.head
            ));
        }
        let mut step = step.clone();
        step.step.parents = kept;
        owned.push(step);
    }
    let owned_ids: Vec<String> = owned.iter().map(|s| s.step.id.clone()).collect();
    if let Some(missing) = regression(owned_baseline, &owned_ids) {
        return Segmentation::SourceRegression { missing };
    }
    let reference =
        match BaseReference::new(&boundary.document_url, &boundary.path_id, &boundary.head) {
            Ok(r) => r,
            Err(e) => return Segmentation::Unsupported(format!("frozen base is unusable: {e}")),
        };
    let mut base = path.path.base.clone().unwrap_or(Base {
        from: None,
        uri: String::new(),
        ref_str: None,
        branch: None,
    });
    base.from = Some(reference);
    let stored = Path {
        path: PathIdentity {
            id: path.path.id.clone(),
            base: Some(base),
            head: path.path.head.clone(),
            graph_ref: path.path.graph_ref.clone(),
        },
        steps: owned,
        meta: path.meta.clone(),
    };
    let mut doc = live.clone();
    doc.paths = vec![PathOrRef::Path(Box::new(stored))];
    let kind = if owned_baseline.is_some() {
        SegmentKind::OwnedUpdate
    } else {
        SegmentKind::Continuation
    };
    Segmentation::Document {
        kind,
        doc: Box::new(doc),
        owned_ids,
    }
}

fn regression(baseline: Option<&HashSet<String>>, owned_ids: &[String]) -> Option<Vec<String>> {
    let baseline = baseline?;
    let present: HashSet<&str> = owned_ids.iter().map(String::as_str).collect();
    let mut missing: Vec<String> = baseline
        .iter()
        .filter(|id| !present.contains(id.as_str()))
        .cloned()
        .collect();
    if missing.is_empty() {
        return None;
    }
    missing.sort();
    Some(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str, parents: &[&str]) -> Step {
        let mut s = Step::new(id, "agent:test", "2026-09-10T00:00:00Z");
        s.step.parents = parents.iter().map(|p| p.to_string()).collect();
        s
    }

    fn graph(steps: Vec<Step>, head: &str) -> Graph {
        let mut path = Path::new("p", Some(Base::vcs("github:o/r", "abc")), head);
        path.steps = steps;
        Graph::from_path(path)
    }

    fn boundary(head: &str, ids: &[&str]) -> FrozenBoundary {
        FrozenBoundary {
            document_url: "https://host/u/o/r/graphs/g1".into(),
            path_id: "p".into(),
            head: head.into(),
            step_ids: ids.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn ids(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn document(seg: Segmentation) -> (SegmentKind, Graph, Vec<String>) {
        match seg {
            Segmentation::Document {
                kind,
                doc,
                owned_ids,
            } => (kind, *doc, owned_ids),
            other => panic!("expected a document, got {other:?}"),
        }
    }

    #[test]
    fn without_a_boundary_the_whole_derive_is_owned() {
        let live = graph(vec![step("a", &[]), step("b", &["a"])], "b");
        let (kind, doc, owned) = document(segment(&live, None, None));
        assert_eq!(kind, SegmentKind::Independent);
        assert_eq!(owned, ["a", "b"]);
        assert!(
            doc.single_path()
                .unwrap()
                .path
                .base
                .as_ref()
                .unwrap()
                .from
                .is_none()
        );
        let (kind, _, _) = document(segment(&live, None, Some(&ids(&["a"]))));
        assert_eq!(kind, SegmentKind::OwnedUpdate);
    }

    #[test]
    fn losing_an_acknowledged_owned_step_is_a_regression() {
        let live = graph(vec![step("a", &[]), step("b", &["a"])], "b");
        match segment(&live, None, Some(&ids(&["a", "b", "c"]))) {
            Segmentation::SourceRegression { missing } => assert_eq!(missing, ["c"]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_fully_frozen_derive_has_nothing_to_upload_even_if_the_head_moved() {
        let live = graph(vec![step("a", &[]), step("b", &["a"])], "a");
        assert!(matches!(
            segment(&live, Some(&boundary("b", &["a", "b"])), None),
            Segmentation::NoNewSteps
        ));
    }

    #[test]
    fn new_steps_off_the_frozen_head_become_a_continuation() {
        let mut changed_frozen = step("b", &["a"]);
        changed_frozen.step.actor = "human:edited-late".into();
        let live = graph(
            vec![
                step("a", &[]),
                changed_frozen,
                step("c", &["b"]),
                step("d", &["c", "b"]),
            ],
            "d",
        );
        let (kind, doc, owned) = document(segment(&live, Some(&boundary("b", &["a", "b"])), None));
        assert_eq!(kind, SegmentKind::Continuation);
        assert_eq!(owned, ["c", "d"]);
        let path = doc.single_path().unwrap();
        assert_eq!(path.path.head, "d");
        assert_eq!(path.steps[0].step.id, "c");
        assert!(path.steps[0].step.parents.is_empty());
        assert_eq!(path.steps[1].step.parents, ["c"]);
        let base = path.path.base.as_ref().unwrap();
        assert_eq!(base.uri, "github:o/r");
        assert_eq!(
            base.from.as_ref().unwrap().to_string(),
            "https://host/u/o/r/graphs/g1#p/b"
        );
    }

    #[test]
    fn a_mutable_continuation_is_updated_and_inherited_absence_is_not_a_regression() {
        // Tail-only source: the frozen steps are not even present.
        let live = graph(vec![step("c", &["b"]), step("d", &["c"])], "d");
        let (kind, _, owned) = document(segment(
            &live,
            Some(&boundary("b", &["a", "b"])),
            Some(&ids(&["c"])),
        ));
        assert_eq!(kind, SegmentKind::OwnedUpdate);
        assert_eq!(owned, ["c", "d"]);
        match segment(
            &live,
            Some(&boundary("b", &["a", "b"])),
            Some(&ids(&["c", "x"])),
        ) {
            Segmentation::SourceRegression { missing } => assert_eq!(missing, ["x"]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn work_rooted_elsewhere_or_disconnected_is_unsupported() {
        let live = graph(
            vec![step("a", &[]), step("b", &["a"]), step("c", &["a"])],
            "c",
        );
        assert!(matches!(
            segment(&live, Some(&boundary("b", &["a", "b"])), None),
            Segmentation::Unsupported(m) if m.contains("not the frozen head")
        ));
        let live = graph(vec![step("c", &[])], "c");
        assert!(matches!(
            segment(&live, Some(&boundary("b", &["a", "b"])), None),
            Segmentation::Unsupported(m) if m.contains("not connected")
        ));
        let live = graph(vec![step("c", &["zzz"])], "c");
        assert!(matches!(
            segment(&live, Some(&boundary("b", &["a", "b"])), None),
            Segmentation::Unsupported(m) if m.contains("unknown parent")
        ));
        let live = graph(vec![step("c", &["b"])], "b");
        assert!(matches!(
            segment(&live, Some(&boundary("b", &["a", "b"])), None),
            Segmentation::Unsupported(m) if m.contains("head")
        ));
    }

    #[test]
    fn mismatched_path_ids_and_multi_path_graphs_are_unsupported() {
        let live = graph(vec![step("c", &["b"])], "c");
        let mut other = boundary("b", &["a", "b"]);
        other.path_id = "q".into();
        assert!(matches!(
            segment(&live, Some(&other), None),
            Segmentation::Unsupported(_)
        ));
        let mut multi = live.clone();
        multi.paths.push(multi.paths[0].clone());
        assert!(matches!(
            segment(&multi, None, None),
            Segmentation::Unsupported(_)
        ));
    }
}
