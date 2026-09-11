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
    /// Every step id frozen along the chain, dead ends included.
    pub(crate) step_ids: HashSet<String>,
    /// The frozen head and its ancestors: the only steps new work may
    /// hang off without forking.
    pub(crate) main_line: HashSet<String>,
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
        /// Owned steps on the head's ancestry, in document order.
        main_line: Vec<String>,
        /// Steps the current graph was acknowledged to own that are gone
        /// from the source, sorted. Sending this document would drop them.
        missing: Vec<String>,
    },
    Unsupported(String),
}

/// `owned_baseline` is the acknowledged owned step set of the current
/// mutable graph, `None` when there is none. Inherited steps are never
/// part of it, so their absence from a tail-only source is not a
/// regression; an owned step's absence is reported as `missing`.
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
        let missing = regression(owned_baseline, &owned_ids);
        let kind = if owned_baseline.is_some() {
            SegmentKind::OwnedUpdate
        } else {
            SegmentKind::Independent
        };
        let main_line = main_line_of(&path.steps, &path.path.head);
        return Segmentation::Document {
            kind,
            doc: Box::new(live.clone()),
            owned_ids,
            main_line,
            missing,
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
    // Every frozen parent the new steps name must be one and the same
    // main-line step: that is the base. Anything else is a fork.
    let mut base_step: Option<&str> = None;
    let mut owned = Vec::with_capacity(new_steps.len());
    for step in new_steps {
        let id = &step.step.id;
        let mut kept = Vec::new();
        let mut frozen_parents = 0;
        for parent in &step.step.parents {
            if boundary.step_ids.contains(parent) {
                if !boundary.main_line.contains(parent) {
                    return Segmentation::Unsupported(format!(
                        "step {id:?} branches from frozen step {parent:?}, which is not on the frozen head's ancestry; forking is not supported yet"
                    ));
                }
                match base_step {
                    None => base_step = Some(parent.as_str()),
                    Some(chosen) if chosen != parent => {
                        return Segmentation::Unsupported(format!(
                            "new steps branch from two frozen steps ({chosen:?} and {parent:?}); forking is not supported yet"
                        ));
                    }
                    Some(_) => {}
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
                "step {id:?} is not connected to the frozen history"
            ));
        }
        let mut step = step.clone();
        step.step.parents = kept;
        owned.push(step);
    }
    let Some(base_step) = base_step else {
        return Segmentation::Unsupported(
            "new steps are connected only to each other, not to the frozen history".into(),
        );
    };
    // The derive's head may be a frozen sidecar step (a file snapshot
    // emitted after the conversation); the stored segment then heads at
    // its newest owned step instead.
    let head = if new_ids.contains(path.path.head.as_str()) {
        path.path.head.clone()
    } else {
        owned
            .last()
            .map(|s| s.step.id.clone())
            .expect("owned is nonempty")
    };
    let owned_ids: Vec<String> = owned.iter().map(|s| s.step.id.clone()).collect();
    let missing = regression(owned_baseline, &owned_ids);
    let reference = match BaseReference::new(&boundary.document_url, &boundary.path_id, base_step) {
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
    let main_line = main_line_of(&owned, &head);
    let stored = Path {
        path: PathIdentity {
            id: path.path.id.clone(),
            base: Some(base),
            head,
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
        main_line,
        missing,
    }
}

/// The steps on `head`'s ancestry among `steps`, in document order.
fn main_line_of(steps: &[Step], head: &str) -> Vec<String> {
    let ancestry = toolpath::v1::query::ancestors(steps, head);
    steps
        .iter()
        .map(|s| s.step.id.clone())
        .filter(|id| ancestry.contains(id))
        .collect()
}

fn regression(baseline: Option<&HashSet<String>>, owned_ids: &[String]) -> Vec<String> {
    let Some(baseline) = baseline else {
        return Vec::new();
    };
    let present: HashSet<&str> = owned_ids.iter().map(String::as_str).collect();
    let mut missing: Vec<String> = baseline
        .iter()
        .filter(|id| !present.contains(id.as_str()))
        .cloned()
        .collect();
    missing.sort();
    missing
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
        boundary_with_main_line(head, ids, ids)
    }

    fn boundary_with_main_line(head: &str, ids: &[&str], main_line: &[&str]) -> FrozenBoundary {
        FrozenBoundary {
            document_url: "https://host/u/o/r/graphs/g1".into(),
            path_id: "p".into(),
            head: head.into(),
            step_ids: ids.iter().map(|s| s.to_string()).collect(),
            main_line: main_line.iter().map(|s| s.to_string()).collect(),
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
                missing,
                ..
            } => {
                assert!(missing.is_empty(), "unexpected regression: {missing:?}");
                (kind, *doc, owned_ids)
            }
            other => panic!("expected a document, got {other:?}"),
        }
    }

    fn missing_of(seg: Segmentation) -> Vec<String> {
        match seg {
            Segmentation::Document { missing, .. } => missing,
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
        assert_eq!(
            missing_of(segment(&live, None, Some(&ids(&["a", "b", "c"])))),
            ["c"]
        );
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
        assert_eq!(
            missing_of(segment(
                &live,
                Some(&boundary("b", &["a", "b"])),
                Some(&ids(&["c", "x"])),
            )),
            ["x"]
        );
    }

    #[test]
    fn work_rooted_at_a_dead_end_or_disconnected_is_unsupported() {
        // x is a frozen dead end off a; b is the head.
        let frozen = boundary_with_main_line("b", &["a", "b", "x"], &["a", "b"]);
        let live = graph(
            vec![
                step("a", &[]),
                step("b", &["a"]),
                step("x", &["a"]),
                step("c", &["x"]),
            ],
            "c",
        );
        assert!(matches!(
            segment(&live, Some(&frozen), None),
            Segmentation::Unsupported(m) if m.contains("not on the frozen head's ancestry")
        ));
        // Two different frozen main-line parents is a fork too.
        let live = graph(vec![step("c", &["a"]), step("d", &["b", "c"])], "d");
        assert!(matches!(
            segment(&live, Some(&frozen), None),
            Segmentation::Unsupported(m) if m.contains("two frozen steps")
        ));
        let live = graph(vec![step("c", &[])], "c");
        assert!(matches!(
            segment(&live, Some(&frozen), None),
            Segmentation::Unsupported(m) if m.contains("not connected")
        ));
        let live = graph(vec![step("c", &["zzz"])], "c");
        assert!(matches!(
            segment(&live, Some(&frozen), None),
            Segmentation::Unsupported(m) if m.contains("unknown parent")
        ));
    }

    #[test]
    fn a_main_line_ancestor_may_be_the_base_and_a_frozen_head_yields_to_the_newest_owned_step() {
        // Claude shape: the head t is a trailing tool snapshot whose parent is
        // the last assistant turn b; the resumed turn c hangs off b and the
        // derive's head stays t.
        let frozen = boundary_with_main_line("t", &["a", "b", "t"], &["a", "b", "t"]);
        let live = graph(
            vec![
                step("a", &[]),
                step("b", &["a"]),
                step("c", &["b"]),
                step("d", &["c"]),
                step("t", &["b"]),
            ],
            "t",
        );
        let (kind, doc, owned) = document(segment(&live, Some(&frozen), None));
        assert_eq!(kind, SegmentKind::Continuation);
        assert_eq!(owned, ["c", "d"]);
        let path = doc.single_path().unwrap();
        assert_eq!(path.path.head, "d");
        assert_eq!(
            path.path
                .base
                .as_ref()
                .unwrap()
                .from
                .as_ref()
                .unwrap()
                .to_string(),
            "https://host/u/o/r/graphs/g1#p/b"
        );
        match segment(&live, Some(&frozen), None) {
            Segmentation::Document { main_line, .. } => assert_eq!(main_line, ["c", "d"]),
            other => panic!("{other:?}"),
        }
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
