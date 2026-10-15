//! The stable form of a projection that differs between identical runs.
//! `path p export copilot` mints a random session id and emits keys in
//! unstable order, so its golden is the canonical form: keys sorted, the
//! session id masked.

use serde_json::Value;

/// Keys sorted on every line, and the Copilot session id (the first
/// `data.sessionId`) replaced by `<session-id>` wherever it occurs. Lines
/// that are not JSON pass through. Every line ends in a newline.
pub fn canonicalize(text: &str) -> String {
    let id = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| v["data"]["sessionId"].as_str().map(str::to_owned));
    let masked = match id {
        Some(id) => text.replace(&id, "<session-id>"),
        None => text.to_owned(),
    };
    masked
        .lines()
        .map(|l| match serde_json::from_str::<Value>(l) {
            Ok(v) => serde_json::to_string(&sorted(v)).unwrap_or_default() + "\n",
            Err(_) => l.to_owned() + "\n",
        })
        .collect()
}

fn sorted(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut kv: Vec<_> = m.into_iter().collect();
            kv.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(kv.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Value::Array(a) => Value::Array(a.into_iter().map(sorted).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_sorted_and_the_session_id_is_masked_everywhere() {
        let raw = "{\"z\":1,\"data\":{\"sessionId\":\"abc-1\"},\"a\":\"abc-1\"}";
        assert_eq!(
            canonicalize(raw),
            "{\"a\":\"<session-id>\",\"data\":{\"sessionId\":\"<session-id>\"},\"z\":1}\n"
        );
    }

    #[test]
    fn two_runs_that_differ_only_by_id_and_order_canonicalize_alike() {
        let a = "{\"type\":\"session.start\",\"data\":{\"sessionId\":\"1111\",\"v\":1}}\n{\"type\":\"user.message\",\"data\":{\"content\":\"hi\"},\"id\":\"1111\"}\n";
        let b = "{\"data\":{\"v\":1,\"sessionId\":\"2222\"},\"type\":\"session.start\"}\n{\"id\":\"2222\",\"type\":\"user.message\",\"data\":{\"content\":\"hi\"}}\n";
        assert_eq!(canonicalize(a), canonicalize(b));
    }

    #[test]
    fn non_json_lines_pass_through() {
        assert_eq!(
            canonicalize("not json\n{\"b\":1,\"a\":2}"),
            "not json\n{\"a\":2,\"b\":1}\n"
        );
    }
}
