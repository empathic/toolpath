//! Classify a round trip's lost and gained key paths (pr/78), in report mode: it counts, it does
//! not fail.
//!
//! A **lost** path is
//! * *dropped*: the target writer's contract (`fixtures/contract/<target>.json`) has a `drop` op
//!   covering it;
//! * *renamed*: a `rename` or `move` op names it as the `from` side (its value comes back under
//!   another key);
//! * *source* bookkeeping: it lies under the recording agent's own envelope
//!   (`fixtures/source-bookkeeping.json`);
//! * else *unexplained*.
//!
//! A **gained** path is
//! * *synthesized*: a `synthesize` op covers it, or it lies under the envelope the TARGET writer
//!   mints for every session (`fixtures/target-synthesized.json`);
//! * *renamed*: the `to` side of a `rename` or `move` op;
//! * else *unexplained*.
//!
//! "Covers" means the path itself or anything under it.

use std::collections::BTreeSet;

use crate::transform::{Contract, Op};

/// Whether `path` is `declared` itself or lies under it (`a.b` covers `a.b.c` and `a.b[]`).
pub fn covers(declared: &str, path: &str) -> bool {
    path == declared
        || path
            .strip_prefix(declared)
            .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
}

/// Counts for one round trip.
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct Classified {
    pub lost_dropped: usize,
    pub lost_renamed: usize,
    pub lost_source: usize,
    pub lost_unexplained: usize,
    pub gained_synthesized: usize,
    pub gained_renamed: usize,
    pub gained_unexplained: usize,
}

/// One source agent's own envelope prefix (`fixtures/source-bookkeeping.json`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBookkeeping {
    pub source: String,
    pub path: String,
    pub why: String,
}

/// One target writer's minted envelope prefix (`fixtures/target-synthesized.json`): paths the
/// writer creates for every session, which no source had.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetSynthesized {
    pub target: String,
    pub path: String,
    pub why: String,
}

/// What a round trip is classified against.
pub struct Declared<'a> {
    pub contract: &'a Contract,
    pub source: &'a str,
    pub source_bookkeeping: &'a [SourceBookkeeping],
    pub target_synthesized: &'a [TargetSynthesized],
}

/// The paths left with no explanation.
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct Unexplained {
    pub lost: Vec<String>,
    pub gained: Vec<String>,
}

fn any_covers<'a>(declared: impl IntoIterator<Item = &'a str>, path: &str) -> bool {
    declared.into_iter().any(|d| covers(d, path))
}

/// Classify `lost` and `gained`; also the paths nothing explains.
pub fn classify_paths(
    d: &Declared,
    lost: &BTreeSet<String>,
    gained: &BTreeSet<String>,
) -> (Classified, Unexplained) {
    let (mut dropped, mut renamed_from, mut renamed_to, mut synth) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for op in &d.contract.ops {
        match op {
            Op::Drop { path, .. } => dropped.push(path.as_str()),
            Op::Rename { from, to } | Op::Move { from, to } => {
                renamed_from.push(from.as_str());
                renamed_to.push(to.as_str());
            }
            Op::Synthesize { path, .. } => synth.push(path.as_str()),
            Op::Keep { .. } | Op::Convert { .. } => {}
        }
    }
    let source = || {
        d.source_bookkeeping
            .iter()
            .filter(|b| b.source == d.source)
            .map(|b| b.path.as_str())
    };
    let minted = || {
        d.target_synthesized
            .iter()
            .filter(|b| b.target == d.contract.writer)
            .map(|b| b.path.as_str())
    };
    let mut c = Classified::default();
    let mut u = Unexplained::default();
    for p in lost {
        if any_covers(dropped.iter().copied(), p) {
            c.lost_dropped += 1;
        } else if any_covers(renamed_from.iter().copied(), p) {
            c.lost_renamed += 1;
        } else if any_covers(source(), p) {
            c.lost_source += 1;
        } else {
            c.lost_unexplained += 1;
            u.lost.push(p.clone());
        }
    }
    for p in gained {
        if any_covers(synth.iter().copied(), p) || any_covers(minted(), p) {
            c.gained_synthesized += 1;
        } else if any_covers(renamed_to.iter().copied(), p) {
            c.gained_renamed += 1;
        } else {
            c.gained_unexplained += 1;
            u.gained.push(p.clone());
        }
    }
    (c, u)
}

/// [`classify_paths`], counts only.
pub fn classify(d: &Declared, lost: &BTreeSet<String>, gained: &BTreeSet<String>) -> Classified {
    classify_paths(d, lost, gained).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract() -> Contract {
        serde_json::from_str(
            r#"{"writer":"x","ops":[
              {"op":"drop","path":"a.b","why":"w"},
              {"op":"rename","from":"c","to":"d"},
              {"op":"synthesize","path":"e","why":"w"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn a_declared_path_covers_what_lies_under_it_and_nothing_beside_it() {
        assert!(covers("a.b", "a.b"));
        assert!(covers("a.b", "a.b.c"));
        assert!(covers("a.b", "a.b[]"));
        assert!(!covers("a.b", "a.bc"));
        assert!(!covers("a.b", "a"));
    }

    #[test]
    fn every_class_is_counted_apart_and_only_unexplained_paths_are_listed() {
        let lost: BTreeSet<String> = ["a.b.x", "c", "env.k", "z"].map(String::from).into();
        let gained: BTreeSet<String> = ["d", "e", "m.k", "q"].map(String::from).into();
        let book = vec![SourceBookkeeping {
            source: "s".into(),
            path: "env".into(),
            why: "w".into(),
        }];
        let synth = vec![TargetSynthesized {
            target: "x".into(),
            path: "m".into(),
            why: "w".into(),
        }];
        let c = contract();
        let d = |source| Declared {
            contract: &c,
            source,
            source_bookkeeping: &book,
            target_synthesized: &synth,
        };
        // another source's bookkeeping does not excuse this source's loss
        assert_eq!(classify(&d("t"), &lost, &gained).lost_source, 0);
        let (counts, unexplained) = classify_paths(&d("s"), &lost, &gained);
        assert_eq!(
            counts,
            Classified {
                lost_dropped: 1,
                lost_renamed: 1,
                lost_source: 1,
                lost_unexplained: 1,
                gained_synthesized: 2,
                gained_renamed: 1,
                gained_unexplained: 1
            }
        );
        assert_eq!(unexplained.lost, vec!["z".to_string()]);
        assert_eq!(unexplained.gained, vec!["q".to_string()]);
    }
}
