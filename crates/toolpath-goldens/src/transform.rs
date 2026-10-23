//! A writer's contract as a small transform IR: what projecting a document
//! for one agent, and reading it back with that agent's adapter, does to each
//! key path. Written up front (`fixtures/contract/<agent>.json`) and checked
//! against what the `writer@<agent>` stage measures, in both directions: a
//! measured loss or gain no op explains fails, and so does an op with no
//! effect.
//!
//! The ops are ours. `schemars` only describes their shape
//! (`docs/baseline/schemas/transform.schema.json`); it does not run them.
//! Paths are generalised key paths as [`crate::keypaths::key_paths`] writes
//! them (`paths[].steps[].change.<artifact>.structural.text`).

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::keypaths::key_paths;

/// One writer's declared transform.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    /// The agent whose writer (and reader) this describes.
    pub writer: String,
    /// What happens to each key path that changes. Paths not named are
    /// expected to survive unchanged.
    pub ops: Vec<Op>,
}

/// What a writer does to one key path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    /// The path survives. Only needed to state it on purpose; checked to be
    /// present before and after.
    Keep { path: String },
    /// The path is gone after the round trip. `defect` marks a loss that is
    /// a bug to fix, not inherent to the target format.
    Drop {
        path: String,
        why: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defect: Option<String>,
    },
    /// The same value comes back under another key (a field the target
    /// names differently).
    Rename { from: String, to: String },
    /// The same value comes back somewhere else in the document.
    Move { from: String, to: String },
    /// The writer invents a value the source never had; it comes back under
    /// `path`. With `value`, every occurrence equals it; without, the value
    /// varies (per call, per turn) and only its presence is checked. `defect`
    /// marks an invention that is a bug to fix.
    Synthesize {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Value>,
        why: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defect: Option<String>,
    },
    /// The path survives with its value converted, by a named conversion
    /// (`how` names it; it is not code).
    Convert { path: String, how: String },
}

/// What one writer stage measured: lost and gained key paths, and the
/// document after the round trip.
pub struct Measured<'a> {
    pub before: &'a Value,
    pub after: &'a Value,
    pub lost: &'a BTreeSet<String>,
    pub gained: &'a BTreeSet<String>,
}

/// Every way `measured` and `contract` disagree, empty when they match.
pub fn check(contract: &Contract, m: &Measured) -> Vec<String> {
    let mut out = Vec::new();
    let mut lost: BTreeSet<String> = BTreeSet::new();
    let mut gained: BTreeSet<String> = BTreeSet::new();
    let before_paths: BTreeSet<String> = key_paths(m.before)
        .into_iter()
        .map(|k| k.as_str().to_owned())
        .collect();
    let after_paths: BTreeSet<String> = key_paths(m.after)
        .into_iter()
        .map(|k| k.as_str().to_owned())
        .collect();
    for op in &contract.ops {
        match op {
            Op::Keep { path } | Op::Convert { path, .. } => {
                if !before_paths.contains(path) || !after_paths.contains(path) {
                    out.push(format!(
                        "{op:?}: {path} is not present both before and after"
                    ));
                }
            }
            Op::Drop { path, .. } => {
                lost.insert(path.clone());
            }
            Op::Rename { from, to } | Op::Move { from, to } => {
                lost.insert(from.clone());
                gained.insert(to.clone());
            }
            Op::Synthesize { path, value, .. } => {
                gained.insert(path.clone());
                let seen = values_at(m.after, path);
                if seen.is_empty() {
                    out.push(format!("synthesize {path}: no value after the round trip"));
                }
                if let Some(value) = value {
                    for v in seen {
                        if &v != value {
                            out.push(format!("synthesize {path}: declared {value}, found {v}"));
                        }
                    }
                }
            }
        }
    }
    for p in m.lost.difference(&lost) {
        out.push(format!("lost but not declared: {p}"));
    }
    for p in lost.difference(m.lost) {
        out.push(format!("declared lost but kept: {p}"));
    }
    for p in m.gained.difference(&gained) {
        out.push(format!("gained but not declared: {p}"));
    }
    for p in gained.difference(m.gained) {
        out.push(format!("declared gained but absent: {p}"));
    }
    out
}

/// Every value at a generalised key path (array indices collapsed, artifact
/// keys as `<artifact>`, actor keys as `<actor>`).
pub fn values_at(doc: &Value, path: &str) -> Vec<Value> {
    fn walk(prefix: &str, v: &Value, want: &str, out: &mut Vec<Value>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    let seg = if k.contains("://") {
                        "<artifact>"
                    } else if prefix.ends_with("actors") {
                        "<actor>"
                    } else {
                        k.as_str()
                    };
                    walk(&format!("{prefix}.{seg}"), x, want, out);
                }
            }
            Value::Array(a) => a
                .iter()
                .for_each(|x| walk(&format!("{prefix}[]"), x, want, out)),
            leaf => {
                if prefix.trim_start_matches('.') == want {
                    out.push(leaf.clone());
                }
            }
        }
    }
    let mut out = Vec::new();
    walk("", doc, path, &mut out);
    out
}

/// The JSON Schema of [`Contract`], as committed under
/// `docs/baseline/schemas/transform.schema.json`.
pub fn schema() -> Value {
    serde_json::to_value(schemars::schema_for!(Contract)).expect("schema serializes")
}

/// The ops marked as defects (bugs to fix, not inherent to the target
/// format), by key path.
pub fn defects(contract: &Contract) -> BTreeMap<String, String> {
    contract
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Drop {
                path,
                defect: Some(d),
                ..
            }
            | Op::Synthesize {
                path,
                defect: Some(d),
                ..
            } => Some((path.clone(), d.clone())),
            _ => None,
        })
        .collect()
}
