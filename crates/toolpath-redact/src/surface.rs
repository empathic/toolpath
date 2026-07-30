//! The field map: every string a secret could hide in, named by pointer.

use std::collections::HashMap;

use serde_json::Value;

use crate::detect::FieldShape;
use crate::{RedactError, Result};

/// One field the map named, whether or not anything was found in it. A
/// surface with zero findings is information: the pass reached that field
/// and the detectors were silent.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Surface {
    pub step: String,
    pub at: String,
    pub shape: FieldShape,
    pub bytes: usize,
}

/// Every string in `path` a detector should see, each named by an RFC 6901
/// pointer relative to its step (`step` is empty for document-level fields).
///
/// The order is part of the contract. Finding ids are positional and a
/// regenerated plan is compared byte for byte, but `Step::change` and
/// `StructuralChange::extra` are `HashMap`s whose iteration order is not
/// stable across runs - so steps keep document order, artifacts sort by key,
/// and a blind walk sorts object keys.
///
/// An artifact's own key is emitted *after* everything beneath it: writing
/// that surface renames the map entry, which invalidates every pointer under
/// the old key, so a caller applying surfaces in order rewrites the key last.
pub fn surfaces(path: &toolpath::v1::Path) -> Vec<Surface> {
    let mut out = Vec::new();
    for step in &path.steps {
        let sid = &step.step.id;
        let mut keys: Vec<&String> = step.change.keys().collect();
        keys.sort();
        for artifact_key in keys {
            let change = &step.change[artifact_key];
            let akey = ptr_escape(artifact_key);

            if let Some(raw) = &change.raw {
                push(
                    &mut out,
                    sid,
                    format!("/change/{akey}/raw"),
                    FieldShape::UnifiedDiff,
                    raw,
                );
            }
            if let Some(s) = &change.structural {
                let base = format!("/change/{akey}/structural/extra");
                match s.change_type.as_str() {
                    "conversation.append" => turn_surfaces(&mut out, sid, &base, &s.extra),
                    "file.write" => file_write_surfaces(&mut out, sid, &base, &s.extra),
                    // The one place a blind leaf walk is correct: the payload
                    // is unmodelled provider JSON.
                    _ => walk_fields(&mut out, sid, &base, &s.extra, FieldShape::OpaqueJson),
                }
            }
            push(
                &mut out,
                sid,
                format!("/change/{akey}"),
                FieldShape::Uri,
                artifact_key,
            );
        }
    }
    if let Some(b) = &path.path.base {
        push(
            &mut out,
            "",
            "/path/base/uri".into(),
            FieldShape::Uri,
            &b.uri,
        );
    }
    if let Some(v) = path
        .meta
        .as_ref()
        .and_then(|m| m.extra.get("vcs_remote"))
        .and_then(|v| v.as_str())
    {
        push(&mut out, "", "/meta/vcs_remote".into(), FieldShape::Uri, v);
    }
    out
}

fn push(out: &mut Vec<Surface>, step: &str, at: String, shape: FieldShape, text: &str) {
    if text.is_empty() {
        return;
    }
    out.push(Surface {
        step: step.to_string(),
        at,
        shape,
        bytes: text.len(),
    });
}

/// The two containers a turn's fields arrive in: a top-level
/// `structural.extra` map, and a delegated turn's JSON object.
trait Fields {
    fn field(&self, key: &str) -> Option<&Value>;
}

impl Fields for HashMap<String, Value> {
    fn field(&self, key: &str) -> Option<&Value> {
        self.get(key)
    }
}

impl Fields for serde_json::Map<String, Value> {
    fn field(&self, key: &str) -> Option<&Value> {
        self.get(key)
    }
}

/// The `conversation.append` rows of the map. A delegated turn serializes
/// with the same field names as the extras of the turn that spawned it, so
/// sub-conversations re-enter here.
fn turn_surfaces(out: &mut Vec<Surface>, step: &str, at: &str, fields: &dyn Fields) {
    for key in ["text", "thinking"] {
        if let Some(s) = fields.field(key).and_then(Value::as_str) {
            push(out, step, format!("{at}/{key}"), FieldShape::Prose, s);
        }
    }

    for (i, tool) in array(fields.field("tool_uses")).iter().enumerate() {
        if let Some(input) = tool.get("input") {
            walk_json(
                out,
                step,
                &format!("{at}/tool_uses/{i}/input"),
                input,
                FieldShape::ToolInput,
            );
        }
        if let Some(s) = tool.pointer("/result/content").and_then(Value::as_str) {
            push(
                out,
                step,
                format!("{at}/tool_uses/{i}/result/content"),
                FieldShape::ToolOutput,
                s,
            );
        }
    }

    for (i, work) in array(fields.field("delegations")).iter().enumerate() {
        let at = format!("{at}/delegations/{i}");
        for key in ["prompt", "result"] {
            if let Some(s) = work.get(key).and_then(Value::as_str) {
                push(out, step, format!("{at}/{key}"), FieldShape::Prose, s);
            }
        }
        for (j, turn) in array(work.get("turns")).iter().enumerate() {
            if let Some(obj) = turn.as_object() {
                turn_surfaces(out, step, &format!("{at}/turns/{j}"), obj);
            }
        }
    }

    // Only a top-level turn's file mutations get hoisted into sibling
    // `file.write` changes; a delegated turn carries its own inline, and they
    // hold the same before/after file content.
    for (i, mutation) in array(fields.field("file_mutations")).iter().enumerate() {
        let at = format!("{at}/file_mutations/{i}");
        if let Some(s) = mutation.get("raw_diff").and_then(Value::as_str) {
            push(
                out,
                step,
                format!("{at}/raw_diff"),
                FieldShape::UnifiedDiff,
                s,
            );
        }
        for key in ["before", "after"] {
            if let Some(s) = mutation.get(key).and_then(Value::as_str) {
                push(out, step, format!("{at}/{key}"), FieldShape::FileContent, s);
            }
        }
    }

    if let Some(s) = fields
        .field("environment")
        .and_then(|e| e.get("working_dir"))
        .and_then(Value::as_str)
    {
        push(
            out,
            step,
            format!("{at}/environment/working_dir"),
            FieldShape::Uri,
            s,
        );
    }
}

/// The `file.write` rows: whole-file states, plus both sides of every edit.
fn file_write_surfaces(
    out: &mut Vec<Surface>,
    step: &str,
    at: &str,
    extra: &HashMap<String, Value>,
) {
    for key in ["before", "after"] {
        if let Some(s) = extra.get(key).and_then(Value::as_str) {
            push(out, step, format!("{at}/{key}"), FieldShape::FileContent, s);
        }
    }
    for (i, edit) in array(extra.get("edits")).iter().enumerate() {
        walk_json(
            out,
            step,
            &format!("{at}/edits/{i}"),
            edit,
            FieldShape::FileContent,
        );
    }
}

fn walk_fields(
    out: &mut Vec<Surface>,
    step: &str,
    at: &str,
    fields: &HashMap<String, Value>,
    shape: FieldShape,
) {
    for (key, value) in sorted(fields.iter()) {
        walk_json(
            out,
            step,
            &format!("{at}/{}", ptr_escape(key)),
            value,
            shape,
        );
    }
}

/// Every string leaf under `value`, named by pointer. Non-string scalars are
/// not candidates: a detector has nothing to span in a number or a bool.
fn walk_json(out: &mut Vec<Surface>, step: &str, at: &str, value: &Value, shape: FieldShape) {
    match value {
        Value::String(s) => push(out, step, at.to_string(), shape, s),
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                walk_json(out, step, &format!("{at}/{i}"), item, shape);
            }
        }
        Value::Object(map) => {
            for (key, item) in sorted(map.iter()) {
                walk_json(out, step, &format!("{at}/{}", ptr_escape(key)), item, shape);
            }
        }
        _ => {}
    }
}

/// Key order decides surface order, and `HashMap`'s is seeded per map while
/// `serde_json::Map`'s depends on the `preserve_order` feature. Sort both.
fn sorted<'a, I: Iterator<Item = (&'a String, &'a Value)>>(
    entries: I,
) -> Vec<(&'a str, &'a Value)> {
    let mut entries: Vec<(&str, &Value)> = entries.map(|(k, v)| (k.as_str(), v)).collect();
    entries.sort_by_key(|(k, _)| *k);
    entries
}

fn array(value: Option<&Value>) -> &[Value] {
    value.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// Resolves a `(step, pointer)` pair against a document for reading and
/// writing. Read and write must resolve identically.
pub struct SurfaceCursor<'a> {
    pub path: &'a mut toolpath::v1::Path,
}

/// Where a pointer lands, parsed once so read and write cannot drift apart.
enum Route {
    BaseUri,
    MetaExtra(String),
    /// The artifact map key itself. Writing renames the entry.
    ArtifactKey(String),
    Raw(String),
    /// `structural.extra[field]`, plus an RFC 6901 pointer into it (empty
    /// when the field is itself the string).
    Extra {
        artifact: String,
        field: String,
        tail: String,
    },
}

fn route(at: &str) -> Option<Route> {
    if at == "/path/base/uri" {
        return Some(Route::BaseUri);
    }
    if let Some(key) = at.strip_prefix("/meta/") {
        return (!key.is_empty() && !key.contains('/')).then(|| Route::MetaExtra(ptr_decode(key)));
    }

    let rest = at.strip_prefix("/change/")?;
    let Some((akey, rest)) = rest.split_once('/') else {
        return (!rest.is_empty()).then(|| Route::ArtifactKey(ptr_decode(rest)));
    };
    let artifact = ptr_decode(akey);
    if rest == "raw" {
        return Some(Route::Raw(artifact));
    }

    let rest = rest.strip_prefix("structural/extra/")?;
    let (field, tail) = match rest.split_once('/') {
        Some((field, tail)) => (field, format!("/{tail}")),
        None => (rest, String::new()),
    };
    (!field.is_empty()).then(|| Route::Extra {
        artifact,
        field: ptr_decode(field),
        tail,
    })
}

fn find_step<'a>(path: &'a toolpath::v1::Path, id: &str) -> Option<&'a toolpath::v1::Step> {
    path.steps.iter().find(|s| s.step.id == id)
}

fn find_step_mut<'a>(
    path: &'a mut toolpath::v1::Path,
    id: &str,
) -> Option<&'a mut toolpath::v1::Step> {
    path.steps.iter_mut().find(|s| s.step.id == id)
}

impl SurfaceCursor<'_> {
    pub fn read(&self, step: &str, at: &str) -> Option<String> {
        match route(at)? {
            Route::BaseUri => Some(self.path.path.base.as_ref()?.uri.clone()),
            Route::MetaExtra(key) => Some(
                self.path
                    .meta
                    .as_ref()?
                    .extra
                    .get(&key)?
                    .as_str()?
                    .to_string(),
            ),
            Route::ArtifactKey(key) => find_step(self.path, step)?
                .change
                .contains_key(&key)
                .then_some(key),
            Route::Raw(key) => find_step(self.path, step)?.change.get(&key)?.raw.clone(),
            Route::Extra {
                artifact,
                field,
                tail,
            } => {
                let value = find_step(self.path, step)?
                    .change
                    .get(&artifact)?
                    .structural
                    .as_ref()?
                    .extra
                    .get(&field)?;
                let leaf = if tail.is_empty() {
                    value
                } else {
                    value.pointer(&tail)?
                };
                Some(leaf.as_str()?.to_string())
            }
        }
    }

    pub fn write(&mut self, step: &str, at: &str, value: &str) -> Result<()> {
        let bad = || RedactError::BadPointer(at.to_string());
        match route(at).ok_or_else(bad)? {
            Route::BaseUri => {
                self.path.path.base.as_mut().ok_or_else(bad)?.uri = value.to_string();
            }
            Route::MetaExtra(key) => {
                let slot = self
                    .path
                    .meta
                    .as_mut()
                    .and_then(|m| m.extra.get_mut(&key))
                    .ok_or_else(bad)?;
                *string_slot(slot).ok_or_else(bad)? = value.to_string();
            }
            Route::ArtifactKey(key) => {
                let target = find_step_mut(self.path, step).ok_or_else(bad)?;
                // Two keys redacting to the same string would silently drop
                // one artifact's changes; refuse instead of destroying data.
                if key != value && target.change.contains_key(value) {
                    return Err(RedactError::PlanMismatch(format!(
                        "redacted artifact key {value} already exists on step {step}"
                    )));
                }
                let change = target.change.remove(&key).ok_or_else(bad)?;
                target.change.insert(value.to_string(), change);
            }
            Route::Raw(key) => {
                let change = find_step_mut(self.path, step)
                    .ok_or_else(bad)?
                    .change
                    .get_mut(&key)
                    .ok_or_else(bad)?;
                *change.raw.as_mut().ok_or_else(bad)? = value.to_string();
            }
            Route::Extra {
                artifact,
                field,
                tail,
            } => {
                let slot = find_step_mut(self.path, step)
                    .ok_or_else(bad)?
                    .change
                    .get_mut(&artifact)
                    .ok_or_else(bad)?
                    .structural
                    .as_mut()
                    .ok_or_else(bad)?
                    .extra
                    .get_mut(&field)
                    .ok_or_else(bad)?;
                let leaf = if tail.is_empty() {
                    slot
                } else {
                    slot.pointer_mut(&tail).ok_or_else(bad)?
                };
                *string_slot(leaf).ok_or_else(bad)? = value.to_string();
            }
        }
        Ok(())
    }
}

/// A write resolves only where a read would have: on a string leaf.
fn string_slot(value: &mut Value) -> Option<&mut String> {
    match value {
        Value::String(s) => Some(s),
        _ => None,
    }
}

/// Escape one pointer token. `~` first, or the `~1` this emits for `/` would
/// be re-escaped into `~01` (RFC 6901).
pub fn ptr_escape(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Decode `~1` before `~0`, or `~01` round-trips wrong (RFC 6901).
fn ptr_decode(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use toolpath::v1::{ArtifactChange, Base, Path, PathMeta, Step, StructuralChange};

    fn object(value: Value) -> HashMap<String, Value> {
        match value {
            Value::Object(map) => map.into_iter().collect(),
            other => panic!("fixture extras must be an object, got {other}"),
        }
    }

    fn path_of(steps: Vec<Step>) -> Path {
        let head = steps.last().map(|s| s.step.id.clone()).unwrap_or_default();
        let mut path = Path::new("path-1", None, head);
        path.steps = steps;
        path
    }

    fn change(change_type: &str, raw: Option<&str>, extra: Value) -> ArtifactChange {
        ArtifactChange {
            raw: raw.map(str::to_string),
            structural: Some(StructuralChange {
                change_type: change_type.to_string(),
                extra: object(extra),
            }),
        }
    }

    fn step_with(id: &str, changes: Vec<(&str, ArtifactChange)>) -> Step {
        let mut step = Step::new(id, "agent:claude-opus-5", "2026-07-30T10:00:00Z");
        for (key, c) in changes {
            step.change.insert(key.to_string(), c);
        }
        step
    }

    fn append_step(id: &str, extra: Value) -> Step {
        step_with(
            id,
            vec![(
                "claude://sess-abc",
                change("conversation.append", None, extra),
            )],
        )
    }

    fn fixture_conversation_append() -> Path {
        path_of(vec![append_step(
            "turn-0f3a",
            json!({
                "role": "assistant",
                "text": "I set AWS_SECRET_ACCESS_KEY for you",
                "thinking": "the key was pasted in the prompt",
                "tool_uses": [{
                    "id": "toolu_01",
                    "name": "Bash",
                    "input": {
                        "command": "aws configure set aws_secret_access_key AKIAIOSFODNN7EXAMPLE",
                        "timeout": 120
                    },
                    "category": "command",
                    "result": {"content": "configured", "is_error": false}
                }],
                "environment": {"working_dir": "/Users/alex/work/repo"},
                "token_usage": {"input_tokens": 10, "output_tokens": 2}
            }),
        )])
    }

    fn fixture_file_write() -> Path {
        path_of(vec![step_with(
            "turn-9c21",
            vec![(
                "src/config.rs",
                change(
                    "file.write",
                    Some(
                        "--- a/src/config.rs\n+++ b/src/config.rs\n@@ -1 +1 @@\n-let k = \"old\";\n+let k = \"new\";\n",
                    ),
                    json!({
                        "tool": "Edit",
                        "tool_id": "toolu_02",
                        "operation": "update",
                        "before": "let k = \"old\";\n",
                        "after": "let k = \"new\";\n"
                    }),
                ),
            )],
        )])
    }

    fn fixture_clean_conversation() -> Path {
        path_of(vec![append_step(
            "turn-clean",
            json!({"role": "user", "text": "please rename the greeting function"}),
        )])
    }

    fn fixture_with_delegation() -> Path {
        path_of(vec![append_step(
            "turn-deleg",
            json!({
                "role": "assistant",
                "text": "delegating the audit",
                "delegations": [{
                    "agent_id": "sub-1",
                    "prompt": "audit the deploy script",
                    "result": "found a hardcoded token",
                    "turns": [{
                        "id": "sub-turn-1",
                        "role": "assistant",
                        "timestamp": "2026-07-30T10:00:01Z",
                        "text": "the script exports GITHUB_TOKEN=ghp_example",
                        "tool_uses": [{
                            "id": "toolu_sub",
                            "name": "Read",
                            "input": {"file_path": "/srv/deploy.sh"},
                            "result": {"content": "export GITHUB_TOKEN=ghp_example", "is_error": false}
                        }],
                        "file_mutations": [{
                            "path": "deploy.sh",
                            "raw_diff": "--- a/deploy.sh\n+++ b/deploy.sh\n@@ -1 +1 @@\n-old\n+new\n",
                            "before": "old\n",
                            "after": "new\n"
                        }]
                    }]
                }]
            }),
        )])
    }

    fn fixture_unknown_change_type() -> Path {
        path_of(vec![step_with(
            "evt-1",
            vec![(
                "claude://sess-abc",
                change(
                    "conversation.event",
                    None,
                    json!({
                        "event_type": "attachment",
                        "data": {"a/b": "slash in the key", "nested": ["leaf", 7]}
                    }),
                ),
            )],
        )])
    }

    /// Every branch of the map in one document, for the whole-surface
    /// read/write sweeps.
    fn fixture_rich() -> Path {
        let mut path = path_of(vec![
            step_with(
                "turn-0f3a",
                vec![
                    (
                        "claude://sess-abc",
                        fixture_conversation_append().steps[0].change["claude://sess-abc"].clone(),
                    ),
                    (
                        "~/notes.md",
                        change(
                            "file.write",
                            Some("--- a/notes.md\n+++ b/notes.md\n@@ -1 +1 @@\n-a\n+b\n"),
                            json!({
                                "before": "a\n",
                                "after": "b\n",
                                "edits": [{"old_string": "a", "new_string": "b", "replace_all": false}]
                            }),
                        ),
                    ),
                ],
            ),
            fixture_with_delegation().steps.remove(0),
            fixture_unknown_change_type().steps.remove(0),
        ]);
        path.path.base = Some(Base::vcs("https://alex:tok@github.com/o/r", "abc123"));
        path.meta = Some(PathMeta {
            extra: object(json!({"vcs_remote": "https://alex:tok@github.com/o/r.git"})),
            ..PathMeta::default()
        });
        path
    }

    fn ats(path: &Path) -> Vec<String> {
        surfaces(path).into_iter().map(|s| s.at).collect()
    }

    #[test]
    fn ptr_escape_handles_urls_and_tildes() {
        assert_eq!(ptr_escape("claude://sess-abc"), "claude:~1~1sess-abc");
        assert_eq!(ptr_escape("src/config.rs"), "src~1config.rs");
        assert_eq!(ptr_escape("a~b"), "a~0b");
        assert_eq!(ptr_escape("~/x"), "~0~1x");
    }

    #[test]
    fn ptr_escape_round_trips() {
        for raw in ["claude://sess-abc", "src/config.rs", "a~b/c", "~01"] {
            // Decode `~1` before `~0`, or `~01` round-trips wrong (RFC 6901).
            let dec = ptr_escape(raw).replace("~1", "/").replace("~0", "~");
            assert_eq!(dec, raw);
        }
    }

    #[test]
    fn conversation_append_surfaces_all_text_fields() {
        let p = fixture_conversation_append();
        let all = surfaces(&p);
        let ats: Vec<&str> = all.iter().map(|s| s.at.as_str()).collect();
        assert!(ats.iter().any(|a| a.ends_with("/structural/extra/text")));
        assert!(
            ats.iter()
                .any(|a| a.ends_with("/structural/extra/thinking"))
        );
        assert!(ats.iter().any(|a| a.contains("/tool_uses/0/input")));
        assert!(
            ats.iter()
                .any(|a| a.contains("/tool_uses/0/result/content"))
        );
    }

    #[test]
    fn file_write_surfaces_diff_and_both_file_states() {
        let p = fixture_file_write();
        let shapes: Vec<FieldShape> = surfaces(&p).iter().map(|s| s.shape).collect();
        assert!(shapes.contains(&FieldShape::UnifiedDiff));
        assert_eq!(
            shapes
                .iter()
                .filter(|s| **s == FieldShape::FileContent)
                .count(),
            2
        );
    }

    #[test]
    fn identity_fields_are_never_surfaced() {
        for s in surfaces(&fixture_conversation_append()) {
            for banned in [
                "/step/id",
                "/step/actor",
                "/step/timestamp",
                "/step/parents",
            ] {
                assert!(
                    !s.at.starts_with(banned),
                    "surfaced identity field: {}",
                    s.at
                );
            }
        }
    }

    #[test]
    fn clean_field_still_appears_as_a_surface() {
        // The dry-run guarantee: a surface with nothing in it is information.
        let p = fixture_clean_conversation();
        assert!(
            surfaces(&p)
                .iter()
                .any(|s| s.at.ends_with("/structural/extra/text"))
        );
    }

    #[test]
    fn delegations_recurse() {
        assert!(
            surfaces(&fixture_with_delegation())
                .iter()
                .any(|s| s.at.contains("/delegations/0/turns/0"))
        );
    }

    #[test]
    fn unknown_change_type_degrades_to_blind_walk() {
        assert!(!surfaces(&fixture_unknown_change_type()).is_empty());
    }

    #[test]
    fn cursor_write_is_readable_at_the_same_pointer() {
        let mut p = fixture_conversation_append();
        let at = surfaces(&p)[0].at.clone();
        let step = surfaces(&p)[0].step.clone();
        let mut c = SurfaceCursor { path: &mut p };
        c.write(&step, &at, "replaced").unwrap();
        assert_eq!(c.read(&step, &at).as_deref(), Some("replaced"));
    }

    #[test]
    fn surfaces_are_deterministic_across_equal_documents() {
        // Two separately built documents, so the `HashMap`s carry different
        // seeds: comparing one document against itself would not catch this.
        let extras = json!({
            "event_type": "attachment", "a": "1", "b": "2", "c": "3",
            "d": "4", "e": "5", "f": "6", "g": "7"
        });
        let build = || {
            path_of(vec![step_with(
                "evt-1",
                vec![
                    (
                        "z://one",
                        change("conversation.event", None, extras.clone()),
                    ),
                    (
                        "y://two",
                        change("conversation.event", None, extras.clone()),
                    ),
                    (
                        "x://three",
                        change("conversation.event", None, extras.clone()),
                    ),
                    (
                        "w://four",
                        change("conversation.event", None, extras.clone()),
                    ),
                ],
            )])
        };
        assert_eq!(surfaces(&build()), surfaces(&build()));
        let once = build();
        assert_eq!(surfaces(&once), surfaces(&once));
    }

    #[test]
    fn artifact_key_surface_follows_its_children() {
        let ats = ats(&fixture_file_write());
        let key = ats.iter().position(|a| a == "/change/src~1config.rs");
        let raw = ats.iter().position(|a| a == "/change/src~1config.rs/raw");
        assert!(key > raw, "the key rewrite must come last: {ats:?}");
    }

    #[test]
    fn bytes_is_byte_length_not_char_length() {
        let p = append_step("turn-mb", json!({"text": "héllo"}));
        let s = surfaces(&path_of(vec![p]));
        let text = s.iter().find(|s| s.at.ends_with("/text")).unwrap();
        assert_eq!(text.bytes, 6, "expected UTF-8 byte length, not chars");
    }

    #[test]
    fn empty_fields_are_not_surfaced() {
        let p = path_of(vec![append_step(
            "turn-empty",
            json!({"text": "", "thinking": "kept"}),
        )]);
        let ats = ats(&p);
        assert!(!ats.iter().any(|a| a.ends_with("/extra/text")), "{ats:?}");
        assert!(ats.iter().any(|a| a.ends_with("/extra/thinking")));
    }

    #[test]
    fn tool_input_recurses_to_string_leaves() {
        let p = path_of(vec![append_step(
            "turn-nested",
            json!({"tool_uses": [{
                "input": {"env": {"AWS_KEY": "AKIA"}, "argv": ["sh", "-c"], "retries": 3}
            }]}),
        )]);
        let ats = ats(&p);
        let leaf = "/change/claude:~1~1sess-abc/structural/extra/tool_uses/0/input";
        assert!(ats.contains(&format!("{leaf}/env/AWS_KEY")), "{ats:?}");
        assert!(ats.contains(&format!("{leaf}/argv/0")));
        assert!(!ats.iter().any(|a| a.ends_with("/retries")));
    }

    #[test]
    fn blind_walk_escapes_object_keys() {
        let p = fixture_unknown_change_type();
        let at = "/change/claude:~1~1sess-abc/structural/extra/data/a~1b";
        assert!(ats(&p).contains(&at.to_string()), "{:?}", ats(&p));

        let mut p = p;
        let mut c = SurfaceCursor { path: &mut p };
        assert_eq!(c.read("evt-1", at).as_deref(), Some("slash in the key"));
        c.write("evt-1", at, "x").unwrap();
        assert_eq!(c.read("evt-1", at).as_deref(), Some("x"));
    }

    #[test]
    fn escaped_artifact_keys_round_trip() {
        let mut p = fixture_rich();
        let mut c = SurfaceCursor { path: &mut p };
        assert_eq!(
            c.read("turn-0f3a", "/change/~0~1notes.md/structural/extra/before")
                .as_deref(),
            Some("a\n")
        );
        c.write("turn-0f3a", "/change/~0~1notes.md", "~/redacted.md")
            .unwrap();
        assert!(p.steps[0].change.contains_key("~/redacted.md"));
        assert!(!p.steps[0].change.contains_key("~/notes.md"));
    }

    #[test]
    fn artifact_key_rename_onto_an_existing_key_is_refused() {
        let mut p = fixture_rich();
        let mut c = SurfaceCursor { path: &mut p };
        assert!(matches!(
            c.write("turn-0f3a", "/change/~0~1notes.md", "claude://sess-abc"),
            Err(RedactError::PlanMismatch(_))
        ));
        assert!(p.steps[0].change.contains_key("~/notes.md"));
    }

    #[test]
    fn every_surface_reads_back() {
        let mut p = fixture_rich();
        let all = surfaces(&p);
        assert!(all.len() > 15, "fixture is too thin: {}", all.len());
        let c = SurfaceCursor { path: &mut p };
        for s in &all {
            assert!(
                c.read(&s.step, &s.at).is_some(),
                "surface does not resolve: {}",
                s.at
            );
        }
    }

    #[test]
    fn every_surface_is_writable_in_emitted_order() {
        let mut p = fixture_rich();
        let all = surfaces(&p);
        let mut c = SurfaceCursor { path: &mut p };
        for (i, s) in all.iter().enumerate() {
            // Unique values: two artifact keys redacting alike would collide.
            c.write(&s.step, &s.at, &format!("v{i}"))
                .unwrap_or_else(|e| panic!("{} failed: {e}", s.at));
        }
    }

    #[test]
    fn writing_an_unresolvable_pointer_errors() {
        let mut p = fixture_rich();
        let mut c = SurfaceCursor { path: &mut p };
        for (step, at) in [
            ("turn-0f3a", "/change/nope~1missing.rs/raw"),
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/extra/nope",
            ),
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/extra/tool_uses/9/input",
            ),
            // `token_usage` is an object, not a string leaf.
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/extra/token_usage",
            ),
            (
                "no-such-step",
                "/change/claude:~1~1sess-abc/structural/extra/text",
            ),
            ("turn-0f3a", "/step/actor"),
            ("turn-0f3a", "/change/claude:~1~1sess-abc/structural/text"),
            ("", "/meta/not_present"),
        ] {
            assert!(
                matches!(c.write(step, at, "x"), Err(RedactError::BadPointer(_))),
                "expected BadPointer for {at}"
            );
            assert_eq!(c.read(step, at), None, "read must agree with write on {at}");
        }
    }

    #[test]
    fn document_level_uris_round_trip() {
        let mut p = fixture_rich();
        let mut c = SurfaceCursor { path: &mut p };
        for at in ["/path/base/uri", "/meta/vcs_remote"] {
            assert!(c.read("", at).is_some(), "{at}");
            c.write("", at, "https://github.com/o/r").unwrap();
            assert_eq!(c.read("", at).as_deref(), Some("https://github.com/o/r"));
        }
    }

    #[test]
    fn delegated_turns_surface_their_own_shapes() {
        let p = fixture_with_delegation();
        let base = "/change/claude:~1~1sess-abc/structural/extra/delegations/0";
        let by_at: HashMap<String, FieldShape> =
            surfaces(&p).into_iter().map(|s| (s.at, s.shape)).collect();
        for (at, shape) in [
            (format!("{base}/prompt"), FieldShape::Prose),
            (format!("{base}/result"), FieldShape::Prose),
            (format!("{base}/turns/0/text"), FieldShape::Prose),
            (
                format!("{base}/turns/0/tool_uses/0/input/file_path"),
                FieldShape::ToolInput,
            ),
            (
                format!("{base}/turns/0/tool_uses/0/result/content"),
                FieldShape::ToolOutput,
            ),
            (
                format!("{base}/turns/0/file_mutations/0/raw_diff"),
                FieldShape::UnifiedDiff,
            ),
            (
                format!("{base}/turns/0/file_mutations/0/before"),
                FieldShape::FileContent,
            ),
        ] {
            assert_eq!(by_at.get(&at), Some(&shape), "missing or mistyped: {at}");
        }
    }

    #[test]
    fn file_write_edits_surface_both_sides() {
        let p = fixture_rich();
        let base = "/change/~0~1notes.md/structural/extra/edits/0";
        let ats = ats(&p);
        assert!(ats.contains(&format!("{base}/old_string")), "{ats:?}");
        assert!(ats.contains(&format!("{base}/new_string")));
        assert!(!ats.iter().any(|a| a.ends_with("/replace_all")));
    }
}
