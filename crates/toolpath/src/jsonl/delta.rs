//! Incremental JSONL emission: the lines that bring a reader holding some
//! steps of a path up to the whole path.
//!
//! See the "Delta Emission" section of `docs/RFC-jsonl.md`.

use super::{
    BatchError, JsonlError, JsonlLine, PathMetaBody, PathMetaPatch, PathOpenBody, actor_def_lines,
    step_lines,
};
use crate::types::{Path, Step};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fmt;

/// Errors produced by [`delta_lines`] and [`delta_bodies`](super::delta_bodies).
#[non_exhaustive]
#[derive(Debug)]
pub enum DeltaError {
    /// Reserved for failures of the existing JSONL machinery the delta reuses.
    Jsonl(JsonlError),
    /// A new step whose parent is neither in `stored` nor among the new steps.
    DanglingParent {
        /// The new step with the missing parent.
        step: String,
        /// The parent id found nowhere.
        parent: String,
    },
    /// New steps whose parent references form a cycle; `steps` names those on
    /// or between cycles, in document order.
    Cycle {
        /// Step ids on or between the cycles.
        steps: Vec<String>,
    },
    /// Steps the reader holds that the caller now derives with other
    /// content. Appending cannot change a held step, so the reader cannot
    /// be brought up to `path`. [`delta_lines`] never sees held content and
    /// never returns this; callers that can see it do. `steps` is in
    /// document order.
    Amended {
        /// Ids of the held steps that now derive differently.
        steps: Vec<String>,
    },
    /// Splitting the lines into bodies failed; see [`BatchError`].
    Batch(BatchError),
}

impl fmt::Display for DeltaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeltaError::Jsonl(_) => write!(f, "JSONL machinery failed"),
            DeltaError::DanglingParent { step, parent } => write!(
                f,
                "step {step:?} has parent {parent:?}, which is neither stored nor a new step"
            ),
            DeltaError::Cycle { steps } => {
                write!(f, "parent references form a cycle among steps {steps:?}")
            }
            DeltaError::Amended { steps } => write!(
                f,
                "steps {steps:?} are already stored with other content; appending cannot change them"
            ),
            DeltaError::Batch(_) => write!(f, "splitting the delta into bodies failed"),
        }
    }
}

impl std::error::Error for DeltaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DeltaError::Jsonl(e) => Some(e),
            DeltaError::Batch(e) => Some(e),
            DeltaError::DanglingParent { .. }
            | DeltaError::Cycle { .. }
            | DeltaError::Amended { .. } => None,
        }
    }
}

impl From<JsonlError> for DeltaError {
    fn from(e: JsonlError) -> Self {
        DeltaError::Jsonl(e)
    }
}

/// Lines that bring a reader holding the step ids in `stored` up to `path`.
/// `opened` says whether the reader already has the path: false starts the
/// lines with a `PathOpen`, true ends them with a `PathMeta` patch (when the
/// path has meta), whatever `stored` holds. See "Delta Emission" in
/// `docs/RFC-jsonl.md`.
///
/// # Errors
///
/// [`DeltaError::DanglingParent`] or [`DeltaError::Cycle`] when the new steps
/// have no parents-first order.
pub fn delta_lines(
    path: &Path,
    stored: &HashSet<String>,
    opened: bool,
) -> Result<Vec<JsonlLine>, DeltaError> {
    let new_steps: Vec<&Step> = path
        .steps
        .iter()
        .filter(|s| !stored.contains(&s.step.id))
        .collect();
    let ordered = parents_first_refs(&new_steps, stored)?;

    let mut lines = Vec::new();
    if !opened {
        lines.push(JsonlLine::PathOpen(PathOpenBody::for_path(path)));
    }
    lines.extend(actor_def_lines(path.meta.as_ref()));
    for step in ordered {
        lines.extend(step_lines(step));
    }
    lines.push(JsonlLine::head(&path.path.head));
    if opened && let Some(m) = &path.meta {
        lines.push(JsonlLine::PathMeta(PathMetaBody {
            patch: PathMetaPatch::full(m),
        }));
    }
    Ok(lines)
}

/// `steps` in an order where every step comes after its parents among
/// `steps`: document order when it already is, else a stable topological
/// order (ready steps taken in document order). A parent not among `steps`
/// may be one of the `held` ids, which the reader already has; a parent
/// among `steps` comes first even when it is also held.
///
/// # Errors
///
/// [`DeltaError::DanglingParent`] for a parent that is neither in `steps`
/// nor `held`; [`DeltaError::Cycle`] for a parent cycle among `steps`.
pub fn parents_first<'a>(
    steps: &'a [Step],
    held: &HashSet<String>,
) -> Result<Vec<&'a Step>, DeltaError> {
    let refs: Vec<&Step> = steps.iter().collect();
    parents_first_refs(&refs, held)
}

/// Order `new` parents-first: document order when it already is, else Kahn's
/// algorithm taking the lowest document index among ready steps.
fn parents_first_refs<'a>(
    new: &[&'a Step],
    stored: &HashSet<String>,
) -> Result<Vec<&'a Step>, DeltaError> {
    let index: HashMap<&str, usize> = new
        .iter()
        .enumerate()
        .map(|(i, s)| (s.step.id.as_str(), i))
        .collect();

    let mut in_order = true;
    for (i, s) in new.iter().enumerate() {
        for p in &s.step.parents {
            match index.get(p.as_str()) {
                Some(&j) => in_order &= j < i,
                None if stored.contains(p) => {}
                None => {
                    return Err(DeltaError::DanglingParent {
                        step: s.step.id.clone(),
                        parent: p.clone(),
                    });
                }
            }
        }
    }
    if in_order {
        return Ok(new.to_vec());
    }

    // Edges parent -> child among new steps; `pending[i]` counts unmet
    // parent references of step i.
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); new.len()];
    let mut pending: Vec<usize> = vec![0; new.len()];
    for (i, s) in new.iter().enumerate() {
        for p in &s.step.parents {
            if let Some(&j) = index.get(p.as_str()) {
                children[j].push(i);
                pending[i] += 1;
            }
        }
    }
    let mut ready: BinaryHeap<Reverse<usize>> = (0..new.len())
        .filter(|&i| pending[i] == 0)
        .map(Reverse)
        .collect();
    let mut out = Vec::with_capacity(new.len());
    let mut emitted = vec![false; new.len()];
    while let Some(Reverse(i)) = ready.pop() {
        out.push(new[i]);
        emitted[i] = true;
        for &c in &children[i] {
            pending[c] -= 1;
            if pending[c] == 0 {
                ready.push(Reverse(c));
            }
        }
    }
    if out.len() == new.len() {
        return Ok(out);
    }

    // Kahn stalled: the unemitted steps are the cycles and everything
    // downstream of them. Prune sinks from that set repeatedly; what remains
    // lies on a cycle (or between two), which is what the error names.
    let mut on_cycle = emitted.iter().map(|e| !e).collect::<Vec<bool>>();
    loop {
        let mut pruned = false;
        for i in 0..new.len() {
            if on_cycle[i] && !children[i].iter().any(|&c| on_cycle[c]) {
                on_cycle[i] = false;
                pruned = true;
            }
        }
        if !pruned {
            break;
        }
    }
    Err(DeltaError::Cycle {
        steps: (0..new.len())
            .filter(|&i| on_cycle[i])
            .map(|i| new[i].step.id.clone())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonl::{JsonlError, JsonlLine};
    use crate::types::{
        ActorDefinition, Base, Graph, Path, PathIdentity, PathMeta, Ref, Signature, Step, StepMeta,
    };
    use serde_json::json;
    use std::collections::{HashMap, HashSet};

    fn step(id: &str, parents: &[&str]) -> Step {
        let mut s = Step::new(id, "human:alex", "2026-01-01T00:00:00Z")
            .with_raw_change("src/main.rs", format!("@@ -1 +1 @@\n-a\n+{id}"));
        for p in parents {
            s = s.with_parent(*p);
        }
        s
    }

    fn path_of(steps: Vec<Step>, head: &str) -> Path {
        Path {
            path: PathIdentity {
                id: "p".into(),
                base: Some(Base::vcs("github:org/repo", "abc")),
                head: head.into(),
                graph_ref: None,
            },
            steps,
            meta: None,
        }
    }

    fn example(json: &str) -> Path {
        Graph::from_json(json)
            .unwrap()
            .single_path()
            .expect("single-path example")
            .clone()
    }

    /// Sorted-key JSON: `to_value` then `to_string` (`preserve_order` is off here).
    fn canonical<T: serde::Serialize>(v: &T) -> String {
        serde_json::to_string(&serde_json::to_value(v).unwrap()).unwrap()
    }

    fn to_text(lines: &[JsonlLine]) -> String {
        let mut out = String::new();
        for l in lines {
            out.push_str(&canonical(l));
            out.push('\n');
        }
        out
    }

    fn tags(lines: &[JsonlLine]) -> Vec<&'static str> {
        lines
            .iter()
            .map(|l| match l {
                JsonlLine::PathOpen(_) => "PathOpen",
                JsonlLine::Step(_) => "Step",
                JsonlLine::ActorDef(_) => "ActorDef",
                JsonlLine::Signature(_) => "Signature",
                JsonlLine::PathMeta(_) => "PathMeta",
                JsonlLine::Head(_) => "Head",
                JsonlLine::PathClose(_) => "PathClose",
            })
            .collect()
    }

    fn step_ids(lines: &[JsonlLine]) -> Vec<String> {
        lines
            .iter()
            .filter_map(|l| match l {
                JsonlLine::Step(b) => Some(b.0.step.id.clone()),
                _ => None,
            })
            .collect()
    }

    fn ids(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// Every emitted step's parents are in `stored` or emitted earlier.
    fn assert_parents_first(lines: &[JsonlLine], stored: &HashSet<String>) {
        let mut seen = stored.clone();
        for l in lines {
            if let JsonlLine::Step(b) = l {
                for p in &b.0.step.parents {
                    assert!(
                        seen.contains(p),
                        "step {} emitted before its parent {p}",
                        b.0.step.id
                    );
                }
                seen.insert(b.0.step.id.clone());
            }
        }
    }

    /// The first `k` steps in document order, head at the last of them
    /// (`path` itself when `k` covers every step).
    fn prefix(path: &Path, k: usize) -> Path {
        let mut p = path.clone();
        if k < p.steps.len() {
            p.steps.truncate(k);
            p.path.head = p.steps[k - 1].step.id.clone();
        }
        p
    }

    /// What a reader can reconstruct: `delta_lines` never carries path-level
    /// signatures.
    fn without_path_signatures(path: &Path) -> Path {
        let mut p = path.clone();
        if let Some(m) = p.meta.as_mut() {
            m.signatures.clear();
        }
        p
    }

    /// Deltas over growing prefixes, concatenated, read back equal to a one-shot write.
    fn assert_incremental_readback(full: &Path) {
        let n = full.steps.len();
        let mut text = String::new();
        let mut stored: HashSet<String> = HashSet::new();
        for k in 1..=n {
            let pre = prefix(full, k);
            let lines = delta_lines(&pre, &stored, !stored.is_empty()).unwrap();
            assert_parents_first(&lines, &stored);
            text.push_str(&to_text(&lines));
            stored = pre.steps.iter().map(|s| s.step.id.clone()).collect();
        }
        let back = Path::from_jsonl_str(&text).unwrap();
        assert_eq!(canonical(&back), canonical(&without_path_signatures(full)));

        // And one jump: lines for 1..k, then the delta straight to N.
        for k in 1..n {
            let pre = prefix(full, k);
            let first = delta_lines(&pre, &HashSet::new(), false).unwrap();
            let stored: HashSet<String> = pre.steps.iter().map(|s| s.step.id.clone()).collect();
            let rest = delta_lines(full, &stored, true).unwrap();
            assert_parents_first(&rest, &stored);
            let text = format!("{}{}", to_text(&first), to_text(&rest));
            let back = Path::from_jsonl_str(&text).unwrap();
            assert_eq!(
                canonical(&back),
                canonical(&without_path_signatures(full)),
                "k = {k}"
            );
        }
    }

    #[test]
    fn first_send_line_sequence() {
        let mut s2 = step("s2", &["s1"]);
        s2.meta = Some(StepMeta {
            signatures: vec![Signature {
                signer: "human:alex".into(),
                key: "gpg:A".into(),
                scope: "author".into(),
                sig: "SIG".into(),
                timestamp: None,
            }],
            ..Default::default()
        });
        let mut actors = HashMap::new();
        actors.insert("human:zoe".to_string(), ActorDefinition::default());
        actors.insert("human:alex".to_string(), ActorDefinition::default());
        let mut path = path_of(vec![step("s1", &[]), s2], "s2");
        path.path.graph_ref = Some("toolpath://g".into());
        path.meta = Some(PathMeta {
            title: Some("T".into()),
            actors: Some(actors),
            ..Default::default()
        });

        let lines = delta_lines(&path, &HashSet::new(), false).unwrap();
        assert_eq!(
            tags(&lines),
            [
                "PathOpen",
                "ActorDef",
                "ActorDef",
                "Step",
                "Step",
                "Signature",
                "Head"
            ]
        );
        match &lines[0] {
            JsonlLine::PathOpen(o) => {
                assert_eq!(o.id, "p");
                assert_eq!(o.graph_ref.as_deref(), Some("toolpath://g"));
                assert!(o.base.is_some());
                assert_eq!(o.meta.as_ref().unwrap().title.as_deref(), Some("T"));
            }
            _ => unreachable!(),
        }
        match (&lines[1], &lines[2]) {
            (JsonlLine::ActorDef(a), JsonlLine::ActorDef(z)) => {
                assert_eq!(a.actor, "human:alex");
                assert_eq!(z.actor, "human:zoe");
            }
            _ => unreachable!(),
        }
        match (&lines[4], &lines[5]) {
            (JsonlLine::Step(b), JsonlLine::Signature(sig)) => {
                assert!(b.0.meta.is_none(), "signature moved out of the step body");
                assert_eq!(sig.target, "step:s2");
            }
            _ => unreachable!(),
        }
        match &lines[6] {
            JsonlLine::Head(h) => assert_eq!(h.step_id, "s2"),
            _ => unreachable!(),
        }
        let back = Path::from_jsonl_str(&to_text(&lines)).unwrap();
        assert_eq!(canonical(&back), canonical(&path));
    }

    #[test]
    fn first_send_never_emits_close_or_path_signatures() {
        let path = example(include_str!(
            "../../../../examples/path-03-signed-pr.path.json"
        ));
        assert!(!path.meta.as_ref().unwrap().signatures.is_empty());
        let lines = delta_lines(&path, &HashSet::new(), false).unwrap();
        for l in &lines {
            match l {
                JsonlLine::PathClose(_) => panic!("PathClose emitted"),
                JsonlLine::Signature(s) => assert!(s.target.starts_with("step:")),
                _ => {}
            }
        }
        let back = Path::from_jsonl_str(&to_text(&lines)).unwrap();
        assert_eq!(canonical(&back), canonical(&without_path_signatures(&path)));
    }

    #[test]
    fn incremental_send_line_sequence() {
        let mut actors = HashMap::new();
        actors.insert("human:alex".to_string(), ActorDefinition::default());
        let mut path = path_of(
            vec![step("s1", &[]), step("s2", &["s1"]), step("s3", &["s2"])],
            "s3",
        );
        path.meta = Some(PathMeta {
            title: Some("T2".into()),
            actors: Some(actors),
            extra: [("k".to_string(), json!(1))].into_iter().collect(),
            ..Default::default()
        });

        let lines = delta_lines(&path, &ids(&["s1"]), true).unwrap();
        assert_eq!(
            tags(&lines),
            ["ActorDef", "Step", "Step", "Head", "PathMeta"]
        );
        assert_eq!(step_ids(&lines), ["s2", "s3"]);
        match &lines[4] {
            JsonlLine::PathMeta(m) => {
                assert_eq!(m.patch.title.as_deref(), Some("T2"));
                assert_eq!(m.patch.extra.get("k"), Some(&json!(1)));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn incremental_send_with_nothing_new_sends_only_head() {
        // No meta, so the PathMeta patch would be empty and is skipped.
        let path = path_of(vec![step("s1", &[])], "s1");
        let lines = delta_lines(&path, &ids(&["s1"]), true).unwrap();
        assert_eq!(tags(&lines), ["Head"]);
    }

    #[test]
    fn incremental_send_with_meta_always_patches_refs() {
        let mut path = path_of(vec![step("s1", &[])], "s1");
        path.meta = Some(PathMeta::default());
        let lines = delta_lines(&path, &ids(&["s1"]), true).unwrap();
        assert_eq!(tags(&lines), ["Head", "PathMeta"]);
        match &lines[1] {
            JsonlLine::PathMeta(m) => assert_eq!(m.patch.refs.as_ref().map(Vec::len), Some(0)),
            _ => unreachable!(),
        }
    }

    #[test]
    fn opened_with_nothing_stored_resends_every_step_without_reopening() {
        let empty = path_of(Vec::new(), "s1");
        let first = delta_lines(&empty, &HashSet::new(), false).unwrap();
        assert_eq!(tags(&first), ["PathOpen", "Head"]);
        let mut full = path_of(vec![step("s1", &[])], "s1");
        full.meta = Some(PathMeta::default());
        let second = delta_lines(&full, &HashSet::new(), true).unwrap();
        assert_eq!(tags(&second), ["Step", "Head", "PathMeta"]);
        let back =
            Path::from_jsonl_str(&format!("{}{}", to_text(&first), to_text(&second))).unwrap();
        assert_eq!(back.steps.len(), 1);
        // Not opened again: a second `PathOpen` is a duplicate.
        let again = delta_lines(&full, &HashSet::new(), false).unwrap();
        let err =
            Path::from_jsonl_str(&format!("{}{}", to_text(&first), to_text(&again))).unwrap_err();
        assert!(
            matches!(err, JsonlError::DuplicatePathOpen { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn stored_ids_absent_from_path_are_ignored() {
        let path = path_of(vec![step("s1", &[]), step("s2", &["s1"])], "s2");
        let lines = delta_lines(&path, &ids(&["s1", "ghost"]), true).unwrap();
        assert_eq!(step_ids(&lines), ["s2"]);
    }

    #[test]
    fn new_step_may_hang_off_a_stored_id_absent_from_path() {
        // The reader holds `s0`; the caller's path no longer carries it.
        let path = path_of(vec![step("s1", &["s0"])], "s1");
        let lines = delta_lines(&path, &ids(&["s0"]), true).unwrap();
        assert_eq!(step_ids(&lines), ["s1"]);
    }

    #[test]
    fn document_order_kept_when_parents_first() {
        let path = example(include_str!(
            "../../../../examples/path-04-exploration.path.json"
        ));
        let lines = delta_lines(&path, &HashSet::new(), false).unwrap();
        let doc: Vec<String> = path.steps.iter().map(|s| s.step.id.clone()).collect();
        assert_eq!(step_ids(&lines), doc);
    }

    #[test]
    fn reorders_steps_that_are_not_parents_first() {
        let path = path_of(
            vec![step("s3", &["s2"]), step("s1", &[]), step("s2", &["s1"])],
            "s3",
        );
        let lines = delta_lines(&path, &HashSet::new(), false).unwrap();
        assert_eq!(step_ids(&lines), ["s1", "s2", "s3"]);
        let back = Path::from_jsonl_str(&to_text(&lines)).unwrap();
        assert_eq!(back.steps.len(), 3);
        assert_eq!(back.path.head, "s3");
    }

    #[test]
    fn topological_sort_is_stable() {
        // c and a are both roots.
        let path = path_of(vec![step("b", &["a"]), step("c", &[]), step("a", &[])], "b");
        let lines = delta_lines(&path, &HashSet::new(), false).unwrap();
        assert_eq!(step_ids(&lines), ["c", "a", "b"]);
        let again = delta_lines(&path, &HashSet::new(), false).unwrap();
        assert_eq!(to_text(&lines), to_text(&again));
    }

    #[test]
    fn reorders_only_new_steps() {
        let path = path_of(
            vec![step("s1", &[]), step("s3", &["s2"]), step("s2", &["s1"])],
            "s3",
        );
        let lines = delta_lines(&path, &ids(&["s1"]), true).unwrap();
        assert_eq!(step_ids(&lines), ["s2", "s3"]);
    }

    #[test]
    fn parents_first_orders_a_parent_among_steps_even_when_held() {
        let steps = vec![step("s2", &["s1"]), step("s1", &[])];
        let order: Vec<&str> = parents_first(&steps, &ids(&["s1"]))
            .unwrap()
            .into_iter()
            .map(|s| s.step.id.as_str())
            .collect();
        assert_eq!(order, ["s1", "s2"]);
    }

    #[test]
    fn new_steps_before_stored_ones_in_document_order_are_sent() {
        // A branch learned late sits before stored steps in document order:
        // `b1` hangs off stored `s1`, `b2` off `b1`, `r` is a new root.
        let path = path_of(
            vec![
                step("r", &[]),
                step("s1", &[]),
                step("b2", &["b1"]),
                step("b1", &["s1"]),
                step("s2", &["s1"]),
            ],
            "s2",
        );
        let stored = ids(&["s1", "s2"]);
        let lines = delta_lines(&path, &stored, true).unwrap();
        assert_eq!(step_ids(&lines), ["r", "b1", "b2"]);
        assert_parents_first(&lines, &stored);
    }

    #[test]
    fn dangling_parent_on_incremental_send() {
        let path = path_of(vec![step("s1", &[]), step("s2", &["missing"])], "s2");
        let err = delta_lines(&path, &ids(&["s1"]), true).unwrap_err();
        match err {
            DeltaError::DanglingParent { step, parent } => {
                assert_eq!(step, "s2");
                assert_eq!(parent, "missing");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn dangling_parent_on_first_send() {
        let path = path_of(vec![step("s1", &["s0"])], "s1");
        let err = delta_lines(&path, &HashSet::new(), false).unwrap_err();
        assert!(matches!(
            err,
            DeltaError::DanglingParent { ref step, ref parent } if step == "s1" && parent == "s0"
        ));
        assert!(err.to_string().contains("s0"));
    }

    #[test]
    fn amended_names_the_steps() {
        let err = DeltaError::Amended {
            steps: vec!["s1".into(), "s2".into()],
        };
        assert!(err.to_string().contains(r#"["s1", "s2"]"#), "{err}");
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn delta_error_wraps_jsonl_error() {
        let err: DeltaError = JsonlError::NoSteps.into();
        assert!(matches!(err, DeltaError::Jsonl(JsonlError::NoSteps)));
        // The inner error is the source, not repeated in the message.
        let source = std::error::Error::source(&err).expect("source");
        assert_eq!(source.to_string(), JsonlError::NoSteps.to_string());
        assert!(!err.to_string().contains(&source.to_string()));
    }

    fn cycle_steps(err: DeltaError) -> Vec<String> {
        match err {
            DeltaError::Cycle { steps } => steps,
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn two_step_cycle_on_first_send() {
        let path = path_of(
            vec![
                step("r", &[]),
                step("a", &["b"]),
                step("b", &["a"]),
                step("c", &["b"]),
            ],
            "c",
        );
        let err = delta_lines(&path, &HashSet::new(), false).unwrap_err();
        assert!(err.to_string().contains("\"a\""));
        assert_eq!(cycle_steps(err), ["a", "b"]);
    }

    #[test]
    fn two_step_cycle_on_incremental_send() {
        let path = path_of(
            vec![step("r", &[]), step("a", &["r", "b"]), step("b", &["a"])],
            "b",
        );
        let err = delta_lines(&path, &ids(&["r"]), true).unwrap_err();
        assert_eq!(cycle_steps(err), ["a", "b"]);
    }

    #[test]
    fn self_parent_on_first_send() {
        let path = path_of(vec![step("r", &[]), step("s", &["s"])], "s");
        let err = delta_lines(&path, &HashSet::new(), false).unwrap_err();
        assert_eq!(cycle_steps(err), ["s"]);
    }

    #[test]
    fn self_parent_on_incremental_send() {
        let path = path_of(vec![step("r", &[]), step("s", &["r", "s"])], "s");
        let err = delta_lines(&path, &ids(&["r"]), true).unwrap_err();
        assert_eq!(cycle_steps(err), ["s"]);
    }

    #[test]
    fn readback_equals_one_shot_for_examples() {
        for json in [
            include_str!("../../../../examples/path-01-pr.path.json"),
            include_str!("../../../../examples/path-02-local-session.path.json"),
            include_str!("../../../../examples/path-03-signed-pr.path.json"),
            include_str!("../../../../examples/path-04-exploration.path.json"),
        ] {
            assert_incremental_readback(&example(json));
        }
    }

    #[test]
    fn readback_with_actors_and_meta_arriving_later() {
        let mut full = path_of(
            vec![
                step("s1", &[]),
                Step::new("s2", "agent:claude", "2026-01-01T00:01:00Z")
                    .with_parent("s1")
                    .with_raw_change("a.rs", "@@"),
            ],
            "s2",
        );
        let mut actors = HashMap::new();
        actors.insert(
            "agent:claude".to_string(),
            ActorDefinition {
                name: Some("Claude".into()),
                ..Default::default()
            },
        );
        full.meta = Some(PathMeta {
            title: Some("final title".into()),
            actors: Some(actors),
            extra: [("late".to_string(), json!({"x": 1}))]
                .into_iter()
                .collect(),
            ..Default::default()
        });
        let mut early = prefix(&full, 1);
        early.meta = Some(PathMeta {
            title: Some("draft title".into()),
            ..Default::default()
        });

        let first = delta_lines(&early, &HashSet::new(), false).unwrap();
        let rest = delta_lines(&full, &ids(&["s1"]), true).unwrap();
        let back = Path::from_jsonl_str(&format!("{}{}", to_text(&first), to_text(&rest))).unwrap();
        assert_eq!(canonical(&back), canonical(&full));
    }

    #[test]
    fn readback_with_refs_changed_after_first_send() {
        let r = |rel: &str, href: &str| Ref {
            rel: rel.into(),
            href: href.into(),
        };
        let mut full = path_of(vec![step("s1", &[]), step("s2", &["s1"])], "s2");
        full.meta = Some(PathMeta {
            refs: vec![r("tracks", "issue://2"), r("fixes", "issue://3")],
            ..Default::default()
        });
        let mut early = prefix(&full, 1);
        early.meta = Some(PathMeta {
            refs: vec![r("fixes", "issue://1")],
            ..Default::default()
        });

        let first = delta_lines(&early, &HashSet::new(), false).unwrap();
        let rest = delta_lines(&full, &ids(&["s1"]), true).unwrap();
        match rest.last() {
            Some(JsonlLine::PathMeta(m)) => {
                assert_eq!(m.patch.refs.as_ref().map(Vec::len), Some(2));
            }
            other => panic!("expected a PathMeta patch last, got {other:?}"),
        }
        let back = Path::from_jsonl_str(&format!("{}{}", to_text(&first), to_text(&rest))).unwrap();
        assert_eq!(canonical(&back), canonical(&full));
    }

    #[test]
    fn readback_with_refs_cleared_after_first_send() {
        let mut full = path_of(vec![step("s1", &[]), step("s2", &["s1"])], "s2");
        full.meta = Some(PathMeta {
            title: Some("T".into()),
            ..Default::default()
        });
        let mut early = prefix(&full, 1);
        early.meta = Some(PathMeta {
            title: Some("T".into()),
            refs: vec![Ref {
                rel: "fixes".into(),
                href: "issue://1".into(),
            }],
            ..Default::default()
        });

        let first = delta_lines(&early, &HashSet::new(), false).unwrap();
        let rest = delta_lines(&full, &ids(&["s1"]), true).unwrap();
        let back = Path::from_jsonl_str(&format!("{}{}", to_text(&first), to_text(&rest))).unwrap();
        assert!(back.meta.as_ref().unwrap().refs.is_empty());
        assert_eq!(canonical(&back), canonical(&full));
    }

    /// Deterministic shuffle (LCG) so the generated cases are reproducible.
    fn shuffle<T>(v: &mut [T], mut seed: u64) {
        for i in (1..v.len()).rev() {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let j = (seed >> 33) as usize % (i + 1);
            v.swap(i, j);
        }
    }

    /// Linear chain of `n`.
    fn linear(n: usize) -> Path {
        let steps: Vec<Step> = (0..n)
            .map(|i| {
                let id = format!("l{i}");
                if i == 0 {
                    step(&id, &[])
                } else {
                    step(&id, &[&format!("l{}", i - 1)])
                }
            })
            .collect();
        path_of(steps, &format!("l{}", n - 1))
    }

    /// Trunk of `n` with a dead-end branch of length `n / 2` off step 1.
    fn fork_with_dead_end(n: usize) -> Path {
        let mut p = linear(n);
        for i in 0..n / 2 {
            let id = format!("d{i}");
            let parent = if i == 0 {
                "l1".to_string()
            } else {
                format!("d{}", i - 1)
            };
            p.steps.push(step(&id, &[&parent]));
        }
        p
    }

    /// Root with `branches` independent branches of length `len`, each a
    /// separate tip (no merges); head is the last branch's tip.
    fn multi_branch(branches: usize, len: usize) -> Path {
        let mut steps = vec![step("r", &[])];
        for b in 0..branches {
            for i in 0..len {
                let id = format!("b{b}_{i}");
                let parent = if i == 0 {
                    "r".to_string()
                } else {
                    format!("b{b}_{}", i - 1)
                };
                steps.push(step(&id, &[&parent]));
            }
        }
        path_of(steps, &format!("b{}_{}", branches - 1, len - 1))
    }

    fn generated() -> Vec<Path> {
        vec![
            linear(1),
            linear(6),
            fork_with_dead_end(6),
            fork_with_dead_end(9),
            multi_branch(3, 3),
            multi_branch(4, 2),
        ]
    }

    #[test]
    fn generated_shapes_incremental_readback() {
        for p in generated() {
            assert_incremental_readback(&p);
        }
    }

    #[test]
    fn generated_shapes_shuffled_are_parents_first_and_read_back() {
        for (n, p) in generated().into_iter().enumerate() {
            for seed in 0..8u64 {
                let mut shuffled = p.clone();
                shuffle(&mut shuffled.steps, seed * 7919 + n as u64);

                let lines = delta_lines(&shuffled, &HashSet::new(), false).unwrap();
                assert_parents_first(&lines, &HashSet::new());
                let back = Path::from_jsonl_str(&to_text(&lines)).unwrap();
                let mut got: Vec<String> = back.steps.iter().map(canonical).collect();
                let mut want: Vec<String> = shuffled.steps.iter().map(canonical).collect();
                got.sort();
                want.sort();
                assert_eq!(got, want);
                assert_eq!(back.path.head, shuffled.path.head);

                // Incremental send over the ancestry of some step.
                let pivot = &p.steps[p.steps.len() / 2].step.id;
                let stored = crate::query::ancestors(&shuffled.steps, pivot);
                let rest = delta_lines(&shuffled, &stored, true).unwrap();
                assert_parents_first(&rest, &stored);
                let emitted: HashSet<String> = step_ids(&rest).into_iter().collect();
                let expected: HashSet<String> = shuffled
                    .steps
                    .iter()
                    .map(|s| s.step.id.clone())
                    .filter(|id| !stored.contains(id))
                    .collect();
                assert_eq!(emitted, expected);
            }
        }
    }
}
