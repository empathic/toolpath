//! Splitting a JSONL line stream into request bodies a store applies one at
//! a time, each leaving it holding a valid path.
//!
//! See "Batching" under "Delta Emission" in `docs/RFC-jsonl.md`.

use super::delta::delta_lines;
use super::{DeltaError, JsonlError, JsonlLine};
use crate::types::Path;
use std::collections::{HashMap, HashSet};
use std::fmt;

/// Size limits for one body. `None` is no limit. Build one with
/// [`BatchLimits::new`] or from `default()` (no limits), so that a limit
/// added later is not a breaking change.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchLimits {
    /// Most bytes in a body's text. A body that holds a single step may
    /// exceed it: a step is never split, so a step (with its signatures,
    /// plus the opening lines in the first body and the closing lines in
    /// the last) larger than the limit goes alone in a body over it.
    pub max_bytes: Option<usize>,
    /// Most `Step` lines in a body; `Some(0)` is treated as `Some(1)`.
    pub max_steps: Option<usize>,
}

impl BatchLimits {
    /// At most `max_bytes` bytes and `max_steps` steps per body.
    pub fn new(max_bytes: Option<usize>, max_steps: Option<usize>) -> Self {
        BatchLimits {
            max_bytes,
            max_steps,
        }
    }
}

/// One request body.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Body {
    /// The lines, compact JSON, each followed by `\n`; ends with the `Head`
    /// line (then `PathClose` in the last body when the input closed).
    pub text: String,
    /// Ids of the `Step` lines in this body, in order.
    pub step_ids: Vec<String>,
    /// The step the closing `Head` line names.
    pub head: String,
    /// Id and byte length of the largest `Step` line (signatures excluded),
    /// for reporting a body the store rejects as too large.
    pub largest_step: Option<(String, usize)>,
}

/// What a [`HeadRule::Custom`] rule sees when choosing a body's head.
#[non_exhaustive]
#[derive(Debug)]
pub struct HeadContext<'a> {
    /// The ids the reader holds once this body lands: `held` plus every
    /// step sent so far, this body's included.
    pub held: &'a HashSet<String>,
    /// The body's last step; `None` only for a body with no steps.
    pub last_step: Option<&'a str>,
}

/// How a body that is not the last one chooses its `Head`. The last body
/// takes the input's own `Head` line when it has one.
#[non_exhaustive]
pub enum HeadRule<'a> {
    /// The body's last step.
    LastStep,
    /// The last id in the slice that the reader holds once the body lands,
    /// else the body's last step. Passing the main line in order keeps a
    /// provisional head off side branches.
    LatestIn(&'a [String]),
    /// The caller's choice. It must name an id in [`HeadContext::held`].
    /// The batcher may call it for a body it then closes differently, so
    /// it should depend only on its argument.
    Custom(&'a dyn Fn(&HeadContext<'_>) -> String),
}

impl fmt::Debug for HeadRule<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeadRule::LastStep => f.write_str("LastStep"),
            HeadRule::LatestIn(order) => f.debug_tuple("LatestIn").field(order).finish(),
            HeadRule::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

/// Errors produced by [`batch_lines`].
#[non_exhaustive]
#[derive(Debug)]
pub enum BatchError {
    /// A line could not be serialized.
    Jsonl(JsonlError),
    /// A `PathOpen` at input `index` other than 0.
    PathOpenNotFirst {
        /// Position of the line in the input.
        index: usize,
    },
    /// A step `Signature` at input `index` that does not directly follow
    /// its step (or that step's other signatures).
    OrphanSignature {
        /// Position of the signature line in the input.
        index: usize,
        /// Id of the step the signature names.
        target: String,
    },
    /// A line at input `index` after `PathClose`.
    AfterClose {
        /// Position of the line in the input.
        index: usize,
    },
    /// Two `Step` lines with the same id.
    DuplicateStep {
        /// The repeated step id.
        step: String,
    },
    /// A step whose parent is neither held nor sent before it.
    ParentNotHeld {
        /// The step with the missing parent.
        step: String,
        /// The parent id that is neither held nor sent before it.
        parent: String,
    },
    /// A head that names a step the reader would not hold yet.
    HeadNotHeld {
        /// The step id the head names.
        step: String,
    },
    /// A body with no steps and no way to name a head.
    NoHead,
    /// `actors` or `signatures` as an `extra` key of `PathOpen.meta` or a
    /// `PathMeta` patch; those travel only as their own line kinds.
    ReservedMetaKey {
        /// The reserved key.
        key: String,
    },
}

impl fmt::Display for BatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BatchError::Jsonl(_) => write!(f, "serializing a JSONL line failed"),
            BatchError::PathOpenNotFirst { index } => {
                write!(
                    f,
                    "input index {index}: PathOpen may only be the first line"
                )
            }
            BatchError::OrphanSignature { index, target } => write!(
                f,
                "input index {index}: Signature for {target:?} does not follow its step"
            ),
            BatchError::AfterClose { index } => write!(f, "input index {index}: after PathClose"),
            BatchError::DuplicateStep { step } => write!(f, "step {step:?} is sent twice"),
            BatchError::ParentNotHeld { step, parent } => write!(
                f,
                "step {step:?} is sent before its parent {parent:?}, which is not held"
            ),
            BatchError::HeadNotHeld { step } => {
                write!(f, "head {step:?} is not held when its body lands")
            }
            BatchError::NoHead => write!(f, "a body has no steps and no head"),
            BatchError::ReservedMetaKey { key } => {
                write!(f, "path meta key {key:?} must travel as its own line kind")
            }
        }
    }
}

impl std::error::Error for BatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BatchError::Jsonl(e) => Some(e),
            _ => None,
        }
    }
}

impl From<JsonlError> for BatchError {
    fn from(e: JsonlError) -> Self {
        BatchError::Jsonl(e)
    }
}

/// [`delta_lines`](super::delta_lines) split by [`batch_lines`]: `opened`
/// false opens the path (first body starts with `PathOpen`), `opened` true
/// patches it (first body starts with a `PathMeta` patch when the path has
/// meta), whatever `stored` holds.
/// `stored` is what the reader holds; a store that skips identical resends
/// may be given any subset of it.
///
/// # Errors
///
/// [`DeltaError::DanglingParent`] and [`DeltaError::Cycle`] as for
/// `delta_lines`; [`DeltaError::Batch`] when batching fails (a head not
/// held, say).
pub fn delta_bodies(
    path: &Path,
    stored: &HashSet<String>,
    opened: bool,
    limits: BatchLimits,
    head: &HeadRule<'_>,
) -> Result<Vec<Body>, DeltaError> {
    let lines = delta_lines(path, stored, opened)?;
    batch_lines(lines, stored, limits, head).map_err(DeltaError::Batch)
}

struct Unit {
    id: String,
    text: String,
    step_len: usize,
}

/// Split `lines`, a parents-first stream for a reader that holds the step
/// ids in `held`, into bodies within `limits`. See "Batching" in
/// `docs/RFC-jsonl.md`.
///
/// The first body starts with the opening lines wherever the input has
/// them: `PathOpen`, then `PathMeta` patches, then `ActorDef`s. Each `Step`
/// travels with the step `Signature` lines that follow it. Path-level
/// `Signature`s, the `Head` and `PathClose` end the last body; the input's
/// last `Head` names that body's head (else `head` chooses), and its other
/// `Head` lines are dropped. Every other body ends with the `Head` that
/// `head` chooses. A body other than the only one always holds a step.
///
/// # Errors
///
/// A [`BatchError`] when the input breaks an ordering rule: `PathOpen` not
/// first, a step `Signature` away from its step, a line after `PathClose`,
/// a step sent twice or before a parent that is not held, or a head the
/// reader would not hold.
pub fn batch_lines(
    lines: Vec<JsonlLine>,
    held: &HashSet<String>,
    limits: BatchLimits,
    head: &HeadRule<'_>,
) -> Result<Vec<Body>, BatchError> {
    let mut opening = String::new();
    let mut patches = String::new();
    let mut actors = String::new();
    let mut units: Vec<Unit> = Vec::new();
    let mut path_sigs = String::new();
    let mut close: Option<String> = None;
    let mut final_head: Option<String> = None;
    let mut holds: HashSet<String> = held.clone();
    let mut sent: HashSet<String> = HashSet::new();
    let mut attach = false;

    for (index, line) in lines.into_iter().enumerate() {
        if close.is_some() {
            return Err(BatchError::AfterClose { index });
        }
        let wire = line.to_wire()?;
        let mut follows_step = false;
        match line {
            JsonlLine::PathOpen(b) => {
                if index != 0 {
                    return Err(BatchError::PathOpenNotFirst { index });
                }
                if let Some(m) = &b.meta {
                    reserved(&m.extra)?;
                }
                opening = wire;
            }
            JsonlLine::PathMeta(b) => {
                reserved(&b.patch.extra)?;
                patches.push_str(&wire);
            }
            JsonlLine::ActorDef(_) => actors.push_str(&wire),
            JsonlLine::Step(b) => {
                let step = b.0.step;
                if !sent.insert(step.id.clone()) {
                    return Err(BatchError::DuplicateStep { step: step.id });
                }
                if let Some(parent) = step.parents.iter().find(|p| !holds.contains(*p)) {
                    return Err(BatchError::ParentNotHeld {
                        step: step.id,
                        parent: parent.clone(),
                    });
                }
                holds.insert(step.id.clone());
                units.push(Unit {
                    id: step.id,
                    step_len: wire.len(),
                    text: wire,
                });
                follows_step = true;
            }
            JsonlLine::Signature(b) if b.target == "path" => path_sigs.push_str(&wire),
            JsonlLine::Signature(b) => {
                match units.last_mut() {
                    Some(u) if attach && b.target.strip_prefix("step:") == Some(&u.id) => {
                        u.text.push_str(&wire);
                    }
                    _ => {
                        return Err(BatchError::OrphanSignature {
                            index,
                            target: b.target,
                        });
                    }
                }
                follows_step = true;
            }
            JsonlLine::Head(b) => final_head = Some(b.step_id),
            JsonlLine::PathClose(_) => close = Some(wire),
        }
        attach = follows_step;
    }

    let mut trailer_len = path_sigs.len();
    trailer_len += close.as_ref().map_or(0, String::len);
    let max_steps = limits.max_steps.map(|m| m.max(1));
    let mut picker = Picker::new(head, held);
    let mut holds: HashSet<String> = held.clone();
    let mut bodies = Vec::new();
    let mut cur = Body {
        text: opening + &patches + &actors,
        ..Body::default()
    };
    let mut cur_head: Option<String> = None;
    let n = units.len();

    for (k, unit) in units.into_iter().enumerate() {
        let last = k + 1 == n;
        holds.insert(unit.id.clone());
        picker.add(&unit.id);
        let unit_head = if last {
            match &final_head {
                Some(h) => h.clone(),
                None => picker.pick(&holds, Some(&unit.id))?,
            }
        } else {
            picker.pick(&holds, Some(&unit.id))?
        };
        if !holds.contains(&unit_head) {
            return Err(BatchError::HeadNotHeld { step: unit_head });
        }
        let head_line = JsonlLine::head(&unit_head).to_wire()?;
        let mut need = cur.text.len() + unit.text.len() + head_line.len();
        if last {
            need += trailer_len;
        }
        let over_bytes = limits.max_bytes.is_some_and(|m| need > m);
        let over_steps = max_steps.is_some_and(|m| cur.step_ids.len() >= m);
        if let Some(h) = cur_head.take_if(|_| over_bytes || over_steps) {
            cur.text.push_str(&JsonlLine::head(&h).to_wire()?);
            cur.head = h;
            bodies.push(std::mem::take(&mut cur));
        }
        cur.text.push_str(&unit.text);
        if cur
            .largest_step
            .as_ref()
            .is_none_or(|(_, l)| unit.step_len > *l)
        {
            cur.largest_step = Some((unit.id.clone(), unit.step_len));
        }
        cur.step_ids.push(unit.id);
        cur_head = Some(unit_head);
    }

    let last_head = match cur_head {
        Some(h) => h,
        None => match final_head {
            Some(h) => h,
            None => picker.pick(&holds, None)?,
        },
    };
    if !holds.contains(&last_head) {
        return Err(BatchError::HeadNotHeld { step: last_head });
    }
    cur.text.push_str(&path_sigs);
    cur.text.push_str(&JsonlLine::head(&last_head).to_wire()?);
    if let Some(c) = close {
        cur.text.push_str(&c);
    }
    cur.head = last_head;
    bodies.push(cur);
    Ok(bodies)
}

fn reserved(extra: &HashMap<String, serde_json::Value>) -> Result<(), BatchError> {
    match ["actors", "signatures"]
        .into_iter()
        .find(|k| extra.contains_key(*k))
    {
        Some(key) => Err(BatchError::ReservedMetaKey { key: key.into() }),
        None => Ok(()),
    }
}

/// [`HeadRule`] state. `LatestIn` tracks the highest position held so far,
/// which only grows because the held set only grows.
struct Picker<'r, 'a> {
    rule: &'r HeadRule<'a>,
    position: HashMap<&'a str, usize>,
    best: Option<usize>,
}

impl<'r, 'a> Picker<'r, 'a> {
    fn new(rule: &'r HeadRule<'a>, held: &HashSet<String>) -> Self {
        let mut picker = Picker {
            rule,
            position: HashMap::new(),
            best: None,
        };
        if let HeadRule::LatestIn(order) = rule {
            picker.position = order
                .iter()
                .enumerate()
                .map(|(i, id)| (id.as_str(), i))
                .collect();
            for id in held {
                picker.add(id);
            }
        }
        picker
    }

    fn add(&mut self, id: &str) {
        if let Some(&p) = self.position.get(id) {
            self.best = self.best.max(Some(p));
        }
    }

    fn pick(&self, held: &HashSet<String>, last_step: Option<&str>) -> Result<String, BatchError> {
        let chosen = match self.rule {
            HeadRule::LastStep => last_step.map(str::to_string),
            HeadRule::LatestIn(order) => self
                .best
                .map(|i| order[i].clone())
                .or_else(|| last_step.map(str::to_string)),
            HeadRule::Custom(f) => Some(f(&HeadContext { held, last_step })),
        };
        chosen.ok_or(BatchError::NoHead)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonl::delta_lines;
    use crate::jsonl::{PathCloseBody, PathOpenBody, parents_first};
    use crate::types::{ActorDefinition, Base, PathIdentity, PathMeta, Signature, Step, StepMeta};
    use serde_json::json;

    fn step(id: &str, parents: &[&str], size: usize) -> Step {
        let mut s = Step::new(id, "human:alex", "2026-01-01T00:00:00Z")
            .with_raw_change("src/main.rs", "x".repeat(size));
        for p in parents {
            s = s.with_parent(*p);
        }
        s
    }

    fn sig(s: &str) -> Signature {
        Signature {
            signer: "human:alex".into(),
            key: "gpg:A".into(),
            scope: "author".into(),
            sig: s.into(),
            timestamp: None,
        }
    }

    fn signed(mut s: Step, n: usize) -> Step {
        s.meta = Some(StepMeta {
            signatures: (0..n).map(|i| sig(&format!("S{i}"))).collect(),
            ..Default::default()
        });
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

    fn with_meta(mut p: Path) -> Path {
        let mut actors = HashMap::new();
        actors.insert("human:alex".to_string(), ActorDefinition::default());
        p.meta = Some(PathMeta {
            title: Some("T".into()),
            actors: Some(actors),
            extra: [("k".to_string(), json!(1))].into_iter().collect(),
            signatures: vec![sig("P")],
            ..Default::default()
        });
        p
    }

    fn linear(n: usize, size: usize) -> Path {
        let steps = (0..n)
            .map(|i| {
                let id = format!("s{i}");
                let parent = format!("s{}", i.wrapping_sub(1));
                if i == 0 {
                    step(&id, &[], size)
                } else {
                    step(&id, &[&parent], size)
                }
            })
            .collect();
        path_of(steps, &format!("s{}", n - 1))
    }

    fn canonical<T: serde::Serialize>(v: &T) -> String {
        serde_json::to_string(&serde_json::to_value(v).unwrap()).unwrap()
    }

    fn joined(bodies: &[Body]) -> String {
        bodies.iter().map(|b| b.text.as_str()).collect()
    }

    fn parse(body: &Body) -> Vec<JsonlLine> {
        body.text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn none() -> HashSet<String> {
        HashSet::new()
    }

    /// The rules every result keeps: `PathOpen` only as the first line of
    /// the first body; parents held or sent before; each body ends with a
    /// `Head` (then optionally `PathClose`) naming a step held by then; a
    /// body over `max_bytes` holds one step; `step_ids` match the lines.
    fn assert_rules(bodies: &[Body], held: &HashSet<String>, limits: BatchLimits) {
        let mut holds = held.clone();
        for (bi, body) in bodies.iter().enumerate() {
            let lines = parse(body);
            let mut ids = Vec::new();
            for (li, line) in lines.iter().enumerate() {
                match line {
                    JsonlLine::PathOpen(_) => assert!(bi == 0 && li == 0, "PathOpen at {bi}:{li}"),
                    JsonlLine::Step(s) => {
                        for p in &s.0.step.parents {
                            assert!(holds.contains(p), "{} before parent {p}", s.0.step.id);
                        }
                        holds.insert(s.0.step.id.clone());
                        ids.push(s.0.step.id.clone());
                    }
                    _ => {}
                }
            }
            assert_eq!(ids, body.step_ids);
            let tail: Vec<&JsonlLine> = lines
                .iter()
                .rev()
                .filter(|l| !matches!(l, JsonlLine::PathClose(_)))
                .take(1)
                .collect();
            match tail[0] {
                JsonlLine::Head(h) => {
                    assert_eq!(h.step_id, body.head);
                    assert!(holds.contains(&h.step_id), "head {} not held", h.step_id);
                }
                other => panic!("body {bi} ends with {other:?}"),
            }
            if let Some(m) = limits.max_bytes
                && body.text.len() > m
            {
                assert!(body.step_ids.len() <= 1, "body {bi} over budget");
            }
            if let Some(m) = limits.max_steps {
                assert!(body.step_ids.len() <= m.max(1));
            }
            if bi + 1 < bodies.len() {
                assert!(!body.step_ids.is_empty());
            }
        }
    }

    #[test]
    fn one_body_without_limits_equals_one_shot_write() {
        let path = with_meta(linear(4, 10));
        let bodies = batch_lines(
            path.to_jsonl_lines(),
            &none(),
            BatchLimits::default(),
            &HeadRule::LastStep,
        )
        .unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].text, path.to_jsonl_string().unwrap());
    }

    #[test]
    fn byte_budget_respected_and_reads_back() {
        let path = with_meta(linear(20, 200));
        let limits = BatchLimits {
            max_bytes: Some(1000),
            max_steps: None,
        };
        let bodies =
            batch_lines(path.to_jsonl_lines(), &none(), limits, &HeadRule::LastStep).unwrap();
        assert!(bodies.len() > 3);
        assert_rules(&bodies, &none(), limits);
        for b in &bodies {
            assert!(b.text.len() <= 1000, "{}", b.text.len());
        }
        let back = Path::from_jsonl_str(&joined(&bodies)).unwrap();
        assert_eq!(canonical(&back), canonical(&path));
    }

    #[test]
    fn oversized_step_goes_alone() {
        let mut path = linear(5, 50);
        path.steps[2] = signed(step("s2", &["s1"], 5000), 2);
        let limits = BatchLimits {
            max_bytes: Some(600),
            max_steps: None,
        };
        let bodies =
            batch_lines(path.to_jsonl_lines(), &none(), limits, &HeadRule::LastStep).unwrap();
        assert_rules(&bodies, &none(), limits);
        let big = bodies
            .iter()
            .find(|b| b.step_ids.contains(&"s2".to_string()))
            .unwrap();
        assert_eq!(big.step_ids, ["s2"]);
        assert!(big.text.len() > 600);
        assert_eq!(big.largest_step.as_ref().unwrap().0, "s2");
        assert_eq!(big.text.matches(r#"{"Signature":"#).count(), 2);
        let back = Path::from_jsonl_str(&joined(&bodies)).unwrap();
        assert_eq!(canonical(&back), canonical(&path));
    }

    #[test]
    fn max_steps_splits_and_last_body_takes_the_real_head() {
        let mut path = linear(7, 10);
        path.path.head = "s3".into();
        let limits = BatchLimits {
            max_bytes: None,
            max_steps: Some(3),
        };
        let bodies =
            batch_lines(path.to_jsonl_lines(), &none(), limits, &HeadRule::LastStep).unwrap();
        let sizes: Vec<usize> = bodies.iter().map(|b| b.step_ids.len()).collect();
        assert_eq!(sizes, [3, 3, 1]);
        let heads: Vec<&str> = bodies.iter().map(|b| b.head.as_str()).collect();
        assert_eq!(heads, ["s2", "s5", "s3"]);
        assert!(bodies[2].text.ends_with("{\"PathClose\":{}}\n"));
        assert_rules(&bodies, &none(), limits);
    }

    #[test]
    fn opening_lines_stay_with_the_first_step() {
        let path = with_meta(linear(3, 10));
        let limits = BatchLimits {
            max_bytes: Some(1),
            max_steps: None,
        };
        let bodies =
            batch_lines(path.to_jsonl_lines(), &none(), limits, &HeadRule::LastStep).unwrap();
        assert_eq!(bodies.len(), 3);
        let first = parse(&bodies[0]);
        assert!(matches!(first[0], JsonlLine::PathOpen(_)));
        assert!(matches!(first[1], JsonlLine::ActorDef(_)));
        assert!(matches!(first[2], JsonlLine::Step(_)));
        let last = parse(&bodies[2]);
        let n = last.len();
        assert!(matches!(last[n - 3], JsonlLine::Signature(ref s) if s.target == "path"));
        assert!(matches!(last[n - 2], JsonlLine::Head(_)));
        assert!(matches!(last[n - 1], JsonlLine::PathClose(_)));
    }

    #[test]
    fn incremental_send_puts_the_meta_patch_first() {
        let path = with_meta(linear(6, 10));
        let stored: HashSet<String> = ["s0", "s1"].iter().map(|s| s.to_string()).collect();
        let limits = BatchLimits {
            max_bytes: None,
            max_steps: Some(2),
        };
        let bodies = delta_bodies(&path, &stored, true, limits, &HeadRule::LastStep).unwrap();
        assert_eq!(bodies.len(), 2);
        let first = parse(&bodies[0]);
        assert!(matches!(first[0], JsonlLine::PathMeta(_)));
        assert!(matches!(first[1], JsonlLine::ActorDef(_)));
        assert!(!bodies[1].text.contains("PathMeta"));
        assert_rules(&bodies, &stored, limits);
    }

    #[test]
    fn opened_with_nothing_stored_resends_everything_as_a_patch() {
        let path = with_meta(linear(3, 10));
        let bodies = delta_bodies(
            &path,
            &none(),
            true,
            BatchLimits::default(),
            &HeadRule::LastStep,
        )
        .unwrap();
        let lines = parse(&bodies[0]);
        assert!(matches!(lines[0], JsonlLine::PathMeta(_)));
        assert!(!bodies[0].text.contains("PathOpen"));
        assert_eq!(bodies[0].step_ids, ["s0", "s1", "s2"]);
    }

    #[test]
    fn continuation_opens_a_path_whose_roots_hang_off_held_steps() {
        let path = linear(4, 10);
        let stored: HashSet<String> = ["s0", "s1"].iter().map(|s| s.to_string()).collect();
        let bodies = delta_bodies(
            &path,
            &stored,
            false,
            BatchLimits::default(),
            &HeadRule::LastStep,
        )
        .unwrap();
        assert!(bodies[0].text.starts_with(r#"{"PathOpen":"#));
        assert_eq!(bodies[0].step_ids, ["s2", "s3"]);
    }

    #[test]
    fn nothing_new_is_one_body_with_the_head() {
        let path = with_meta(linear(2, 10));
        let stored: HashSet<String> = ["s0", "s1"].iter().map(|s| s.to_string()).collect();
        let bodies = delta_bodies(
            &path,
            &stored,
            true,
            BatchLimits {
                max_bytes: Some(1),
                max_steps: Some(1),
            },
            &HeadRule::LastStep,
        )
        .unwrap();
        assert_eq!(bodies.len(), 1);
        assert!(bodies[0].step_ids.is_empty());
        assert_eq!(bodies[0].head, "s1");
    }

    /// Trunk `t0..t5` with a side branch `b0..b3` off `t1`, document order
    /// interleaving them, head `t5`.
    fn branched() -> Path {
        let steps = vec![
            step("t0", &[], 10),
            step("t1", &["t0"], 10),
            step("b0", &["t1"], 10),
            step("t2", &["t1"], 10),
            step("b1", &["b0"], 10),
            step("b2", &["b1"], 10),
            step("t3", &["t2"], 10),
            step("b3", &["b2"], 10),
            step("t4", &["t3"], 10),
            step("t5", &["t4"], 10),
        ];
        path_of(steps, "t5")
    }

    #[test]
    fn latest_in_keeps_provisional_heads_on_the_main_line() {
        let path = branched();
        let main: Vec<String> = (0..6).map(|i| format!("t{i}")).collect();
        let limits = BatchLimits {
            max_bytes: None,
            max_steps: Some(2),
        };
        let bodies =
            delta_bodies(&path, &none(), false, limits, &HeadRule::LatestIn(&main)).unwrap();
        let heads: Vec<&str> = bodies.iter().map(|b| b.head.as_str()).collect();
        assert_eq!(heads, ["t1", "t2", "t2", "t3", "t5"]);
        let last_step: Vec<String> =
            delta_bodies(&path, &none(), false, limits, &HeadRule::LastStep)
                .unwrap()
                .into_iter()
                .map(|b| b.head)
                .collect();
        assert_eq!(last_step, ["t1", "t2", "b2", "b3", "t5"]);
        assert_rules(&bodies, &none(), limits);
    }

    #[test]
    fn latest_in_counts_held_steps() {
        let path = branched();
        let main: Vec<String> = (0..6).map(|i| format!("t{i}")).collect();
        let stored: HashSet<String> = ["t0", "t1", "t2", "t3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let limits = BatchLimits {
            max_bytes: None,
            max_steps: Some(1),
        };
        let bodies =
            delta_bodies(&path, &stored, true, limits, &HeadRule::LatestIn(&main)).unwrap();
        let heads: Vec<&str> = bodies.iter().map(|b| b.head.as_str()).collect();
        // b0..b3 go first; the head stays t3 until t4 lands.
        assert_eq!(heads, ["t3", "t3", "t3", "t3", "t4", "t5"]);
    }

    #[test]
    fn custom_head_must_be_held() {
        let path = linear(4, 10);
        let limits = BatchLimits {
            max_bytes: None,
            max_steps: Some(2),
        };
        let good = |c: &HeadContext<'_>| -> String {
            assert!(c.last_step.is_some());
            "s0".to_string()
        };
        let bodies = delta_bodies(&path, &none(), false, limits, &HeadRule::Custom(&good)).unwrap();
        assert_eq!(bodies[0].head, "s0");
        assert_eq!(bodies[1].head, "s3");
        let bad = |_: &HeadContext<'_>| "s3".to_string();
        let err = delta_bodies(&path, &none(), false, limits, &HeadRule::Custom(&bad)).unwrap_err();
        assert!(
            matches!(err, DeltaError::Batch(BatchError::HeadNotHeld { ref step }) if step == "s3"),
            "{err:?}"
        );
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn final_head_not_held_is_an_error() {
        let mut path = linear(2, 10);
        path.path.head = "ghost".into();
        let err = batch_lines(
            path.to_jsonl_lines(),
            &none(),
            BatchLimits::default(),
            &HeadRule::LastStep,
        )
        .unwrap_err();
        assert!(matches!(err, BatchError::HeadNotHeld { ref step } if step == "ghost"));
    }

    #[test]
    fn stepless_first_send_has_no_head() {
        let lines = vec![JsonlLine::PathOpen(PathOpenBody::for_path(&linear(1, 1)))];
        let err =
            batch_lines(lines, &none(), BatchLimits::default(), &HeadRule::LastStep).unwrap_err();
        assert!(matches!(err, BatchError::NoHead));
    }

    fn lines_of(path: &Path) -> Vec<JsonlLine> {
        delta_lines(path, &none(), false).unwrap()
    }

    #[test]
    fn structural_errors() {
        let path = with_meta(linear(3, 10));
        let run = |lines: Vec<JsonlLine>, held: &HashSet<String>| {
            batch_lines(lines, held, BatchLimits::default(), &HeadRule::LastStep).unwrap_err()
        };

        let mut l = lines_of(&path);
        l.swap(0, 1);
        assert!(matches!(
            run(l, &none()),
            BatchError::PathOpenNotFirst { index: 1 }
        ));

        let mut l = lines_of(&path);
        l.push(JsonlLine::PathClose(PathCloseBody {}));
        l.push(JsonlLine::head("s2"));
        assert!(matches!(run(l, &none()), BatchError::AfterClose { .. }));

        let mut l = lines_of(&path);
        let dup = l
            .iter()
            .find(|x| matches!(x, JsonlLine::Step(_)))
            .unwrap()
            .clone();
        l.insert(l.len() - 1, dup);
        assert!(matches!(run(l, &none()), BatchError::DuplicateStep { ref step } if step == "s0"));

        let l = lines_of(&path);
        let without_first: Vec<JsonlLine> = l
            .into_iter()
            .filter(|x| !matches!(x, JsonlLine::Step(s) if s.0.step.id == "s0"))
            .collect();
        assert!(matches!(
            run(without_first, &none()),
            BatchError::ParentNotHeld { ref step, ref parent } if step == "s1" && parent == "s0"
        ));

        let mut signed_path = linear(3, 10);
        signed_path.steps[0] = signed(step("s0", &[], 10), 1);
        let mut l = lines_of(&signed_path);
        let at = l
            .iter()
            .position(|x| matches!(x, JsonlLine::Signature(_)))
            .unwrap();
        let s = l.remove(at);
        l.insert(l.len() - 1, s);
        assert!(matches!(
            run(l, &none()),
            BatchError::OrphanSignature { ref target, .. } if target == "step:s0"
        ));

        for key in ["actors", "signatures"] {
            let mut p = linear(1, 1);
            p.meta = Some(PathMeta {
                extra: [(key.to_string(), json!({}))].into_iter().collect(),
                ..Default::default()
            });
            assert!(matches!(
                run(lines_of(&p), &none()),
                BatchError::ReservedMetaKey { key: ref k } if k == key
            ));
            let held: HashSet<String> = ["s0".to_string()].into();
            let l = delta_lines(&p, &held, true).unwrap();
            assert!(matches!(run(l, &held), BatchError::ReservedMetaKey { .. }));
        }
        assert!(BatchError::NoHead.to_string().contains("no head"));
    }

    #[test]
    fn errors_name_the_input_position_unambiguously() {
        let msg = BatchError::PathOpenNotFirst { index: 1 }.to_string();
        assert!(!msg.starts_with("line 1:"), "{msg}");
        assert!(msg.contains("index 1"), "{msg}");
    }

    // ── Generated DAGs ──────────────────────────────────────────────────

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }

        fn below(&mut self, n: usize) -> usize {
            self.next() as usize % n.max(1)
        }
    }

    /// `n` steps; each after the first takes one or two random earlier
    /// steps as parents (sometimes none); random sizes and signatures;
    /// document order shuffled; head at a random step.
    fn random_path(rng: &mut Lcg, n: usize) -> Path {
        let mut steps = Vec::with_capacity(n);
        for i in 0..n {
            let id = format!("n{i}");
            let mut parents: Vec<String> = Vec::new();
            if i > 0 && rng.below(8) != 0 {
                parents.push(format!("n{}", rng.below(i)));
                if rng.below(4) == 0 {
                    let q = format!("n{}", rng.below(i));
                    if !parents.contains(&q) {
                        parents.push(q);
                    }
                }
            }
            let refs: Vec<&str> = parents.iter().map(String::as_str).collect();
            let s = step(&id, &refs, 1 + rng.below(400));
            let sigs = if rng.below(5) == 0 {
                1 + rng.below(2)
            } else {
                0
            };
            steps.push(if sigs > 0 { signed(s, sigs) } else { s });
        }
        for i in (1..steps.len()).rev() {
            steps.swap(i, rng.below(i + 1));
        }
        let head = steps[rng.below(n)].step.id.clone();
        let mut p = path_of(steps, &head);
        if rng.below(2) == 0 {
            p = with_meta(p);
        }
        p
    }

    fn random_limits(rng: &mut Lcg) -> BatchLimits {
        BatchLimits {
            max_bytes: [None, Some(1), Some(300), Some(900), Some(4000)][rng.below(5)],
            max_steps: [None, Some(1), Some(3), Some(7)][rng.below(4)],
        }
    }

    fn sorted_steps(p: &Path) -> Vec<String> {
        let mut v: Vec<String> = p.steps.iter().map(canonical).collect();
        v.sort();
        v
    }

    fn assert_same_path(back: &Path, want: &Path) {
        assert_eq!(sorted_steps(back), sorted_steps(want));
        assert_eq!(back.path.head, want.path.head);
        assert_eq!(canonical(&back.path), canonical(&want.path));
        assert_eq!(canonical(&back.meta), canonical(&want.meta));
    }

    fn without_path_signatures(p: &Path) -> Path {
        let mut p = p.clone();
        if let Some(m) = p.meta.as_mut() {
            m.signatures.clear();
        }
        p
    }

    #[test]
    fn generated_whole_path_uploads() {
        let mut rng = Lcg(7);
        for case in 0..200 {
            let n = 1 + rng.below(40);
            let path = random_path(&mut rng, n);
            let limits = random_limits(&mut rng);
            let ordered = Path {
                steps: parents_first_owned(&path),
                ..path.clone()
            };
            let bodies = batch_lines(
                ordered.to_jsonl_lines(),
                &none(),
                limits,
                &HeadRule::LastStep,
            )
            .unwrap_or_else(|e| panic!("case {case}: {e}"));
            assert_rules(&bodies, &none(), limits);
            let back = Path::from_jsonl_str(&joined(&bodies)).unwrap();
            assert_same_path(&back, &path);
            assert_eq!(canonical(&back), canonical(&ordered), "case {case}");
        }
    }

    fn parents_first_owned(p: &Path) -> Vec<Step> {
        parents_first(&p.steps, &none())
            .unwrap()
            .into_iter()
            .cloned()
            .collect()
    }

    #[test]
    fn generated_deltas_in_two_sends() {
        let mut rng = Lcg(11);
        for case in 0..200 {
            let n = 2 + rng.below(40);
            let path = random_path(&mut rng, n);
            let pivot = path.steps[rng.below(n)].step.id.clone();
            let stored = crate::query::ancestors(&path.steps, &pivot);
            let early = Path {
                path: PathIdentity {
                    head: pivot.clone(),
                    ..path.path.clone()
                },
                steps: path
                    .steps
                    .iter()
                    .filter(|s| stored.contains(&s.step.id))
                    .cloned()
                    .collect(),
                meta: path.meta.clone(),
            };
            let limits = random_limits(&mut rng);
            let main: Vec<String> = {
                let anc = crate::query::ancestors(&path.steps, &path.path.head);
                parents_first_owned(&path)
                    .into_iter()
                    .map(|s| s.step.id)
                    .filter(|id| anc.contains(id))
                    .collect()
            };
            let rule = if rng.below(2) == 0 {
                HeadRule::LastStep
            } else {
                HeadRule::LatestIn(&main)
            };
            let first = delta_bodies(&early, &none(), false, limits, &rule)
                .unwrap_or_else(|e| panic!("case {case}: {e}"));
            assert_rules(&first, &none(), limits);
            let rest = delta_bodies(&path, &stored, true, limits, &rule)
                .unwrap_or_else(|e| panic!("case {case}: {e}"));
            assert_rules(&rest, &stored, limits);
            let back =
                Path::from_jsonl_str(&format!("{}{}", joined(&first), joined(&rest))).unwrap();
            assert_same_path(&back, &without_path_signatures(&path));
        }
    }
}
