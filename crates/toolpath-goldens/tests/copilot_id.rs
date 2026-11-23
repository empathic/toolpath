//! The Copilot session id is deterministic: the same document projects to the same bytes, id
//! included, and going round twice changes nothing. Before this the writer minted
//! `Uuid::new_v4()` on every projection, so no two projections matched and twice-around always
//! differed by id (graph.id, path.id, meta.title).

use toolpath::v1::Graph;
use toolpath_convo::{ConversationView, DeriveConfig, derive_path};
use toolpath_goldens::model::Agent;
use toolpath_goldens::pipeline::{projection, writer_with_document};

const VIEW: &str = include_str!("../fixtures/full-view.json");

fn document() -> String {
    let view: ConversationView =
        serde_json::from_str(VIEW).expect("fixtures/full-view.json is a ConversationView");
    let cfg = DeriveConfig {
        include_thinking: true,
        include_tool_uses: true,
        ..DeriveConfig::default()
    };
    Graph::from_path(derive_path(&view, &cfg))
        .to_json()
        .expect("document serializes")
}

fn session_id(projection: &str) -> String {
    let first = projection.lines().next().expect("a first event");
    let v: serde_json::Value = serde_json::from_str(first).expect("event JSON");
    v["data"]["sessionId"]
        .as_str()
        .expect("session.start carries the sessionId")
        .to_owned()
}

#[test]
fn the_same_document_projects_to_the_same_bytes_id_included() {
    let doc = document();
    let a = projection(&doc, Agent::Copilot).expect("projection");
    let b = projection(&doc, Agent::Copilot).expect("projection");
    assert_eq!(
        session_id(&a),
        session_id(&b),
        "the session id is minted afresh"
    );
    same_lines(&a, &b);
}

/// Equal, and when not, the first differing event named.
fn same_lines(a: &str, b: &str) {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        assert_eq!(x, y, "event {i} differs");
    }
    assert_eq!(a.lines().count(), b.lines().count(), "event counts differ");
}

#[test]
fn copilot_twice_around_is_byte_identical_including_id() {
    let home = tempfile::tempdir().expect("temp HOME");
    let doc = document();
    // Pass 1: the document Copilot's reader derives from the projection of the original.
    let (_, first) = writer_with_document(&doc, Agent::Copilot, home.path()).expect("round trip");
    let first = first.expect("copilot reads its own projection");
    // Pass 2: the same again, from pass 1's document (what twice-around measures).
    let (_, second) = writer_with_document(&first.to_string(), Agent::Copilot, home.path())
        .expect("second round trip");
    let second = second.expect("copilot reads its own projection again");
    assert_eq!(
        first, second,
        "the second pass moved something (graph.id, path.id and meta.title carry the session id)"
    );
    // and the projections of the two documents are the same events, id included
    let p1 = projection(&first.to_string(), Agent::Copilot).expect("projection");
    let p2 = projection(&second.to_string(), Agent::Copilot).expect("projection");
    assert_eq!(session_id(&p1), session_id(&p2));
    same_lines(&p1, &p2);
}

#[test]
fn the_id_is_a_valid_uuid() {
    let id = session_id(&projection(&document(), Agent::Copilot).expect("projection"));
    let u = uuid::Uuid::parse_str(&id).expect("a UUID, which Copilot requires");
    assert_eq!(u.get_version_num(), 5, "derived, not random: {id}");
}
