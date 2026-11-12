//! The opencode database read, pinned. The `opencode` set reads
//! `test-fixtures/opencode/convo.json` (an `opencode export`) through the
//! adapter's types; nothing else in the goldens opens a database the way
//! `path` does on a real machine. Here the same session is stored as opencode
//! 1.18.18 stores it, in `opencode-stable.db` under the data dir (the
//! channel-named file, `test-fixtures/opencode/schema-1.18.18.sql`), read back
//! with `ConvoIO` through the resolver's channel rule, and compared to the
//! export read.
//!
//! `snapshots/opencode_db@opencode.snap` holds what the database read finds
//! (the file it resolved, the rows it loaded) and every IR key path whose
//! value differs from the export read. An empty difference means the two
//! reads agree; a new entry is a change in the database read.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value, json};
use toolpath_goldens::model::Set;
use toolpath_goldens::pipeline::run;
use toolpath_opencode::{ConvoIO, PathResolver};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test-fixtures/opencode/convo.json"
);
const SCHEMA: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test-fixtures/opencode/schema-1.18.18.sql"
);

#[test]
fn opencode_database_read_matches_the_export_read() {
    let home = tempfile::tempdir().expect("temp home");
    let data = home.path().join(".local/share/opencode");
    std::fs::create_dir_all(&data).expect("data dir");

    let export: Value =
        serde_json::from_slice(&std::fs::read(FIXTURE).expect("fixture")).expect("fixture JSON");
    let session_id = store(&data.join("opencode-stable.db"), &export);

    // The resolver as `path` builds it, minus the environment: the channel
    // rule picks the file.
    let resolver = PathResolver::new()
        .with_home(home.path())
        .with_data_dir(&data)
        .with_db_file(None::<&Path>)
        .with_channel_db(true);
    let io = ConvoIO::with_resolver(resolver.clone());
    let db_file = io.db_path().expect("db path");
    let session = io.read_session(&session_id).expect("read the session row");
    let from_db = serde_json::to_value(toolpath_opencode::provider::to_view_with_resolver(
        &session, &resolver,
    ))
    .expect("IR serialises");

    let set = Set::from_input_file(Path::new(FIXTURE)).expect("set");
    let scratch = tempfile::tempdir().expect("scratch");
    let from_export =
        serde_json::to_value(run(&set, scratch.path()).expect("pipeline").ir).expect("IR");

    let mut differs = BTreeMap::new();
    diff("", &from_export, &from_db, &mut differs);

    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("../snapshots");
    settings.set_prepend_module_to_snapshot(false);
    settings.set_snapshot_suffix("opencode");
    settings.set_sort_maps(true);
    settings.bind(|| {
        insta::assert_json_snapshot!(
            "opencode_db",
            json!({
                "resolved_file": db_file.file_name().and_then(|n| n.to_str()),
                "messages": session.messages.len(),
                "parts": session.messages.iter().map(|m| m.parts.len()).sum::<usize>(),
                "ir_differs_from_export": differs,
            })
        );
    });
}

/// Store an `opencode export` as opencode does: the session's columns, and
/// each message's and part's row with its ids in columns and the rest of its
/// JSON in `data`. Returns the session id.
fn store(db: &Path, export: &Value) -> String {
    let conn = rusqlite::Connection::open(db).expect("create db");
    conn.execute_batch(&std::fs::read_to_string(SCHEMA).expect("schema"))
        .expect("apply schema");

    let info = &export["info"];
    let s = |k: &str| info[k].as_str().map(str::to_owned);
    let created = info["time"]["created"].as_i64().unwrap_or(0);
    let updated = info["time"]["updated"].as_i64().unwrap_or(created);
    let session_id = s("id").expect("session id");
    let project_id = s("projectID").expect("project id");
    let directory = s("directory").unwrap_or_else(|| "/".to_owned());
    conn.execute(
        "INSERT INTO project (id, worktree, vcs, time_created, time_updated, sandboxes)
         VALUES (?1, ?2, 'git', ?3, ?3, '[]')",
        rusqlite::params![project_id, directory, created],
    )
    .expect("project row");
    conn.execute(
        "INSERT INTO session (id, project_id, slug, directory, path, title, version,
                              summary_additions, summary_deletions, summary_files,
                              permission, time_created, time_updated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        rusqlite::params![
            session_id,
            project_id,
            s("slug").unwrap_or_default(),
            directory,
            s("path"),
            s("title").unwrap_or_default(),
            s("version").unwrap_or_default(),
            info["summary"]["additions"].as_i64(),
            info["summary"]["deletions"].as_i64(),
            info["summary"]["files"].as_i64(),
            info.get("permission").map(Value::to_string),
            created,
            updated,
        ],
    )
    .expect("session row");

    for m in export["messages"].as_array().expect("messages") {
        let mut row = m["info"].as_object().cloned().expect("message info");
        let id = take(&mut row, "id");
        take(&mut row, "sessionID");
        let created = row["time"]["created"].as_i64().unwrap_or(0);
        let updated = row["time"]["completed"].as_i64().unwrap_or(created);
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                id,
                session_id,
                created,
                updated,
                Value::Object(row).to_string()
            ],
        )
        .expect("message row");
        for p in m["parts"].as_array().into_iter().flatten() {
            let mut part = p.as_object().cloned().expect("part");
            let part_id = take(&mut part, "id");
            take(&mut part, "messageID");
            take(&mut part, "sessionID");
            conn.execute(
                "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    part_id,
                    id,
                    session_id,
                    created,
                    created,
                    Value::Object(part).to_string()
                ],
            )
            .expect("part row");
        }
    }
    session_id
}

fn take(row: &mut Map<String, Value>, key: &str) -> String {
    row.remove(key)
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Every key path at which `b` differs from `a`, with both values (leaves
/// only; arrays by index).
fn diff(at: &str, a: &Value, b: &Value, out: &mut BTreeMap<String, Value>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                let null = Value::Null;
                diff(
                    &format!("{at}/{k}"),
                    x.get(k).unwrap_or(&null),
                    y.get(k).unwrap_or(&null),
                    out,
                );
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (u, v)) in x.iter().zip(y).enumerate() {
                diff(&format!("{at}/{i}"), u, v, out);
            }
        }
        _ if a != b => {
            out.insert(at.to_owned(), json!({ "export": a, "database": b }));
        }
        _ => {}
    }
}
