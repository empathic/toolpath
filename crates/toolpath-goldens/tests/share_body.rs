//! What `path share` uploads is the derived document itself (pr/89): `run_pathbase_inner`
//! (crates/path-cli/src/cmd_export.rs) validates `body` with `Graph::from_json` and sends the
//! same `body`. So the share cells that lose a value lose it in Pathbase's reader, not on the way
//! up. This pins the toolpath side: for every committed document, the body `Graph::to_json`
//! writes is the same bytes however many times it is read and written again, and it is the
//! document (same value) the snapshot holds. The cache fast path and a fresh derive upload
//! "byte for byte" the same text only if this holds.

use std::path::PathBuf;

use serde_json::Value;
use toolpath::v1::Graph;

fn snapshots() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("snapshots")
}

#[test]
fn the_upload_body_is_the_derived_document_byte_for_byte() {
    let mut checked = 0;
    for entry in std::fs::read_dir(snapshots()).expect("snapshots") {
        let path = entry.expect("entry").path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Some(set) = name
            .strip_prefix("document@")
            .and_then(|r| r.strip_suffix(".snap"))
        else {
            continue;
        };
        let text = std::fs::read_to_string(&path).unwrap();
        let body_json = text.splitn(3, "---\n").nth(2).expect("snapshot body");
        let graph = Graph::from_json(body_json).unwrap_or_else(|e| panic!("{set}: {e}"));

        // the body `path share` builds from a freshly derived document
        let body = graph.to_json().unwrap_or_else(|e| panic!("{set}: {e}"));
        // validation reads it back; sending is of `body` itself, so it must survive the read
        let again = Graph::from_json(&body)
            .unwrap_or_else(|e| panic!("{set}: body is not a document: {e}"))
            .to_json()
            .unwrap();
        assert_eq!(
            body, again,
            "{set}: the body changes when it is read and written again"
        );

        // and nothing was added or removed on the way: same value as the committed document
        let sent: Value = serde_json::from_str(&body).unwrap();
        let derived: Value = serde_json::from_str(body_json).unwrap();
        assert_eq!(
            sent, derived,
            "{set}: the upload body differs from the derived document"
        );
        checked += 1;
    }
    assert!(
        checked >= 13,
        "expected a document per recording, found {checked}"
    );
}
