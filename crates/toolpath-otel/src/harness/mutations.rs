//! File mutations from write/edit/patch tool calls, with inputs
//! canonicalized onto Claude's key names before `file_write_diff`.

use serde_json::{Map, Value, json};
use toolpath_convo::shell_writes::parse_patch;
use toolpath_convo::{FileMutation, ToolCategory, ToolInvocation, file_write_diff};

/// Includes the keys convo's fallback reads, so the fallback fires only for
/// a call [`fallback_path`] leaves to it. Claude NotebookEdit `notebook_path`.
const PATH_KEYS: [&str; 6] = [
    "file_path",
    "path",
    "filePath",
    "filename",
    "file",
    "notebook_path",
];
/// The keys convo's fallback reads, in its order.
const FALLBACK_PATH_KEYS: [&str; 4] = ["file_path", "path", "filename", "file"];
/// Claude `old_string`, opencode `oldString`, pi `oldText`.
const OLD_KEYS: [&str; 3] = ["old_string", "oldString", "oldText"];
const NEW_KEYS: [&str; 3] = ["new_string", "newString", "newText"];
/// Codex `apply_patch` `{input}` (or a bare string), opencode `{patchText}`.
const PATCH_KEYS: [&str; 3] = ["input", "patch", "patchText"];

fn str_field<'v>(v: &'v Value, keys: &[&str]) -> Option<&'v str> {
    keys.iter().find_map(|k| v.get(*k)?.as_str())
}

/// A write/edit input rewritten onto Claude's keys: `file_path`,
/// `old_string`, `new_string`, `content`, and `edits` as
/// `[{old_string, new_string}]` (pi's `edits: [{oldText, newText}]`).
pub fn canonical_input(input: &Value) -> Value {
    let mut m = Map::new();
    if let Some(p) = str_field(input, &PATH_KEYS) {
        m.insert("file_path".into(), json!(p));
    }
    if let Some(o) = str_field(input, &OLD_KEYS) {
        m.insert("old_string".into(), json!(o));
    }
    if let Some(n) = str_field(input, &NEW_KEYS) {
        m.insert("new_string".into(), json!(n));
    }
    if let Some(c) = str_field(input, &["content"]) {
        m.insert("content".into(), json!(c));
    }
    if let Some(edits) = input.get("edits").and_then(Value::as_array) {
        let edits: Vec<Value> = edits
            .iter()
            .map(|e| {
                json!({
                    "old_string": str_field(e, &OLD_KEYS).unwrap_or(""),
                    "new_string": str_field(e, &NEW_KEYS).unwrap_or(""),
                })
            })
            .collect();
        m.insert("edits".into(), Value::Array(edits));
    }
    Value::Object(m)
}

/// The file mutations one tool call makes, each with `tool_id` set.
/// Non-FileWrite tools, and calls [`fallback_path`] leaves to convo's
/// fallback, make none.
pub fn file_mutations(tool: &ToolInvocation) -> Vec<FileMutation> {
    if tool.category != Some(ToolCategory::FileWrite) {
        return Vec::new();
    }
    // A marker-less patch parses to nothing; still record the file.
    let muts = match patch_text(tool).map(patch_mutations) {
        Some(muts) if !muts.is_empty() => muts,
        _ if fallback_path(tool).is_some() => Vec::new(),
        _ => write_edit(tool).into_iter().collect(),
    };
    muts.into_iter()
        .map(|m| FileMutation {
            tool_id: Some(tool.id.clone()),
            ..m
        })
        .collect()
}

fn patch_text(tool: &ToolInvocation) -> Option<&str> {
    if !matches!(tool.name.as_str(), "apply_patch" | "patch") {
        return None;
    }
    match &tool.input {
        Value::String(s) => Some(s),
        v => str_field(v, &PATCH_KEYS),
    }
}

/// The path of a MultiEdit-shaped (`edits`) FileWrite call that convo's
/// fallback reads exactly as [`write_edit`] would. Such a call is left to
/// the fallback, which also records the structural `edits` array a
/// `FileMutation` cannot carry.
pub fn fallback_path(tool: &ToolInvocation) -> Option<&str> {
    if tool.category != Some(ToolCategory::FileWrite)
        || patch_text(tool).is_some_and(|p| !parse_patch(p).is_empty())
    {
        return None;
    }
    let input = &tool.input;
    input.get("edits")?.as_array()?;
    let path = str_field(input, &FALLBACK_PATH_KEYS)?;
    let canonical = canonical_input(input);
    let get = |k: &str| canonical.get(k).and_then(Value::as_str);
    let edits_only = (get("old_string").is_none() || get("new_string").is_none())
        && get("content").is_none()
        && str_field(input, &["new_source"]).is_none();
    let same = str_field(input, &PATH_KEYS) == Some(path)
        && file_write_diff(&tool.name, input, path, None)
            == file_write_diff(&tool.name, &canonical, path, None);
    (edits_only && same).then_some(path)
}

/// Write → `after = content`; Edit → `before = old`, `after = new`;
/// NotebookEdit → `after = new_source`; MultiEdit-shaped (`edits`) →
/// `raw_diff` only; a path with nothing to write → a path-only mutation.
/// opencode `delete` (and `rm`) → operation `delete`, as `toolpath-opencode`.
fn write_edit(tool: &ToolInvocation) -> Option<FileMutation> {
    let path = str_field(&tool.input, &PATH_KEYS)?;
    let canonical = canonical_input(&tool.input);
    let get = |k: &str| canonical.get(k).and_then(Value::as_str);
    let (before, after) = match (get("old_string"), get("new_string")) {
        (Some(old), Some(new)) => (Some(old), Some(new)),
        _ => (
            None,
            get("content").or_else(|| str_field(&tool.input, &["new_source"])),
        ),
    };
    let operation = (tool.name == "delete").then(|| "delete".to_string());
    Some(FileMutation {
        path: path.to_string(),
        raw_diff: file_write_diff(&tool.name, &canonical, path, None),
        before: before.map(str::to_string),
        after: after.map(str::to_string),
        operation,
        ..Default::default()
    })
}

/// Codex/opencode `apply_patch` (V4A) → one mutation per file, read by
/// [`toolpath_convo::shell_writes::parse_patch`]. Added files carry their
/// full content in `after`; updates carry no diff (V4A hunks are not
/// unified diffs).
pub fn patch_mutations(patch: &str) -> Vec<FileMutation> {
    parse_patch(patch)
        .into_iter()
        .map(|f| FileMutation {
            path: f.path,
            operation: Some(f.op.as_str().to_string()),
            rename_to: f.move_to,
            after: f.added,
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::classifier::provider_tool_category;
    use toolpath_convo::{ConversationView, DeriveConfig, Role, Turn, derive_path, unified_diff};

    fn tool(id: &str, name: &str, input: Value) -> ToolInvocation {
        ToolInvocation {
            id: id.into(),
            name: name.into(),
            input,
            result: None,
            category: provider_tool_category("unknown", name),
        }
    }

    fn view_of(
        tool_uses: Vec<ToolInvocation>,
        file_mutations: Vec<FileMutation>,
    ) -> ConversationView {
        let turn = Turn {
            id: "t1".into(),
            parent_id: None,
            group_id: None,
            role: Role::Assistant,
            timestamp: "2026-09-28T00:00:00.000Z".into(),
            text: String::new(),
            thinking: None,
            tool_uses,
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: Vec::new(),
            file_mutations,
        };
        ConversationView {
            id: "v".into(),
            turns: vec![turn],
            provider_id: Some("otel".into()),
            ..Default::default()
        }
    }

    #[test]
    fn write_edit_and_multi_edit_equal_the_convo_fallback() {
        let tools = vec![
            tool(
                "w",
                "Write",
                json!({"file_path": "src/a.rs", "content": "fn a() {}\n"}),
            ),
            tool(
                "e",
                "Edit",
                json!({"file_path": "/abs/b.rs", "old_string": "x\n", "new_string": "y\n"}),
            ),
            tool(
                "m",
                "MultiEdit",
                json!({"file_path": "c.rs", "edits": [{"old_string": "a", "new_string": "b"}]}),
            ),
        ];
        let muts: Vec<FileMutation> = tools.iter().flat_map(file_mutations).collect();
        assert_eq!(muts.len(), 2, "MultiEdit is left to the fallback");
        let config = DeriveConfig::default();
        let ours = derive_path(&view_of(tools.clone(), muts), &config);
        let fallback = derive_path(&view_of(tools, Vec::new()), &config);
        let ours = serde_json::to_value(&ours.steps).unwrap();
        assert_eq!(ours, serde_json::to_value(&fallback.steps).unwrap());
        let multi = &ours[0]["change"]["c.rs"];
        assert_eq!(
            multi["structural"]["edits"],
            json!([{"old_string": "a", "new_string": "b"}])
        );
        assert!(multi["raw"].is_string());
    }

    #[test]
    fn multi_edit_is_left_to_the_fallback_only_when_it_reads_the_same() {
        let claude =
            json!({"file_path": "c.rs", "edits": [{"old_string": "a", "new_string": "b"}]});
        let t = tool("m", "MultiEdit", claude);
        assert_eq!(fallback_path(&t), Some("c.rs"));
        assert!(file_mutations(&t).is_empty());
        // The fallback reads neither `filePath` nor pi's `oldText`/`newText`.
        for input in [
            json!({"filePath": "c.rs", "edits": [{"old_string": "a", "new_string": "b"}]}),
            json!({"path": "c.rs", "edits": [{"oldText": "a", "newText": "b"}]}),
        ] {
            let t = tool("m", "edit", input.clone());
            assert_eq!(fallback_path(&t), None, "{input}");
            let m = file_mutations(&t);
            assert_eq!(m.len(), 1, "{input}");
            assert!(m[0].raw_diff.is_some(), "{input}");
            assert_eq!(
                (m[0].before.as_deref(), m[0].after.as_deref()),
                (None, None)
            );
        }
        assert_eq!(
            fallback_path(&tool(
                "r",
                "read",
                json!({"file_path": "c.rs", "edits": []})
            )),
            None
        );
    }

    #[test]
    fn notebook_edit_records_the_notebook_and_its_new_source() {
        let m = file_mutations(&tool(
            "n",
            "NotebookEdit",
            json!({"notebook_path": "/w/a.ipynb", "cell_id": "c1", "new_source": "print(1)\n", "edit_mode": "replace"}),
        ));
        assert_eq!(m.len(), 1);
        assert_eq!(
            (
                m[0].path.as_str(),
                m[0].after.as_deref(),
                m[0].before.as_deref()
            ),
            ("/w/a.ipynb", Some("print(1)\n"), None)
        );
        assert_eq!(m[0].tool_id.as_deref(), Some("n"));
    }

    #[test]
    fn opencode_delete_is_a_delete_operation() {
        let t = ToolInvocation {
            category: provider_tool_category("opencode", "delete"),
            ..tool("d", "delete", json!({"filePath": "/w/gone.txt"}))
        };
        let m = file_mutations(&t);
        assert_eq!(m.len(), 1);
        assert_eq!(
            (m[0].path.as_str(), m[0].operation.as_deref()),
            ("/w/gone.txt", Some("delete"))
        );
        let w = file_mutations(&tool("w", "write", json!({"filePath": "x", "content": ""})));
        assert_eq!(w[0].operation, None);
    }

    #[test]
    fn opencode_and_pi_key_spellings_canonicalize() {
        let oc = file_mutations(&tool(
            "o",
            "edit",
            json!({"filePath": "b.txt", "oldString": "a", "newString": "b", "replaceAll": false}),
        ));
        assert_eq!(
            (
                oc[0].path.as_str(),
                oc[0].before.as_deref(),
                oc[0].after.as_deref()
            ),
            ("b.txt", Some("a"), Some("b"))
        );
        assert_eq!(
            oc[0].raw_diff.as_deref(),
            Some(unified_diff("b.txt", "a", "b").as_str())
        );
        let pi_old = file_mutations(&tool(
            "p",
            "edit",
            json!({"path": "c.txt", "oldText": "x", "newText": "y"}),
        ));
        assert_eq!(
            (pi_old[0].before.as_deref(), pi_old[0].after.as_deref()),
            (Some("x"), Some("y"))
        );
        let pi_new = file_mutations(&tool(
            "q",
            "edit",
            json!({"path": "d.txt", "edits": [{"oldText": "1", "newText": "2"}]}),
        ));
        let canonical =
            json!({"file_path": "d.txt", "edits": [{"old_string": "1", "new_string": "2"}]});
        assert_eq!(
            pi_new[0].raw_diff,
            file_write_diff("edit", &canonical, "d.txt", None)
        );
        let w = file_mutations(&tool(
            "w",
            "write",
            json!({"filePath": "/w/a.txt", "content": "hi\n"}),
        ));
        assert_eq!(
            (w[0].path.as_str(), w[0].after.as_deref()),
            ("/w/a.txt", Some("hi\n"))
        );
    }

    #[test]
    fn a_path_with_nothing_to_write_is_a_path_only_mutation() {
        for input in [
            json!({"filePath": "x"}),
            json!({"filename": "y"}),
            json!({"file": "z"}),
        ] {
            let m = file_mutations(&tool("n", "write", input.clone()));
            assert_eq!(m.len(), 1, "{input}");
            assert!(m[0].raw_diff.is_none() && m[0].after.is_none());
        }
        assert!(file_mutations(&tool("r", "read", json!({"filePath": "x"}))).is_empty());
        assert!(file_mutations(&tool("n", "write", json!({"content": "no path"}))).is_empty());
    }

    #[test]
    fn a_marker_less_patch_falls_back_to_write_edit() {
        let tools = vec![tool(
            "p",
            "patch",
            json!({"file_path": "src/a.rs", "patch": "@@\n-old\n+new\n"}),
        )];
        let muts = file_mutations(&tools[0]);
        assert_eq!(muts.len(), 1);
        assert_eq!(muts[0].path, "src/a.rs");
        assert_eq!(muts[0].tool_id.as_deref(), Some("p"));
        let config = DeriveConfig::default();
        let ours = derive_path(&view_of(tools.clone(), muts), &config);
        let fallback = derive_path(&view_of(tools, Vec::new()), &config);
        assert_eq!(
            serde_json::to_value(&ours.steps).unwrap(),
            serde_json::to_value(&fallback.steps).unwrap()
        );
    }

    /// (path, operation, rename_to, after) of one patch mutation.
    type PatchRow<'a> = (&'a str, Option<&'a str>, Option<&'a str>, Option<&'a str>);

    #[test]
    fn apply_patch_from_codex_and_opencode() {
        let patch = "*** Begin Patch\n*** Add File: a.py\n+x = 1\n+y = 2\n*** Update File: b.py\n*** Move to: c.py\n@@\n-old\n+new\n*** Delete File: d.py\n*** End Patch";
        for input in [
            json!(patch),
            json!({"input": patch}),
            json!({"patchText": patch}),
        ] {
            let m = file_mutations(&tool("p", "apply_patch", input));
            let got: Vec<PatchRow> = m
                .iter()
                .map(|x| {
                    (
                        x.path.as_str(),
                        x.operation.as_deref(),
                        x.rename_to.as_deref(),
                        x.after.as_deref(),
                    )
                })
                .collect();
            assert_eq!(
                got,
                vec![
                    ("a.py", Some("add"), None, Some("x = 1\ny = 2\n")),
                    ("b.py", Some("update"), Some("c.py"), None),
                    ("d.py", Some("delete"), None, None),
                ]
            );
            assert!(m.iter().all(|x| x.tool_id.as_deref() == Some("p")));
        }
    }
}
