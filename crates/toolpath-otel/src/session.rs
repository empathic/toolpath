//! One session's generations and the key its ids are chained from.

use crate::generation::Generation;
use crate::hash::sha256_hex;
use crate::normalize::{NormMessage, is_system_like, kept_prompt};

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// The client session id, else `otel-cluster:<16 hex>` (a full-history
    /// first generation) or `otel-trace:<16 hex>` (a delta one).
    pub key: String,
    pub session_id: Option<String>,
    /// Sorted by `(start_ns, id)`.
    pub generations: Vec<Generation>,
    /// Some generation of this session was skipped as truncated.
    pub truncated: bool,
}

impl Session {
    /// Sorts `generations` by `(start_ns, id)`.
    pub fn new(key: String, session_id: Option<String>, mut generations: Vec<Generation>) -> Self {
        generations.sort_by(|a, b| (a.start_ns, &a.id).cmp(&(b.start_ns, &b.id)));
        Session {
            key,
            session_id,
            generations,
            truncated: false,
        }
    }

    /// All of `generations` as one session, keyed by the first client
    /// session id in start order, else by the first generation; `None`
    /// when empty.
    pub fn from_generations(generations: Vec<Generation>) -> Option<Self> {
        let mut s = Session::new(String::new(), None, generations);
        let first = s.generations.first()?;
        s.session_id = s.generations.iter().find_map(|g| g.session_id.clone());
        s.key = match &s.session_id {
            Some(id) => id.clone(),
            None if first.is_delta() => trace_key(first.client_key.as_deref(), &first.trace_id),
            None => cluster_key(
                first.client_key.as_deref(),
                &kept_prompt(&first.messages),
                &first.id,
            ),
        };
        Some(s)
    }
}

/// `otel-trace:` + the first 16 hex of sha256(client key ‖ `\0` ‖ trace id).
pub fn trace_key(client_key: Option<&str>, trace_id: &str) -> String {
    let h = sha256_hex(&[
        client_key.unwrap_or("").as_bytes(),
        b"\0",
        trace_id.as_bytes(),
    ]);
    format!("otel-trace:{}", &h[..16])
}

/// `otel-cluster:` + hash of (client key, leading system message, first
/// user message, generation id).
pub fn cluster_key(
    client_key: Option<&str>,
    prompt: &[NormMessage],
    generation_id: &str,
) -> String {
    let system = prompt
        .first()
        .filter(|m| is_system_like(&m.role))
        .map_or("", |m| m.text.as_str());
    let user = prompt
        .iter()
        .find(|m| m.role == "user")
        .map_or("", |m| m.text.as_str());
    let h = sha256_hex(&[
        client_key.unwrap_or("").as_bytes(),
        b"\0",
        system.as_bytes(),
        b"\0",
        user.as_bytes(),
        b"\0",
        generation_id.as_bytes(),
    ]);
    format!("otel-cluster:{}", &h[..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::{Absent, Message};
    use serde_json::json;

    fn g(id: &str, start: u64, session_id: Option<&str>) -> Generation {
        Generation {
            id: id.into(),
            trace_id: format!("trace-{id}"),
            start_ns: start,
            client_key: Some("k".into()),
            session_id: session_id.map(str::to_string),
            messages: vec![Message {
                role: "user".into(),
                content: json!("hi"),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn trace_key_known_answers() {
        // python3: "otel-trace:" + sha256(b"k\0t-1").hexdigest()[:16]
        assert_eq!(trace_key(Some("k"), "t-1"), "otel-trace:8462f8d801d1bffd");
        // python3: "otel-trace:" + sha256(b"\0t-1").hexdigest()[:16]
        assert_eq!(trace_key(None, "t-1"), "otel-trace:78828f2e39964aa0");
    }

    #[test]
    fn keys_come_from_the_session_id_else_the_first_generation() {
        let s = Session::from_generations(vec![g("b", 2, Some("s1")), g("a", 1, None)]).unwrap();
        assert_eq!(
            (s.key.as_str(), s.session_id.as_deref()),
            ("s1", Some("s1"))
        );
        assert_eq!(s.generations[0].id, "a");

        let s = Session::from_generations(vec![g("a", 1, None)]).unwrap();
        assert_eq!(
            s.key,
            cluster_key(Some("k"), &kept_prompt(&s.generations[0].messages), "a")
        );

        let mut delta = g("a", 1, None);
        delta.absent = Absent {
            prompt: true,
            completion: false,
        };
        let s = Session::from_generations(vec![delta]).unwrap();
        assert_eq!(s.key, trace_key(Some("k"), "trace-a"));

        assert!(Session::from_generations(Vec::new()).is_none());
    }

    #[test]
    fn same_opening_sessions_without_an_id_get_distinct_keys() {
        let one = Session::from_generations(vec![g("a", 1, None)]).unwrap();
        let other = Session::from_generations(vec![g("b", 1, None)]).unwrap();
        assert_ne!(one.key, other.key);
        let grown =
            Session::from_generations(vec![g("a", 1, None), g("c", 2, None), g("d", 3, None)])
                .unwrap();
        assert_eq!(grown.key, one.key);
    }
}
