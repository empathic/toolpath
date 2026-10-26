//! What each harness's recordings actually contain: per record kind, every
//! field (key path, arrays collapsed), the JSON shapes seen, and whether it
//! was ever missing. The facts `docs/agents/formats/` states by hand
//! ("optional = seen without it, required = never seen missing", "what we've
//! seen in sample data"), computed from the recordings instead, so a new
//! recording that changes them changes the generated table.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::Value;

use crate::keypaths::json_values;
use crate::model::Agent;

/// One recording of a harness: its name in the baseline and its bytes.
pub struct Recording<'a> {
    pub name: &'a str,
    pub text: &'a str,
}

/// The keys whose values name the harness version that wrote a recording.
const VERSION_KEYS: [&str; 3] = ["version", "cli_version", "copilotVersion"];

#[derive(Default)]
struct Field {
    shapes: BTreeSet<&'static str>,
    present: usize,
}

#[derive(Default)]
struct Kind {
    records: usize,
    fields: BTreeMap<String, Field>,
}

/// The record kind a value belongs to: its `type` (with Codex's
/// `payload.type`), or `document` for a recording that is one JSON
/// document, or `(no type)`.
fn kind_of(v: &Value, single_document: bool) -> String {
    if single_document {
        return "document".to_owned();
    }
    match (v["type"].as_str(), v["payload"]["type"].as_str()) {
        (Some(t), Some(p)) => format!("{t}/{p}"),
        (Some(t), None) => t.to_owned(),
        (None, _) => v
            .as_object()
            .and_then(|o| o.keys().find(|k| k.starts_with('$')).cloned())
            .unwrap_or_else(|| "(no type)".to_owned()),
    }
}

fn shape(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Every leaf key path of `v` (arrays collapsed to `[]`), with its shape.
/// Empty arrays and objects are leaves too, so a field that is always empty
/// still shows.
fn leaves(prefix: &str, v: &Value, out: &mut BTreeMap<String, BTreeSet<&'static str>>) {
    match v {
        Value::Object(m) if !m.is_empty() => {
            for (k, x) in m {
                let k = if data_key(k) { "<key>" } else { k };
                leaves(&format!("{prefix}.{k}"), x, out);
            }
        }
        Value::Array(a) if !a.is_empty() => {
            for x in a {
                leaves(&format!("{prefix}[]"), x, out);
            }
        }
        leaf => {
            out.entry(prefix.trim_start_matches('.').to_owned())
                .or_default()
                .insert(shape(leaf));
        }
    }
}

/// A key that names a record's own data rather than the format's structure
/// (a file path, an id, a dated model id) would make the table grow with every session; such
/// a key is shown as `<key>`.
fn data_key(k: &str) -> bool {
    let digit_run = k
        .split(|c: char| !c.is_ascii_digit())
        .map(str::len)
        .max()
        .unwrap_or(0);
    k.contains('/')
        || k.len() > 40
        || (k.len() >= 16 && k.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        // a dated model id (`claude-haiku-4-5-20251001`) used as a key
        || digit_run >= 8
}

/// The harness versions the recordings name: `(key path, value, recording)`.
pub fn versions(recordings: &[Recording]) -> BTreeSet<(String, String, String)> {
    let mut out = BTreeSet::new();
    for r in recordings {
        for v in json_values(r.text) {
            let mut leafs = BTreeMap::new();
            leaves("", &v, &mut leafs);
            for path in leafs.keys() {
                let last = path.rsplit('.').next().unwrap_or(path);
                if !VERSION_KEYS.contains(&last)
                    || path.matches('.').count() > 2
                    || path.contains("<key>")
                {
                    continue;
                }
                let ptr = format!("/{}", path.replace('.', "/"));
                if let Some(val) = v.pointer(&ptr).filter(|x| x.is_string() || x.is_number()) {
                    let val = val.as_str().map_or_else(|| val.to_string(), str::to_owned);
                    out.insert((path.clone(), val, r.name.to_owned()));
                }
            }
        }
    }
    out
}

/// The record kinds the recordings contain (see [`render`]).
pub fn kinds(recordings: &[Recording]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for r in recordings {
        let single = serde_json::from_str::<Value>(r.text).is_ok();
        for v in json_values(r.text) {
            out.insert(kind_of(&v, single));
        }
    }
    out
}

/// The generated Markdown table for one harness.
pub fn render(agent: Agent, recordings: &[Recording]) -> String {
    let mut kinds: BTreeMap<String, Kind> = BTreeMap::new();
    for r in recordings {
        let single = serde_json::from_str::<Value>(r.text).is_ok();
        for v in json_values(r.text) {
            let kind = kinds.entry(kind_of(&v, single)).or_default();
            kind.records += 1;
            let mut leafs = BTreeMap::new();
            leaves("", &v, &mut leafs);
            for (p, s) in leafs {
                let f = kind.fields.entry(p).or_default();
                f.present += 1;
                f.shapes.extend(s);
            }
        }
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "<!-- Generated from the baseline's recordings by `crates/toolpath-goldens/tests/observed.rs`; do not edit.\n     Regenerate: OBSERVED_UPDATE=1 cargo test -p toolpath-goldens --test observed -->\n"
    );
    let _ = writeln!(out, "# {agent}: fields observed in the recordings\n");
    let _ = writeln!(
        out,
        "Computed from every recording of this harness the baseline runs. **required** = present in every record of \
         its kind in these recordings; **optional** = seen missing at least once. Shapes are the JSON types seen. \
         Arrays are collapsed to `[]`; a key that is data (a path, an id) is shown as `<key>`. This is what the \
         recordings contain, not a specification: a field absent here may exist in versions we have no recording of.\n"
    );
    let _ = writeln!(out, "## Recordings\n");
    for r in recordings {
        let _ = writeln!(out, "- `{}`", r.name);
    }
    let vs = versions(recordings);
    let _ = writeln!(out, "\n## Versions the recordings name\n");
    if vs.is_empty() {
        let _ = writeln!(
            out,
            "None: these recordings do not record the harness version."
        );
    } else {
        for (path, val, rec) in &vs {
            let _ = writeln!(out, "- `{path}` = `{val}` (`{rec}`)");
        }
    }
    for (name, kind) in &kinds {
        let plural = if kind.records == 1 {
            "record"
        } else {
            "records"
        };
        let _ = writeln!(out, "\n## `{name}` ({} {plural})\n", kind.records);
        let _ = writeln!(out, "| field | shape | presence |");
        let _ = writeln!(out, "|---|---|---|");
        for (path, f) in &kind.fields {
            let shapes: Vec<&str> = f.shapes.iter().copied().collect();
            let presence = if f.present == kind.records {
                "required".to_owned()
            } else {
                format!("optional ({}/{})", f.present, kind.records)
            };
            let _ = writeln!(
                out,
                "| `{}` | {} | {} |",
                path.replace('|', "\\|"),
                shapes.join(", "),
                presence
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_is_counted_per_kind() {
        let text = "{\"type\":\"a\",\"x\":1,\"y\":\"s\"}\n{\"type\":\"a\",\"x\":2}\n{\"type\":\"b\",\"z\":null}\n";
        let md = render(Agent::Claude, &[Recording { name: "t", text }]);
        assert!(md.contains("## `a` (2 records)"), "{md}");
        assert!(md.contains("## `b` (1 record)"), "{md}");
        assert!(md.contains("| `x` | number | required |"), "{md}");
        assert!(md.contains("| `y` | string | optional (1/2) |"), "{md}");
        assert!(md.contains("| `z` | null | required |"), "{md}");
    }

    #[test]
    fn data_keys_are_generalised() {
        let text = "{\"type\":\"a\",\"change\":{\"/tmp/x.rs\":{\"raw\":\"d\"}}}\n";
        let md = render(Agent::Claude, &[Recording { name: "t", text }]);
        assert!(
            md.contains("| `change.<key>.raw` | string | required |"),
            "{md}"
        );
        assert!(data_key("0123456789abcdef0123"));
        assert!(data_key("claude-haiku-4-5-20251001"));
        assert!(!data_key("content"));
        assert!(!data_key("cache_creation_input_tokens"));
    }

    #[test]
    fn versions_are_found_near_the_top() {
        let text = "{\"type\":\"session_meta\",\"payload\":{\"cli_version\":\"0.128.0\"}}\n";
        let v = versions(&[Recording {
            name: "codex",
            text,
        }]);
        assert!(v.contains(&(
            "payload.cli_version".to_owned(),
            "0.128.0".to_owned(),
            "codex".to_owned()
        )));
    }
}
