//! OpenRouter Privacy Mode spans. With a `session.id`, the id is the
//! session key (docs/agents/formats/otel.md, Sessions and ids), so a
//! request whose prompt was withheld is a prompt-absent skeleton in the same
//! session and chains from the previous generation's completion
//! (otel.md, Skeletons). Without one, each withheld request is a delta and
//! forms its own Layer T session.

use crate::tests::otel::{ProfileSelection, group_sessions, read_deliveries, stitch};
use serde_json::{Value, json};

fn root(trace: &str, id: &str, start: u64, session: Option<&str>, prompt: Option<&str>) -> Value {
    let mut attrs = vec![
        json!({"key": "gen_ai.response.id", "value": {"stringValue": id}}),
        json!({"key": "gen_ai.completion", "value": {"stringValue": "{\"completion\":\"ok\"}"}}),
        json!({"key": "trace.metadata.openrouter.api_key_name", "value": {"stringValue": "k"}}),
    ];
    if let Some(s) = session {
        attrs.push(json!({"key": "session.id", "value": {"stringValue": s}}));
    }
    if let Some(p) = prompt {
        attrs.push(json!({"key": "gen_ai.prompt", "value": {"stringValue": p}}));
    }
    json!({"resourceSpans": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "openrouter"}}]},
        "scopeSpans": [{"scope": {"name": "openrouter"}, "spans": [{"traceId": trace, "spanId": "0000000000000001",
        "name": "LLM Generation", "kind": 3, "startTimeUnixNano": start.to_string(),
        "endTimeUnixNano": (start + 1).to_string(), "attributes": attrs, "status": {"code": 1}}]}]}]})
}

#[test]
fn sessionless_privacy_mode_requests_form_their_own_trace_sessions() {
    let d = [
        root("t1", "gen-a", 1, None, None),
        root("t2", "gen-b", 2, None, None),
    ];
    let out = read_deliveries(&d, ProfileSelection::Auto).unwrap();
    let sessions = group_sessions(out.generations);
    let keys: Vec<&str> = sessions.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            crate::session::trace_key(Some("k"), "t1").as_str(),
            crate::session::trace_key(Some("k"), "t2").as_str(),
        ]
    );
}

#[test]
fn sessionless_privacy_mode_bodies_group_into_one_trace_session_each() {
    let d = [
        root("t1", "gen-a", 1, None, None),
        root("t2", "gen-b", 2, None, None),
    ];
    let grouped = crate::group::group_sessions(&d, ProfileSelection::Auto)
        .unwrap()
        .output;
    let keys: Vec<&str> = grouped.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            crate::session::trace_key(Some("k"), "t1").as_str(),
            crate::session::trace_key(Some("k"), "t2").as_str(),
        ]
    );
    for (s, body) in grouped.iter().zip(&d) {
        assert_eq!(s.session_id, None);
        assert_eq!(s.requests, std::slice::from_ref(body));
        let p = crate::group::derive_session(s, &Default::default())
            .unwrap()
            .output;
        assert_eq!(
            p.meta.unwrap().extra["otel"]["derived_session_id"],
            s.derived_session_id()
        );
    }
}

#[test]
fn privacy_mode_with_session_id_chains_from_the_previous_generation() {
    let full = "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}";
    let d = [
        root("t1", "gen-a", 1, Some("s"), Some(full)),
        root("t2", "gen-b", 2, Some("s"), None),
    ];
    let out = read_deliveries(&d, ProfileSelection::Auto).unwrap();
    let sessions = group_sessions(out.generations);
    assert_eq!(sessions.len(), 1);
    let g = stitch(&sessions[0]);
    let skeleton = g.nodes.iter().find(|n| n.producer == Some(1)).unwrap();
    assert_eq!(
        skeleton.parent.as_deref(),
        Some(g.links[0].completion.as_str())
    );
}
