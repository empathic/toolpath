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
/// pointer that resolves against the serialized document: relative to the
/// step for a step's fields, relative to the document root for the rest
/// (those carry an empty `step`). An audit record publishes these pointers,
/// so one that does not resolve is a lie told to whoever reads it.
///
/// The order is part of the contract. Finding ids are positional and a
/// regenerated plan is compared byte for byte, but `Step::change` and every
/// `extra` are `HashMap`s whose iteration order is not stable across runs -
/// so steps keep document order, artifacts sort by key, and every blind walk
/// sorts object keys.
///
/// An artifact's own key is emitted *after* everything beneath it: writing
/// that surface renames the map entry, which invalidates every pointer under
/// the old key, so a caller applying surfaces in order rewrites the key last.
/// Each typed arm likewise emits its named fields before the blind walk of
/// the keys it did not consume.
///
/// The map is closed by construction, so what it leaves out is as much a
/// decision as what it names. Deliberately never scanned:
///
/// - `/step/*` and `/path/{id,head}` - identity the DAG is keyed on;
///   rewriting one detaches a step from its parents.
/// - `/path/graph_ref` - a toolpath-internal link to a sibling document, not
///   content anyone typed.
/// - `StructuralChange::type`, `PathMeta::{kind,source}` - closed
///   vocabularies this format defines, with no room for a user's string.
/// - `VcsSource::{type,revision,change_id}` and `Ref::rel` - identifiers the
///   VCS or the format assigns, not the human.
/// - `{Step,Path}Meta::signatures` - integrity material over the content
///   this pass rewrites, which `apply` drops wholesale rather than rewrites.
/// - the `actors` map's own keys, and each definition's `provider`, `model`,
///   and `keys[i].{type,fingerprint}`. A key is the string `step.actor`
///   names, and `step.actor` is not rewritten, so renaming one would detach
///   every step from its actor. `provider` and `model` are the harness and
///   API vocabularies the derivation reports. A key fingerprint is a digest
///   of a public key: rewriting it hides nothing and leaves any signature
///   that survived the pass unverifiable. The rest of a definition *is*
///   scanned - see `actor_surfaces`.
/// - every non-string leaf - a detector has nothing to span in a number.
/// - anything outside the `Path` handed in. A `Graph`'s own `meta` carries a
///   title, an intent, refs and extras that no call on its member paths can
///   reach; redacting a graph needs a pass of its own.
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
                // `StructuralChange::extra` is `#[serde(flatten)]`: its keys
                // sit directly under `structural` in the document, so an
                // `extra` segment would name a field that is not there.
                let base = format!("/change/{akey}/structural");
                match s.change_type.as_str() {
                    "conversation.append" => turn_surfaces(&mut out, sid, &base, &s.extra),
                    "file.write" => file_write_surfaces(&mut out, sid, &base, &s.extra),
                    // The one place a blind leaf walk is the whole story: the
                    // payload is unmodelled provider JSON.
                    _ => residue(&mut out, sid, &base, &s.extra, &[]),
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
        if let Some(meta) = &step.meta {
            step_meta_surfaces(&mut out, sid, meta);
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
        // A branch name is author-chosen text in a path-shaped field, and a
        // `ref` is whatever someone tagged the state as.
        for (key, value) in [("branch", &b.branch), ("ref", &b.ref_str)] {
            if let Some(v) = value {
                push(
                    &mut out,
                    "",
                    format!("/path/base/{key}"),
                    FieldShape::OpaqueJson,
                    v,
                );
            }
        }
    }
    if let Some(meta) = &path.meta {
        path_meta_surfaces(&mut out, meta);
    }
    out
}

/// A path's own metadata. `PathMeta::extra` is `#[serde(flatten)]`, so its
/// keys sit directly under `/meta` with no segment of their own.
fn path_meta_surfaces(out: &mut Vec<Surface>, meta: &toolpath::v1::PathMeta) {
    for (key, value) in [("title", &meta.title), ("intent", &meta.intent)] {
        if let Some(v) = value {
            push(out, "", format!("/meta/{key}"), FieldShape::Prose, v);
        }
    }
    for (i, r) in meta.refs.iter().enumerate() {
        push(
            out,
            "",
            format!("/meta/refs/{i}/href"),
            FieldShape::Uri,
            &r.href,
        );
    }
    actor_surfaces(out, "", meta.actors.as_ref());
    if let Some(v) = meta.extra.get("vcs_remote") {
        walk_json(out, "", "/meta/vcs_remote", v, FieldShape::Uri);
    }
    // These are byte-identical copies of the `step.change` keys the map
    // already scans. Redacting the key and leaving the copy here hands the
    // reader the original back, so it has to be the same value or neither.
    if let Some(v) = meta.extra.get("files_changed") {
        walk_json(out, "", "/meta/files_changed", v, FieldShape::Uri);
    }
    residue(
        out,
        "",
        "/meta",
        &meta.extra,
        &["vcs_remote", "files_changed"],
    );
}

/// A step's own metadata. `intent` is the git commit message on a
/// `p import git` document and the pull-request or review body on a GitHub
/// one - and since `toolpath-git` emits raw-only changes, on those documents
/// it is the only prose in the whole path.
fn step_meta_surfaces(out: &mut Vec<Surface>, step: &str, meta: &toolpath::v1::StepMeta) {
    if let Some(v) = &meta.intent {
        push(out, step, "/meta/intent".into(), FieldShape::Prose, v);
    }
    for (i, r) in meta.refs.iter().enumerate() {
        push(
            out,
            step,
            format!("/meta/refs/{i}/href"),
            FieldShape::Uri,
            &r.href,
        );
    }
    actor_surfaces(out, step, meta.actors.as_ref());
    if let Some(src) = &meta.source {
        residue(out, step, "/meta/source", &src.extra, &[]);
    }
    residue(out, step, "/meta", &meta.extra, &[]);
}

/// The free text on an actor definition. The rest of the struct is a
/// vocabulary the tooling assigned, but these are not: `toolpath-git` writes
/// the committer's real name into `name` and their email address into
/// `identities[i].id`, and a `keys[i].href` is a URL like any other.
///
/// Sorted by actor key: `actors` is a `HashMap` and emission order is part of
/// the contract.
fn actor_surfaces(
    out: &mut Vec<Surface>,
    step: &str,
    actors: Option<&HashMap<String, toolpath::v1::ActorDefinition>>,
) {
    let Some(actors) = actors else { return };
    let mut keys: Vec<&String> = actors.keys().collect();
    keys.sort();
    for key in keys {
        let def = &actors[key];
        let at = format!("/meta/actors/{}", ptr_escape(key));
        if let Some(v) = &def.name {
            push(out, step, format!("{at}/name"), FieldShape::Prose, v);
        }
        for (i, id) in def.identities.iter().enumerate() {
            let at = format!("{at}/identities/{i}");
            push(
                out,
                step,
                format!("{at}/system"),
                FieldShape::OpaqueJson,
                &id.system,
            );
            push(
                out,
                step,
                format!("{at}/id"),
                FieldShape::OpaqueJson,
                &id.id,
            );
        }
        for (i, k) in def.keys.iter().enumerate() {
            if let Some(href) = &k.href {
                push(
                    out,
                    step,
                    format!("{at}/keys/{i}/href"),
                    FieldShape::Uri,
                    href,
                );
            }
        }
    }
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
    /// Sorted: both backing maps iterate in an order that is not stable
    /// across runs, and emission order is part of the contract.
    fn entries(&self) -> Vec<(&str, &Value)>;
}

impl Fields for HashMap<String, Value> {
    fn field(&self, key: &str) -> Option<&Value> {
        self.get(key)
    }
    fn entries(&self) -> Vec<(&str, &Value)> {
        sorted(self.iter())
    }
}

impl Fields for serde_json::Map<String, Value> {
    fn field(&self, key: &str) -> Option<&Value> {
        self.get(key)
    }
    fn entries(&self) -> Vec<(&str, &Value)> {
        sorted(self.iter())
    }
}

/// Every key of `fields` the typed arm above it did not read, walked blind.
/// The map is a closed list, and a field it does not name is never scanned -
/// so `environment.vcs_branch`, an MCP server name inside `tool_uses[i].name`,
/// or a delegated mutation's `path` would ship a secret with nothing to
/// notice. Emitted after the typed fields, per the emission-order contract.
fn residue(out: &mut Vec<Surface>, step: &str, at: &str, fields: &dyn Fields, consumed: &[&str]) {
    for (key, value) in fields.entries() {
        if consumed.contains(&key) {
            continue;
        }
        walk_json(
            out,
            step,
            &format!("{at}/{}", ptr_escape(key)),
            value,
            FieldShape::OpaqueJson,
        );
    }
}

/// `residue` over a value the caller expected to be an object. A value of
/// some other shape was not read by the typed arm either, so it goes to the
/// blind walk whole rather than falling out of the map.
fn residue_of(
    out: &mut Vec<Surface>,
    step: &str,
    at: &str,
    value: Option<&Value>,
    consumed: &[&str],
) {
    match value {
        Some(Value::Object(map)) => residue(out, step, at, map, consumed),
        Some(other) => walk_json(out, step, at, other, FieldShape::OpaqueJson),
        None => {}
    }
}

/// The array a typed arm expects at `key`, and nothing if it is absent. A
/// value of another shape is walked blind here for the same reason
/// `residue_of` does it: the typed arm below will not read it.
fn typed_array<'a>(
    out: &mut Vec<Surface>,
    step: &str,
    at: &str,
    fields: &'a dyn Fields,
    key: &str,
) -> &'a [Value] {
    match fields.field(key) {
        Some(Value::Array(items)) => items,
        Some(other) => {
            walk_json(
                out,
                step,
                &format!("{at}/{key}"),
                other,
                FieldShape::OpaqueJson,
            );
            &[]
        }
        None => &[],
    }
}

/// The `conversation.append` rows of the map, and - because the two share
/// enough field names to share an arm - the rows of a delegated turn.
///
/// They are not the same object. A top-level turn's extras are assembled
/// field by field by `toolpath_convo::derive_path`; a delegated turn is a
/// whole `Turn` handed to `serde_json::to_value`. The delta, all of it in the
/// delegated direction: `file_mutations` (a top-level turn's are hoisted into
/// sibling `file.write` changes instead), plus `id`, `parent_id`, `timestamp`
/// and `model`, and `role` as serde writes the enum (`"Assistant"`, or
/// externally tagged `{"Other": "tool"}`) rather than the lowercase string
/// the top level stores. Nothing here reads those by name; the residue walk
/// at the end covers them, which is exactly why it exists.
fn turn_surfaces(out: &mut Vec<Surface>, step: &str, at: &str, fields: &dyn Fields) {
    const CONSUMED: &[&str] = &[
        "text",
        "thinking",
        "tool_uses",
        "delegations",
        "file_mutations",
        "environment",
    ];

    for key in ["text", "thinking"] {
        if let Some(v) = fields.field(key) {
            walk_json(out, step, &format!("{at}/{key}"), v, FieldShape::Prose);
        }
    }

    for (i, tool) in typed_array(out, step, at, fields, "tool_uses")
        .iter()
        .enumerate()
    {
        let at = format!("{at}/tool_uses/{i}");
        if let Some(input) = tool.get("input") {
            walk_json(
                out,
                step,
                &format!("{at}/input"),
                input,
                FieldShape::ToolInput,
            );
        }
        if let Some(content) = tool.pointer("/result/content") {
            walk_json(
                out,
                step,
                &format!("{at}/result/content"),
                content,
                FieldShape::ToolOutput,
            );
        }
        residue_of(
            out,
            step,
            &format!("{at}/result"),
            tool.get("result"),
            &["content"],
        );
        // Picks up `name`, which for an MCP call is `mcp__<server>__<tool>` -
        // and the server half comes from the user's own config.
        residue_of(out, step, &at, Some(tool), &["input", "result"]);
    }

    for (i, work) in typed_array(out, step, at, fields, "delegations")
        .iter()
        .enumerate()
    {
        let at = format!("{at}/delegations/{i}");
        for key in ["prompt", "result"] {
            if let Some(v) = work.get(key) {
                walk_json(out, step, &format!("{at}/{key}"), v, FieldShape::Prose);
            }
        }
        for (j, turn) in array(work.get("turns")).iter().enumerate() {
            let at = format!("{at}/turns/{j}");
            match turn.as_object() {
                Some(obj) => turn_surfaces(out, step, &at, obj),
                None => walk_json(out, step, &at, turn, FieldShape::OpaqueJson),
            }
        }
        residue_of(out, step, &at, Some(work), &["prompt", "result", "turns"]);
    }

    // Only a top-level turn's file mutations get hoisted into sibling
    // `file.write` changes; a delegated turn carries its own inline, and they
    // hold the same before/after file content. The residue below is what
    // reaches `path` and `rename_to` on the delegated ones - an entry
    // carrying only `path` has no typed field at all.
    for (i, mutation) in typed_array(out, step, at, fields, "file_mutations")
        .iter()
        .enumerate()
    {
        let at = format!("{at}/file_mutations/{i}");
        if let Some(v) = mutation.get("raw_diff") {
            walk_json(
                out,
                step,
                &format!("{at}/raw_diff"),
                v,
                FieldShape::UnifiedDiff,
            );
        }
        for key in ["before", "after"] {
            if let Some(v) = mutation.get(key) {
                walk_json(
                    out,
                    step,
                    &format!("{at}/{key}"),
                    v,
                    FieldShape::FileContent,
                );
            }
        }
        residue_of(
            out,
            step,
            &at,
            Some(mutation),
            &["raw_diff", "before", "after"],
        );
    }

    if let Some(env) = fields.field("environment") {
        let at = format!("{at}/environment");
        if let Some(v) = env.get("working_dir") {
            walk_json(out, step, &format!("{at}/working_dir"), v, FieldShape::Uri);
        }
        // `vcs_branch` and `vcs_revision` sit right beside `working_dir`, and
        // a branch name is as author-chosen as a commit message.
        residue_of(out, step, &at, Some(env), &["working_dir"]);
    }

    residue(out, step, at, fields, CONSUMED);
}

/// The `file.write` rows: whole-file states, plus both sides of every edit.
/// The residue reaches `rename_to`, which is a path exactly like the one
/// that became this change's artifact key - and that key is scanned.
fn file_write_surfaces(
    out: &mut Vec<Surface>,
    step: &str,
    at: &str,
    extra: &HashMap<String, Value>,
) {
    for key in ["before", "after"] {
        if let Some(v) = extra.get(key) {
            walk_json(
                out,
                step,
                &format!("{at}/{key}"),
                v,
                FieldShape::FileContent,
            );
        }
    }
    for (i, edit) in typed_array(out, step, at, extra, "edits")
        .iter()
        .enumerate()
    {
        walk_json(
            out,
            step,
            &format!("{at}/edits/{i}"),
            edit,
            FieldShape::FileContent,
        );
    }
    residue(out, step, at, extra, &["before", "after", "edits"]);
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
/// writing.
///
/// Read and write resolve identically, with one exception:
/// [`Route::ArtifactKey`] writes by *renaming* the map entry, so once that
/// write lands, the pointer it was made at - and every pointer beneath it -
/// resolves to nothing. That is not a defect to fix here but the reason
/// `surfaces()` emits an artifact's key after everything under it: a caller
/// applying surfaces in emission order never reads through a renamed key.
pub struct SurfaceCursor<'a> {
    pub path: &'a mut toolpath::v1::Path,
}

/// Which object a pointer is relative to. `/meta/…` names `StepMeta` under a
/// step and `PathMeta` at the document root - the pointer alone cannot tell
/// them apart, so read and write must agree on the frame before parsing.
#[derive(Clone, Copy)]
enum Scope {
    Document,
    Step,
}

/// An empty step id is the document frame; anything else is a step's.
fn scope_of(step: &str) -> Scope {
    if step.is_empty() {
        Scope::Document
    } else {
        Scope::Step
    }
}

/// Where a pointer lands, parsed once so read and write cannot drift apart.
enum Route {
    /// A typed `Option<String>` slot on the document itself.
    DocText(DocText),
    DocRefHref(usize),
    /// `PathMeta::extra[field]`, plus an RFC 6901 pointer into it (empty when
    /// the field is itself the string). `extra` is `#[serde(flatten)]`, so
    /// the pointer carries no `extra` segment.
    DocMetaExtra {
        field: String,
        tail: String,
    },
    /// `PathMeta::actors[key]`, and which of its strings.
    DocActor {
        key: String,
        field: ActorField,
    },
    StepIntent,
    StepRefHref(usize),
    StepActor {
        key: String,
        field: ActorField,
    },
    /// `StepMeta::source.extra[field]`, flattened the same way.
    StepSourceExtra {
        field: String,
        tail: String,
    },
    StepMetaExtra {
        field: String,
        tail: String,
    },
    /// The artifact map key itself. Writing renames the entry.
    ArtifactKey(String),
    Raw(String),
    /// `StructuralChange::extra[field]`, flattened the same way.
    Extra {
        artifact: String,
        field: String,
        tail: String,
    },
}

/// Which string on an `ActorDefinition` a pointer names. The fields left out
/// are the ones `surfaces()` does not emit, so a pointer at one of them
/// resolves to nothing - which is what an unnamed field should do.
#[derive(Clone, Copy)]
enum ActorField {
    Name,
    IdentitySystem(usize),
    IdentityId(usize),
    KeyHref(usize),
}

/// The document's typed string slots, named so read and write share a parse.
#[derive(Clone, Copy)]
enum DocText {
    BaseUri,
    BaseBranch,
    BaseRef,
    MetaTitle,
    MetaIntent,
}

fn route(scope: Scope, at: &str) -> Option<Route> {
    match scope {
        Scope::Document => route_document(at),
        Scope::Step => route_step(at),
    }
}

fn route_document(at: &str) -> Option<Route> {
    if let Some(field) = at.strip_prefix("/path/base/") {
        return match field {
            "uri" => Some(Route::DocText(DocText::BaseUri)),
            "branch" => Some(Route::DocText(DocText::BaseBranch)),
            "ref" => Some(Route::DocText(DocText::BaseRef)),
            _ => None,
        };
    }

    let rest = at.strip_prefix("/meta/")?;
    match rest {
        "title" => return Some(Route::DocText(DocText::MetaTitle)),
        "intent" => return Some(Route::DocText(DocText::MetaIntent)),
        _ => {}
    }
    if let Some(i) = ref_href_index(rest) {
        return Some(Route::DocRefHref(i));
    }
    if let Some((key, field)) = actor_route(rest) {
        return Some(Route::DocActor { key, field });
    }
    let (field, tail) = split_field(rest);
    Some(Route::DocMetaExtra { field, tail })
}

fn route_step(at: &str) -> Option<Route> {
    if let Some(rest) = at.strip_prefix("/meta/") {
        if rest == "intent" {
            return Some(Route::StepIntent);
        }
        if let Some(i) = ref_href_index(rest) {
            return Some(Route::StepRefHref(i));
        }
        if let Some((key, field)) = actor_route(rest) {
            return Some(Route::StepActor { key, field });
        }
        if let Some(rest) = rest.strip_prefix("source/") {
            let (field, tail) = split_field(rest);
            return Some(Route::StepSourceExtra { field, tail });
        }
        let (field, tail) = split_field(rest);
        return Some(Route::StepMetaExtra { field, tail });
    }

    let rest = at.strip_prefix("/change/")?;
    let Some((akey, rest)) = rest.split_once('/') else {
        return (!rest.is_empty()).then(|| Route::ArtifactKey(ptr_decode(rest)));
    };
    let artifact = ptr_decode(akey);
    if rest == "raw" {
        return Some(Route::Raw(artifact));
    }

    let rest = rest.strip_prefix("structural/")?;
    let (field, tail) = split_field(rest);
    Some(Route::Extra {
        artifact,
        field,
        tail,
    })
}

/// `actors/{key}/…` - the strings the map names on an actor definition. An
/// actor key is pointer-escaped, so it never holds a literal `/` and the
/// first split always lands on the key boundary.
fn actor_route(rest: &str) -> Option<(String, ActorField)> {
    let (key, rest) = rest.strip_prefix("actors/")?.split_once('/')?;
    let field = if rest == "name" {
        ActorField::Name
    } else {
        let (list, rest) = rest.split_once('/')?;
        let (index, leaf) = rest.split_once('/')?;
        let i = index.parse().ok()?;
        match (list, leaf) {
            ("identities", "system") => ActorField::IdentitySystem(i),
            ("identities", "id") => ActorField::IdentityId(i),
            ("keys", "href") => ActorField::KeyHref(i),
            _ => return None,
        }
    };
    Some((ptr_decode(key), field))
}

/// `refs/{i}/href` - the only leaf under `refs` the map names.
fn ref_href_index(rest: &str) -> Option<usize> {
    let (index, leaf) = rest.strip_prefix("refs/")?.split_once('/')?;
    if leaf != "href" {
        return None;
    }
    index.parse().ok()
}

/// Split a flattened-extras pointer into the map key and an RFC 6901 pointer
/// into that key's value (empty when the value is itself the string).
///
/// An empty key is a key: `{"": "secret"}` is legal JSON, the residue walk
/// emits a surface for it, and refusing to resolve it here would count that
/// field as scanned while no detector ever saw it.
fn split_field(rest: &str) -> (String, String) {
    match rest.split_once('/') {
        Some((field, tail)) => (ptr_decode(field), format!("/{tail}")),
        None => (ptr_decode(rest), String::new()),
    }
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

/// The string a `(step, pointer)` pair names, read without the `&mut` a
/// `SurfaceCursor` demands. Plan generation and `verify` only ever read, and
/// nothing that only reads should have to clone the document to do it.
pub fn read_at(path: &toolpath::v1::Path, step: &str, at: &str) -> Option<String> {
    resolve(path, scope_of(step), find_step(path, step), at)
}

/// `read_at` with the step already resolved. A caller reading thousands of
/// surfaces indexes `path.steps` once instead of scanning it per pointer.
///
/// `None` means the document frame - the same thing an empty step id means to
/// `read_at`. A caller holding a step id that does not resolve must reject it
/// before calling, or a step-relative `/meta/…` silently reads the path's
/// metadata instead.
pub fn read_at_in(
    path: &toolpath::v1::Path,
    step: Option<&toolpath::v1::Step>,
    at: &str,
) -> Option<String> {
    let scope = match step {
        Some(_) => Scope::Step,
        None => Scope::Document,
    };
    resolve(path, scope, step, at)
}

fn resolve(
    path: &toolpath::v1::Path,
    scope: Scope,
    step: Option<&toolpath::v1::Step>,
    at: &str,
) -> Option<String> {
    match route(scope, at)? {
        Route::DocText(field) => doc_text(path, field).cloned(),
        Route::DocRefHref(i) => Some(path.meta.as_ref()?.refs.get(i)?.href.clone()),
        Route::DocMetaExtra { field, tail } => leaf(&path.meta.as_ref()?.extra, &field, &tail),
        Route::DocActor { key, field } => {
            actor_text(path.meta.as_ref()?.actors.as_ref()?.get(&key)?, field).cloned()
        }
        Route::StepIntent => step?.meta.as_ref()?.intent.clone(),
        Route::StepRefHref(i) => Some(step?.meta.as_ref()?.refs.get(i)?.href.clone()),
        Route::StepActor { key, field } => {
            actor_text(step?.meta.as_ref()?.actors.as_ref()?.get(&key)?, field).cloned()
        }
        Route::StepSourceExtra { field, tail } => {
            leaf(&step?.meta.as_ref()?.source.as_ref()?.extra, &field, &tail)
        }
        Route::StepMetaExtra { field, tail } => leaf(&step?.meta.as_ref()?.extra, &field, &tail),
        Route::ArtifactKey(key) => step?.change.contains_key(&key).then_some(key),
        Route::Raw(key) => step?.change.get(&key)?.raw.clone(),
        Route::Extra {
            artifact,
            field,
            tail,
        } => leaf(
            &step?.change.get(&artifact)?.structural.as_ref()?.extra,
            &field,
            &tail,
        ),
    }
}

/// The document's typed string slots. A `_mut` twin follows; the two must
/// stay in step, which is why both are one `match` over the same enum.
fn doc_text(path: &toolpath::v1::Path, field: DocText) -> Option<&String> {
    match field {
        DocText::BaseUri => Some(&path.path.base.as_ref()?.uri),
        DocText::BaseBranch => path.path.base.as_ref()?.branch.as_ref(),
        DocText::BaseRef => path.path.base.as_ref()?.ref_str.as_ref(),
        DocText::MetaTitle => path.meta.as_ref()?.title.as_ref(),
        DocText::MetaIntent => path.meta.as_ref()?.intent.as_ref(),
    }
}

fn doc_text_mut(path: &mut toolpath::v1::Path, field: DocText) -> Option<&mut String> {
    match field {
        DocText::BaseUri => Some(&mut path.path.base.as_mut()?.uri),
        DocText::BaseBranch => path.path.base.as_mut()?.branch.as_mut(),
        DocText::BaseRef => path.path.base.as_mut()?.ref_str.as_mut(),
        DocText::MetaTitle => path.meta.as_mut()?.title.as_mut(),
        DocText::MetaIntent => path.meta.as_mut()?.intent.as_mut(),
    }
}

/// An actor's named string. A `_mut` twin follows; the two must stay in step,
/// which is why both are one `match` over the same enum.
fn actor_text(def: &toolpath::v1::ActorDefinition, field: ActorField) -> Option<&String> {
    match field {
        ActorField::Name => def.name.as_ref(),
        ActorField::IdentitySystem(i) => Some(&def.identities.get(i)?.system),
        ActorField::IdentityId(i) => Some(&def.identities.get(i)?.id),
        ActorField::KeyHref(i) => def.keys.get(i)?.href.as_ref(),
    }
}

fn actor_text_mut(
    def: &mut toolpath::v1::ActorDefinition,
    field: ActorField,
) -> Option<&mut String> {
    match field {
        ActorField::Name => def.name.as_mut(),
        ActorField::IdentitySystem(i) => Some(&mut def.identities.get_mut(i)?.system),
        ActorField::IdentityId(i) => Some(&mut def.identities.get_mut(i)?.id),
        ActorField::KeyHref(i) => def.keys.get_mut(i)?.href.as_mut(),
    }
}

/// A flattened-extras leaf: the map key, then the pointer into its value.
fn leaf(extra: &HashMap<String, Value>, field: &str, tail: &str) -> Option<String> {
    let value = extra.get(field)?;
    let leaf = if tail.is_empty() {
        value
    } else {
        value.pointer(tail)?
    };
    Some(leaf.as_str()?.to_string())
}

fn leaf_mut<'a>(
    extra: &'a mut HashMap<String, Value>,
    field: &str,
    tail: &str,
) -> Option<&'a mut String> {
    let value = extra.get_mut(field)?;
    let leaf = if tail.is_empty() {
        value
    } else {
        value.pointer_mut(tail)?
    };
    string_slot(leaf)
}

impl SurfaceCursor<'_> {
    pub fn read(&self, step: &str, at: &str) -> Option<String> {
        read_at(self.path, step, at)
    }

    pub fn write(&mut self, step: &str, at: &str, value: &str) -> Result<()> {
        let bad = || RedactError::BadPointer(at.to_string());
        match route(scope_of(step), at).ok_or_else(bad)? {
            Route::DocText(field) => {
                *doc_text_mut(self.path, field).ok_or_else(bad)? = value.to_string();
            }
            Route::DocRefHref(i) => {
                self.path
                    .meta
                    .as_mut()
                    .ok_or_else(bad)?
                    .refs
                    .get_mut(i)
                    .ok_or_else(bad)?
                    .href = value.to_string();
            }
            Route::DocMetaExtra { field, tail } => {
                let extra = &mut self.path.meta.as_mut().ok_or_else(bad)?.extra;
                *leaf_mut(extra, &field, &tail).ok_or_else(bad)? = value.to_string();
            }
            Route::DocActor { key, field } => {
                let actors = self
                    .path
                    .meta
                    .as_mut()
                    .ok_or_else(bad)?
                    .actors
                    .as_mut()
                    .ok_or_else(bad)?;
                let def = actors.get_mut(&key).ok_or_else(bad)?;
                *actor_text_mut(def, field).ok_or_else(bad)? = value.to_string();
            }
            Route::StepActor { key, field } => {
                let actors = find_step_mut(self.path, step)
                    .ok_or_else(bad)?
                    .meta
                    .as_mut()
                    .ok_or_else(bad)?
                    .actors
                    .as_mut()
                    .ok_or_else(bad)?;
                let def = actors.get_mut(&key).ok_or_else(bad)?;
                *actor_text_mut(def, field).ok_or_else(bad)? = value.to_string();
            }
            Route::StepIntent => {
                let meta = find_step_mut(self.path, step)
                    .ok_or_else(bad)?
                    .meta
                    .as_mut()
                    .ok_or_else(bad)?;
                *meta.intent.as_mut().ok_or_else(bad)? = value.to_string();
            }
            Route::StepRefHref(i) => {
                find_step_mut(self.path, step)
                    .ok_or_else(bad)?
                    .meta
                    .as_mut()
                    .ok_or_else(bad)?
                    .refs
                    .get_mut(i)
                    .ok_or_else(bad)?
                    .href = value.to_string();
            }
            Route::StepSourceExtra { field, tail } => {
                let extra = &mut find_step_mut(self.path, step)
                    .ok_or_else(bad)?
                    .meta
                    .as_mut()
                    .ok_or_else(bad)?
                    .source
                    .as_mut()
                    .ok_or_else(bad)?
                    .extra;
                *leaf_mut(extra, &field, &tail).ok_or_else(bad)? = value.to_string();
            }
            Route::StepMetaExtra { field, tail } => {
                let extra = &mut find_step_mut(self.path, step)
                    .ok_or_else(bad)?
                    .meta
                    .as_mut()
                    .ok_or_else(bad)?
                    .extra;
                *leaf_mut(extra, &field, &tail).ok_or_else(bad)? = value.to_string();
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
                let extra = &mut find_step_mut(self.path, step)
                    .ok_or_else(bad)?
                    .change
                    .get_mut(&artifact)
                    .ok_or_else(bad)?
                    .structural
                    .as_mut()
                    .ok_or_else(bad)?
                    .extra;
                *leaf_mut(extra, &field, &tail).ok_or_else(bad)? = value.to_string();
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
pub(crate) fn ptr_decode(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;
    use toolpath::v1::{
        ActorDefinition, ArtifactChange, Base, Identity, Key, Path, PathMeta, Ref, Step, StepMeta,
        StructuralChange, VcsSource,
    };

    /// A `human:` actor as `toolpath-git` writes one: the committer's real
    /// name and their email address, neither of which is a vocabulary.
    fn actors() -> HashMap<String, ActorDefinition> {
        HashMap::from([
            (
                "human:alex".to_string(),
                ActorDefinition {
                    name: Some("Alex Mercer".into()),
                    identities: vec![Identity {
                        system: "email".into(),
                        id: "alex@acme-internal.example".into(),
                    }],
                    keys: vec![Key {
                        key_type: "ssh-ed25519".into(),
                        fingerprint: "SHA256:abc".into(),
                        href: Some("https://alex:tok@keys.acme-internal.example/alex.pub".into()),
                    }],
                    ..ActorDefinition::default()
                },
            ),
            (
                "agent:claude-opus-5".to_string(),
                ActorDefinition {
                    name: Some("claude-opus-5".into()),
                    provider: Some("claude-code".into()),
                    model: Some("claude-opus-5-20260101".into()),
                    ..ActorDefinition::default()
                },
            ),
        ])
    }

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

    fn signature() -> toolpath::v1::Signature {
        toolpath::v1::Signature {
            signer: "human:alex".into(),
            key: "SHA256:abc".into(),
            scope: "path".into(),
            sig: "MEUCIQDexample".into(),
            timestamp: Some("2026-07-30T10:00:00Z".into()),
        }
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
                "environment": {
                    "working_dir": "/Users/alex/work/repo",
                    "vcs_branch": "evan/redact",
                    "vcs_revision": "7a7c366"
                },
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
                        }, {
                            // Only `path`: no typed field at all, so this
                            // entry is the whole test for the residue walk.
                            "path": "/srv/acme-internal/rotate.sh"
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
    /// read/write sweeps and the leaf-coverage proof.
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
                                "rename_to": "~/acme-internal-notes.md",
                                "edits": [{"old_string": "a", "new_string": "b", "replace_all": false}]
                            }),
                        ),
                    ),
                ],
            ),
            fixture_with_delegation().steps.remove(0),
            fixture_unknown_change_type().steps.remove(0),
        ]);
        // A derived document is a chain, and `parents` is a list of strings
        // the map declines to name. Absent from the fixture, that exclusion
        // would pass the coverage proof by not being there.
        path.steps[1].step.parents = vec!["turn-0f3a".into()];
        path.steps[2].step.parents = vec!["turn-deleg".into()];
        path.steps[0].meta = Some(StepMeta {
            intent: Some("rotate the deploy credentials".into()),
            source: Some(VcsSource {
                vcs_type: "git".into(),
                revision: "abc123".into(),
                change_id: Some("I8473b95934b5732ac55d26311a706c9c2bde9940".into()),
                extra: object(json!({"author_email": "alex@acme-internal.example"})),
            }),
            refs: vec![Ref {
                rel: "pull-request".into(),
                href: "https://github.com/o/r/pull/42".into(),
            }],
            extra: object(json!({"note": "cherry-picked from the release branch"})),
            actors: Some(actors()),
            signatures: vec![signature()],
        });
        path.path.base = Some(Base {
            uri: "https://alex:tok@github.com/o/r".into(),
            ref_str: Some("abc123".into()),
            branch: Some("evan/redact".into()),
        });
        path.path.graph_ref = Some("toolpath://archive/release-v2".into());
        path.meta = Some(PathMeta {
            title: Some("redaction field map".into()),
            kind: Some(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION.into()),
            source: Some("claude-code".into()),
            intent: Some("close the coverage gaps".into()),
            refs: vec![Ref {
                rel: "self".into(),
                href: "https://pathbase.dev/o/r/redact".into(),
            }],
            extra: object(json!({
                "vcs_remote": "https://alex:tok@github.com/o/r.git",
                // Byte-identical to a `step.change` key the map scans.
                "files_changed": ["~/notes.md", "/srv/acme-internal/rotate.sh"]
            })),
            actors: Some(actors()),
            signatures: vec![signature()],
        });
        path
    }

    fn ats(path: &Path) -> Vec<String> {
        surfaces(path).into_iter().map(|s| s.at).collect()
    }

    /// Every non-empty string leaf in `value`, named by absolute pointer.
    /// Empty ones are excluded because `push` excludes them: there is nothing
    /// in an empty string for a detector to span.
    fn string_leaves(value: &Value, at: String, out: &mut Vec<String>) {
        match value {
            Value::String(s) => {
                if !s.is_empty() {
                    out.push(at);
                }
            }
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    string_leaves(item, format!("{at}/{i}"), out);
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    string_leaves(item, format!("{at}/{}", ptr_escape(key)), out);
                }
            }
            _ => {}
        }
    }

    /// A surface's pointer rebased on the document root - where whoever reads
    /// the audit record will actually try to follow it.
    fn absolute(path: &Path, s: &Surface) -> String {
        match path.steps.iter().position(|st| st.step.id == s.step) {
            Some(i) => format!("/steps/{i}{}", s.at),
            None => s.at.clone(),
        }
    }

    /// The map is a closed list: a field it does not name is never scanned,
    /// so a secret there ships and nothing notices. This walks the other
    /// direction - from the document to the map - so a new field in
    /// `toolpath` or `toolpath-convo` cannot quietly go unscanned. Every
    /// exclusion is named below with the reason it is not a candidate.
    ///
    /// This is only as good as `fixture_rich()` is complete, so the fixture
    /// carries every string-bearing field of the `Path` type tree - including
    /// the ones the map declines to name. A field the fixture does not carry
    /// passes here by being absent, which is the failure this test exists to
    /// prevent.
    #[test]
    fn every_string_leaf_in_a_derived_document_is_surfaced_exactly_once() {
        const EXPECTED_OUT_OF_SCOPE: &[(&str, &str)] = &[
            ("/path/id", "document identity; plans are keyed on it"),
            ("/path/head", "names a step id, not content"),
            (
                "/path/graph_ref",
                "a toolpath-internal link to a sibling document",
            ),
            ("/steps/1/step/parents/0", "names a step id, not content"),
            ("/steps/2/step/parents/0", "names a step id, not content"),
            ("/steps/0/step/id", "step identity; the DAG is keyed on it"),
            ("/steps/1/step/id", "step identity; the DAG is keyed on it"),
            ("/steps/2/step/id", "step identity; the DAG is keyed on it"),
            ("/steps/0/step/actor", "`type:name`, a closed vocabulary"),
            ("/steps/1/step/actor", "`type:name`, a closed vocabulary"),
            ("/steps/2/step/actor", "`type:name`, a closed vocabulary"),
            ("/steps/0/step/timestamp", "machine-generated ISO 8601"),
            ("/steps/1/step/timestamp", "machine-generated ISO 8601"),
            ("/steps/2/step/timestamp", "machine-generated ISO 8601"),
            (
                "/steps/0/change/claude:~1~1sess-abc/structural/type",
                "change type: a vocabulary this format defines",
            ),
            (
                "/steps/0/change/~0~1notes.md/structural/type",
                "change type: a vocabulary this format defines",
            ),
            (
                "/steps/1/change/claude:~1~1sess-abc/structural/type",
                "change type: a vocabulary this format defines",
            ),
            (
                "/steps/2/change/claude:~1~1sess-abc/structural/type",
                "change type: a vocabulary this format defines",
            ),
            ("/steps/0/meta/source/type", "VCS name, a closed vocabulary"),
            (
                "/steps/0/meta/source/revision",
                "a commit hash the VCS assigned",
            ),
            ("/steps/0/meta/refs/0/rel", "a link relation name"),
            ("/meta/refs/0/rel", "a link relation name"),
            (
                "/meta/actors/agent:claude-opus-5/provider",
                "the harness name the derivation reports",
            ),
            (
                "/steps/0/meta/actors/agent:claude-opus-5/provider",
                "the harness name the derivation reports",
            ),
            (
                "/meta/actors/agent:claude-opus-5/model",
                "a model id the API assigns",
            ),
            (
                "/steps/0/meta/actors/agent:claude-opus-5/model",
                "a model id the API assigns",
            ),
            (
                "/meta/actors/human:alex/keys/0/type",
                "a key-algorithm name, a closed vocabulary",
            ),
            (
                "/steps/0/meta/actors/human:alex/keys/0/type",
                "a key-algorithm name, a closed vocabulary",
            ),
            (
                "/meta/actors/human:alex/keys/0/fingerprint",
                "a digest of a public key; rewriting hides nothing",
            ),
            (
                "/steps/0/meta/actors/human:alex/keys/0/fingerprint",
                "a digest of a public key; rewriting hides nothing",
            ),
            ("/meta/kind", "a kind URI this format publishes"),
            ("/meta/source", "the deriving harness, a closed vocabulary"),
            (
                "/steps/0/meta/source/change_id",
                "a change id the VCS assigned",
            ),
            // `apply` clears these rather than rewriting them: a signature
            // over redacted content cannot be repaired, only dropped.
            ("/meta/signatures/0/signer", "integrity material; dropped"),
            ("/meta/signatures/0/key", "integrity material; dropped"),
            ("/meta/signatures/0/scope", "integrity material; dropped"),
            ("/meta/signatures/0/sig", "integrity material; dropped"),
            (
                "/meta/signatures/0/timestamp",
                "integrity material; dropped",
            ),
            (
                "/steps/0/meta/signatures/0/signer",
                "integrity material; dropped",
            ),
            (
                "/steps/0/meta/signatures/0/key",
                "integrity material; dropped",
            ),
            (
                "/steps/0/meta/signatures/0/scope",
                "integrity material; dropped",
            ),
            (
                "/steps/0/meta/signatures/0/sig",
                "integrity material; dropped",
            ),
            (
                "/steps/0/meta/signatures/0/timestamp",
                "integrity material; dropped",
            ),
        ];

        let p = fixture_rich();
        let doc = serde_json::to_value(&p).unwrap();

        let mut found = Vec::new();
        string_leaves(&doc, String::new(), &mut found);
        let leaves: BTreeSet<String> = found.into_iter().collect();

        let scanned: Vec<String> = surfaces(&p).iter().map(|s| absolute(&p, s)).collect();
        let unique: BTreeSet<String> = scanned.iter().cloned().collect();
        assert_eq!(
            unique.len(),
            scanned.len(),
            "a field scanned twice is a finding reported twice: {scanned:#?}"
        );

        let allowed: BTreeSet<&str> = EXPECTED_OUT_OF_SCOPE.iter().map(|(at, _)| *at).collect();
        assert_eq!(
            allowed.len(),
            EXPECTED_OUT_OF_SCOPE.len(),
            "a duplicated allowlist entry hides how much is excluded"
        );
        for at in &allowed {
            assert!(
                leaves.contains(*at),
                "allowlist entry names nothing in the fixture: {at}"
            );
        }

        let missed: Vec<&String> = leaves
            .difference(&unique)
            .filter(|a| !allowed.contains(a.as_str()))
            .collect();
        assert!(
            missed.is_empty(),
            "string leaves no detector will ever see: {missed:#?}"
        );

        // An artifact key is the one surface that names a map *key*, so it
        // resolves to the object beneath it rather than to a string. Nothing
        // else may: a surface aimed at a subtree reports a whole object as
        // one scanned field, and no detector can span that.
        let artifact_key = |at: &str| {
            let mut segs = at.split('/').skip(1);
            matches!(
                (
                    segs.next(),
                    segs.next(),
                    segs.next(),
                    segs.next(),
                    segs.next()
                ),
                (Some("steps"), Some(_), Some("change"), Some(_), None)
            ) && doc.pointer(at).is_some_and(Value::is_object)
        };
        let phantom: Vec<&String> = unique
            .difference(&leaves)
            .filter(|a| !artifact_key(a))
            .collect();
        assert!(
            phantom.is_empty(),
            "surfaces whose pointers resolve to nothing: {phantom:#?}"
        );
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
        assert!(ats.iter().any(|a| a.ends_with("/structural/text")));
        assert!(ats.iter().any(|a| a.ends_with("/structural/thinking")));
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
                .any(|s| s.at.ends_with("/structural/text"))
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
    }

    /// The emission order in full, over the fixture that exercises every arm.
    /// Finding ids are positional over this list and a regenerated plan is
    /// compared byte for byte, so the order is as much a contract as the
    /// pointers are. One literal pins all of it at once: steps in document
    /// order, artifacts sorted by key, each artifact's own key after
    /// everything beneath it, and every typed field ahead of the residue
    /// walk that follows it.
    #[test]
    fn the_emission_order_is_pinned() {
        const GOLDEN: &[(&str, &str)] = &[
            ("turn-0f3a", "/change/claude:~1~1sess-abc/structural/text"),
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/thinking",
            ),
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/tool_uses/0/input/command",
            ),
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/tool_uses/0/result/content",
            ),
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/tool_uses/0/category",
            ),
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/tool_uses/0/id",
            ),
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/tool_uses/0/name",
            ),
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/environment/working_dir",
            ),
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/environment/vcs_branch",
            ),
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/environment/vcs_revision",
            ),
            ("turn-0f3a", "/change/claude:~1~1sess-abc/structural/role"),
            ("turn-0f3a", "/change/claude:~1~1sess-abc"),
            ("turn-0f3a", "/change/~0~1notes.md/raw"),
            ("turn-0f3a", "/change/~0~1notes.md/structural/before"),
            ("turn-0f3a", "/change/~0~1notes.md/structural/after"),
            (
                "turn-0f3a",
                "/change/~0~1notes.md/structural/edits/0/new_string",
            ),
            (
                "turn-0f3a",
                "/change/~0~1notes.md/structural/edits/0/old_string",
            ),
            ("turn-0f3a", "/change/~0~1notes.md/structural/rename_to"),
            ("turn-0f3a", "/change/~0~1notes.md"),
            ("turn-0f3a", "/meta/intent"),
            ("turn-0f3a", "/meta/refs/0/href"),
            ("turn-0f3a", "/meta/actors/agent:claude-opus-5/name"),
            ("turn-0f3a", "/meta/actors/human:alex/name"),
            ("turn-0f3a", "/meta/actors/human:alex/identities/0/system"),
            ("turn-0f3a", "/meta/actors/human:alex/identities/0/id"),
            ("turn-0f3a", "/meta/actors/human:alex/keys/0/href"),
            ("turn-0f3a", "/meta/source/author_email"),
            ("turn-0f3a", "/meta/note"),
            ("turn-deleg", "/change/claude:~1~1sess-abc/structural/text"),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/prompt",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/result",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/text",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/tool_uses/0/input/file_path",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/tool_uses/0/result/content",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/tool_uses/0/id",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/tool_uses/0/name",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/file_mutations/0/raw_diff",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/file_mutations/0/before",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/file_mutations/0/after",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/file_mutations/0/path",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/file_mutations/1/path",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/id",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/role",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/turns/0/timestamp",
            ),
            (
                "turn-deleg",
                "/change/claude:~1~1sess-abc/structural/delegations/0/agent_id",
            ),
            ("turn-deleg", "/change/claude:~1~1sess-abc/structural/role"),
            ("turn-deleg", "/change/claude:~1~1sess-abc"),
            ("evt-1", "/change/claude:~1~1sess-abc/structural/data/a~1b"),
            (
                "evt-1",
                "/change/claude:~1~1sess-abc/structural/data/nested/0",
            ),
            ("evt-1", "/change/claude:~1~1sess-abc/structural/event_type"),
            ("evt-1", "/change/claude:~1~1sess-abc"),
            ("", "/path/base/uri"),
            ("", "/path/base/branch"),
            ("", "/path/base/ref"),
            ("", "/meta/title"),
            ("", "/meta/intent"),
            ("", "/meta/refs/0/href"),
            ("", "/meta/actors/agent:claude-opus-5/name"),
            ("", "/meta/actors/human:alex/name"),
            ("", "/meta/actors/human:alex/identities/0/system"),
            ("", "/meta/actors/human:alex/identities/0/id"),
            ("", "/meta/actors/human:alex/keys/0/href"),
            ("", "/meta/vcs_remote"),
            ("", "/meta/files_changed/0"),
            ("", "/meta/files_changed/1"),
        ];

        let actual: Vec<(String, String)> = surfaces(&fixture_rich())
            .into_iter()
            .map(|s| (s.step, s.at))
            .collect();
        let expected: Vec<(String, String)> = GOLDEN
            .iter()
            .map(|(step, at)| (step.to_string(), at.to_string()))
            .collect();
        assert_eq!(actual, expected);
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
        assert!(
            !ats.iter().any(|a| a.ends_with("/structural/text")),
            "{ats:?}"
        );
        assert!(ats.iter().any(|a| a.ends_with("/structural/thinking")));
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
        let leaf = "/change/claude:~1~1sess-abc/structural/tool_uses/0/input";
        assert!(ats.contains(&format!("{leaf}/env/AWS_KEY")), "{ats:?}");
        assert!(ats.contains(&format!("{leaf}/argv/0")));
        assert!(!ats.iter().any(|a| a.ends_with("/retries")));
    }

    #[test]
    fn blind_walk_escapes_object_keys() {
        let p = fixture_unknown_change_type();
        let at = "/change/claude:~1~1sess-abc/structural/data/a~1b";
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
            c.read("turn-0f3a", "/change/~0~1notes.md/structural/before")
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
        // Through `read_at`, not a cursor: plan generation reads every
        // surface in the document and has no business cloning one to do it.
        let p = fixture_rich();
        let all = surfaces(&p);
        assert!(all.len() > 40, "fixture is too thin: {}", all.len());
        for s in &all {
            assert!(
                read_at(&p, &s.step, &s.at).is_some(),
                "surface does not resolve: {}",
                s.at
            );
        }
    }

    /// The fields the closed map used to leave out. Each carried a real value
    /// in a captured session and none was ever handed to a detector.
    #[test]
    fn the_fields_the_closed_map_used_to_miss_are_scanned() {
        let p = fixture_rich();
        let named: BTreeSet<(String, String)> =
            surfaces(&p).into_iter().map(|s| (s.step, s.at)).collect();
        let convo = "/change/claude:~1~1sess-abc/structural";
        for (step, at) in [
            // Sits beside the `working_dir` the map already scanned; 13
            // occurrences of a branch name in one real 753-step session.
            ("turn-0f3a", format!("{convo}/environment/vcs_branch")),
            ("turn-0f3a", format!("{convo}/environment/vcs_revision")),
            // A path, exactly like the one that became the artifact key.
            (
                "turn-0f3a",
                "/change/~0~1notes.md/structural/rename_to".into(),
            ),
            // `mcp__<server>__<tool>`, and the server half is user config.
            ("turn-0f3a", format!("{convo}/tool_uses/0/name")),
            // A delegated mutation carrying only `path` had no typed field
            // at all, so it produced zero surfaces - while the identical
            // value at the top level was scanned.
            (
                "turn-deleg",
                format!("{convo}/delegations/0/turns/0/file_mutations/1/path"),
            ),
            // The git commit message, and the PR body on a GitHub import.
            ("turn-0f3a", "/meta/intent".into()),
            ("turn-0f3a", "/meta/source/author_email".into()),
            ("turn-0f3a", "/meta/refs/0/href".into()),
            ("turn-0f3a", "/meta/note".into()),
            ("", "/path/base/branch".into()),
            ("", "/path/base/ref".into()),
            ("", "/meta/title".into()),
            ("", "/meta/intent".into()),
            ("", "/meta/refs/0/href".into()),
            // Byte-identical to a `step.change` key the map does scan, so
            // redacting only the key leaves the original in the document.
            ("", "/meta/files_changed/0".into()),
            // An actor definition was excluded as material redaction "drops
            // wholesale" - but only `signatures` is ever dropped, so a git
            // import shipped the committer's name and email unscanned.
            ("", "/meta/actors/human:alex/name".into()),
            ("", "/meta/actors/human:alex/identities/0/id".into()),
            ("", "/meta/actors/human:alex/keys/0/href".into()),
            ("turn-0f3a", "/meta/actors/human:alex/name".into()),
            (
                "turn-0f3a",
                "/meta/actors/human:alex/identities/0/id".into(),
            ),
        ] {
            assert!(
                named.contains(&(step.to_string(), at.clone())),
                "not scanned: {step} {at}"
            );
            assert!(
                read_at(&p, step, &at).is_some(),
                "scanned but unreadable: {step} {at}"
            );
        }
    }

    #[test]
    fn an_empty_extra_key_still_resolves() {
        // `{"": "…"}` is legal JSON and the walk emits a surface for it, so
        // the pass counts that field as scanned. A route that refused it
        // would make the count a lie: no detector would ever see the value.
        let mut p = path_of(vec![append_step(
            "turn-empty-key",
            json!({"": "AKIAIOSFODNN7EXAMPLE"}),
        )]);
        let at = "/change/claude:~1~1sess-abc/structural/";
        assert!(ats(&p).contains(&at.to_string()), "{:?}", ats(&p));

        let mut c = SurfaceCursor { path: &mut p };
        assert_eq!(
            c.read("turn-empty-key", at).as_deref(),
            Some("AKIAIOSFODNN7EXAMPLE")
        );
        c.write("turn-empty-key", at, "x").unwrap();
        assert_eq!(c.read("turn-empty-key", at).as_deref(), Some("x"));
    }

    #[test]
    fn renaming_an_artifact_key_invalidates_pointers_beneath_it() {
        // The one place read and write stop agreeing, and the whole reason
        // `surfaces()` emits an artifact's key after everything under it.
        let mut p = fixture_file_write();
        let mut c = SurfaceCursor { path: &mut p };
        let raw = "/change/src~1config.rs/raw";
        assert!(c.read("turn-9c21", raw).is_some());

        c.write("turn-9c21", "/change/src~1config.rs", "redacted.rs")
            .unwrap();
        assert_eq!(c.read("turn-9c21", raw), None);
        assert!(c.read("turn-9c21", "/change/redacted.rs/raw").is_some());
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
            ("turn-0f3a", "/change/claude:~1~1sess-abc/structural/nope"),
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/tool_uses/9/input",
            ),
            // `token_usage` is an object, not a string leaf.
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/token_usage",
            ),
            (
                "no-such-step",
                "/change/claude:~1~1sess-abc/structural/text",
            ),
            ("turn-0f3a", "/step/actor"),
            // The `extra` segment used to be part of the pointer, and it
            // named nothing: `StructuralChange::extra` is flattened.
            (
                "turn-0f3a",
                "/change/claude:~1~1sess-abc/structural/extra/text",
            ),
            // `type` is `structural`'s one typed sibling, and it is not in
            // the extras map the pointer resolves against.
            ("turn-0f3a", "/change/claude:~1~1sess-abc/structural/type"),
            ("", "/meta/not_present"),
            // The actor fields the map deliberately leaves out. An unnamed
            // field must resolve to nothing, or the pass would report a
            // surface no detector was ever handed.
            ("", "/meta/actors/agent:claude-opus-5/model"),
            ("", "/meta/actors/agent:claude-opus-5/provider"),
            ("", "/meta/actors/human:alex/keys/0/fingerprint"),
            ("", "/meta/actors/human:alex/keys/0/type"),
            ("", "/meta/actors/no-such-actor/name"),
            ("", "/meta/actors/human:alex/identities/9/id"),
            ("turn-0f3a", "/meta/actors/agent:claude-opus-5/model"),
            // `turn-deleg` has no metadata at all, so nothing under it can.
            ("turn-deleg", "/meta/actors/human:alex/name"),
            ("", "/path/base/nope"),
            ("", "/meta/refs/9/href"),
            ("", "/meta/refs/0/rel"),
            // A step-relative `/meta/…` must not fall through to the path's
            // metadata, whether the step is missing or just has none.
            ("no-such-step", "/meta/intent"),
            ("turn-deleg", "/meta/intent"),
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
        let base = "/change/claude:~1~1sess-abc/structural/delegations/0";
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
        let base = "/change/~0~1notes.md/structural/edits/0";
        let ats = ats(&p);
        assert!(ats.contains(&format!("{base}/old_string")), "{ats:?}");
        assert!(ats.contains(&format!("{base}/new_string")));
        assert!(!ats.iter().any(|a| a.ends_with("/replace_all")));
    }
}
