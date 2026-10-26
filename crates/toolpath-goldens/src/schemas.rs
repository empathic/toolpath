//! JSON Schemas of the baseline's snapshot kinds.
//!
//! A snapshot is one stage's pinned output for one recording; its kind is
//! the name before the first `@` (`round_trip@codex@claude.snap` is a
//! `round_trip`). The kinds whose body is one of this crate's types get a
//! schema generated from that type here; the rest are written by hand under
//! [`DIR`] (see its README). `tests/schemas.rs` keeps the generated files in
//! step with the types and validates every committed snapshot body against
//! its kind's schema.
//!
//! Regenerate after changing a model type:
//! `UPDATE_SCHEMAS=1 cargo test -p toolpath-goldens --test schemas`.

use schemars::schema_for;
use serde_json::{Value, json};

use crate::model::{KnownDefect, RoundTrip, Shed, TwiceAround, Validate};

/// Where the schemas live, relative to the workspace root.
pub const DIR: &str = "docs/baseline/schemas";

/// The generated schemas: `(kind, schema)`, in file-name order.
pub fn generated() -> Vec<(&'static str, Value)> {
    vec![
        (
            "known_defects",
            titled(
                serde_json::to_value(schema_for!(Vec<KnownDefect>)).expect("schema serialises"),
                "known_defects snapshot",
                "The defects accepted today for one recording. A known defect passes the baseline; a new one, or one that gets worse, fails it.",
            ),
        ),
        (
            "round_trip",
            titled(
                serde_json::to_value(schema_for!(RoundTrip)).expect("schema serialises"),
                "round_trip snapshot",
                "One recording's document projected into another harness, read back by that harness's own reader and derived again: what survived, what was lost, what was gained.",
            ),
        ),
        (
            "shed",
            titled(
                serde_json::to_value(schema_for!(Shed)).expect("schema serialises"),
                "shed snapshot",
                "What each stage sheds for one recording: transcript, document, every projection, and the same-format round trip field by field.",
            ),
        ),
        (
            "twice_around",
            titled(
                serde_json::to_value(schema_for!(TwiceAround)).expect("schema serialises"),
                "twice_around snapshot",
                "A round trip's derived document projected to the same harness a second time and read back: the key paths lost, gained or changed between pass 1 and pass 2. Anything listed is accumulating encoding error and must be declared.",
            ),
        ),
        (
            "validate",
            titled(
                serde_json::to_value(schema_for!(Validate)).expect("schema serialises"),
                "validate snapshot",
                "`path p validate` over one recording's document and over every projection derived again.",
            ),
        ),
        (
            "writer",
            titled(
                serde_json::to_value(schema_for!(RoundTrip)).expect("schema serialises"),
                "writer snapshot",
                "One writer on its own: the generated full-view document projected by a single harness's writer and read back by that harness's reader.",
            ),
        ),
    ]
}

/// A generated schema with the baseline's title and description on top.
fn titled(mut schema: Value, title: &str, description: &str) -> Value {
    let obj = schema.as_object_mut().expect("a root schema is an object");
    obj.insert("title".into(), json!(title));
    obj.insert("description".into(), json!(description));
    schema
}

/// The file a kind's schema lives in, relative to the workspace root.
pub fn file(kind: &str) -> String {
    format!("{DIR}/{kind}.schema.json")
}

/// A schema as committed: pretty JSON and a trailing newline.
pub fn render(schema: &Value) -> String {
    let mut s = serde_json::to_string_pretty(schema).expect("schema serialises");
    s.push('\n');
    s
}
