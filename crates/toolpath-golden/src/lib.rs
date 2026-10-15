//! Test-only stub factory (goldens spec rule 10): test inputs are derived from captured bytes, never typed.
//!
//! ```ignore
//! let g = toolpath_golden::golden("claude");
//! let jsonl = g.slice(0..6);                                  // a prefix of a real session
//! let bad = g.with_field(2, "message.role", json!("robot"));  // one-field mutation of one line
//! ```
//!
//! Add it as a `[dev-dependencies]` entry (`toolpath-golden = { workspace = true }`). It reads the repo's
//! `goldens/manifest.json` and the fixture each set names, so it only works inside this workspace.

use std::fs;
use std::path::Path;

use serde_json::Value;

/// A handle on one golden set's captured input.
pub struct Golden {
    name: String,
    text: String,
}

/// The captured input of golden set `set`, read from the repo's `goldens/manifest.json` (the path
/// the manifest names for the set's fixture). Panics with a readable message if the set is unknown:
/// this is a test helper.
pub fn golden(set: &str) -> Golden {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest: Value = serde_json::from_slice(
        &fs::read(root.join("goldens/manifest.json")).expect("goldens/manifest.json"),
    )
    .expect("manifest is JSON");
    let fixture = manifest["goldens"]
        .as_array()
        .and_then(|a| a.iter().find(|e| e["name"] == set))
        .and_then(|e| e["fixture"].as_str())
        .unwrap_or_else(|| panic!("no golden set named {set:?}"))
        .to_string();
    let text = fs::read_to_string(root.join(&fixture)).unwrap_or_else(|e| panic!("{fixture}: {e}"));
    Golden {
        name: set.to_string(),
        text,
    }
}

impl Golden {
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The whole captured input.
    pub fn input(&self) -> &str {
        &self.text
    }
    /// Every line parsed as JSON (non-JSON lines are skipped).
    pub fn events(&self) -> Vec<Value> {
        self.text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
    /// Lines `range` of the input, newline-joined (no trailing newline).
    pub fn slice(&self, range: std::ops::Range<usize>) -> String {
        self.text
            .lines()
            .skip(range.start)
            .take(range.len())
            .collect::<Vec<_>>()
            .join("\n")
    }
    /// The first string value of key `key` anywhere in the input (e.g. `sessionId`, `cwd`).
    pub fn first_string(&self, key: &str) -> Option<String> {
        fn find(v: &Value, key: &str) -> Option<String> {
            match v {
                Value::Object(m) => m
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| m.values().find_map(|x| find(x, key))),
                Value::Array(a) => a.iter().find_map(|x| find(x, key)),
                _ => None,
            }
        }
        self.events().iter().find_map(|v| find(v, key))
    }
    /// The input with `value` set at dotted `path` on line `line` (one-field mutation).
    pub fn with_field(&self, line: usize, path: &str, value: Value) -> String {
        let mut lines: Vec<String> = self.text.lines().map(str::to_string).collect();
        let mut v: Value = serde_json::from_str(&lines[line]).expect("line is JSON");
        let mut cur = &mut v;
        let parts: Vec<&str> = path.split('.').collect();
        for p in &parts[..parts.len() - 1] {
            cur = &mut cur[*p];
        }
        cur[parts[parts.len() - 1]] = value;
        lines[line] = serde_json::to_string(&v).expect("serialises");
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn slices_and_mutates_captured_bytes() {
        let g = golden("claude");
        assert_eq!(g.name(), "claude");
        assert_eq!(g.input().lines().count(), g.events().len());
        // a prefix of the real session, and one field changed on one line of it
        assert_eq!(g.slice(0..3).lines().count(), 3);
        let mutated = g.with_field(0, "marker", json!("changed"));
        assert_eq!(mutated.lines().count(), g.input().lines().count());
        assert!(
            mutated
                .lines()
                .next()
                .unwrap()
                .contains("\"marker\":\"changed\"")
        );
        // identity fields come from the capture, not from a literal in the test
        assert!(g.first_string("sessionId").is_some() && g.first_string("cwd").is_some());
    }
}
