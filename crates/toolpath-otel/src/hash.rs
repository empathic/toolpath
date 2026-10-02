//! sha256 helpers for chained content ids and derived session ids.
//!
//! Changing a salt, a separator, or the input order re-keys every id ever
//! imported; the known-answer tests pin the exact outputs.

use serde::Serialize;
use sha2::{Digest, Sha256};

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// Lowercase hex, two digits per byte, no separators.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)].into());
        s.push(DIGITS[usize::from(b & 0xf)].into());
    }
    s
}

/// Full sha256 of the concatenated parts, as 64 lowercase hex chars.
pub fn sha256_hex(parts: &[&[u8]]) -> String {
    hex(&sha256(parts))
}

/// Chain seed for a session: `id_{-1}`.
pub fn root_id(session_key: &str) -> String {
    hex(&sha256(&[b"toolpath-otel/v1\0", session_key.as_bytes()])[..8])
}

/// `id_i = sha256(id_{i-1} ‖ canonical(norm(msg_i)))`, truncated to 16 hex.
///
/// `prev` is hashed as its 16-char lowercase hex *text* (its UTF-8 bytes),
/// not as the 8 raw bytes it encodes, with no separator before `canonical`.
pub fn chain_id(prev: &str, canonical: &[u8]) -> String {
    hex(&sha256(&[prev.as_bytes(), canonical])[..8])
}

/// The session id the otel view is projected under: deterministic, and never
/// the harness's own id, so a resume never lands on the harness transcript.
pub fn derived_session_id(session_key: &str) -> String {
    let d = sha256(&[b"toolpath-otel/session\0", session_key.as_bytes()]);
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[..16]);
    b[6] = (b[6] & 0x0f) | 0x80; // version 8 (custom)
    b[8] = (b[8] & 0x3f) | 0x80; // RFC 9562 variant
    let h = hex(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// The RFC 8785 (JCS) form of `v`: the one canonical serialization every
/// content hash, chained turn id and digest in this crate is defined on.
///
/// JCS writes a number as its IEEE 754 double, so `1` and `1.0`, `0` and
/// `-0.0`, and integers past 2^53 that round to one double
/// (`9007199254740993` and `9007199254740992`) canonicalize alike. The
/// crate assumes serde_json without its `arbitrary_precision` feature: with
/// it enabled anywhere in a build (features unify), a number no double
/// holds (`1e400`) parses from client input and this panics, and number
/// spellings carried verbatim into step content change.
pub fn canonical_json<T: Serialize>(v: &T) -> String {
    serde_json_canonicalizer::to_string(v).expect("JSON value serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase_two_digits_per_byte() {
        assert_eq!(hex(&[0x00, 0x0f, 0xab]), "000fab");
    }

    #[test]
    fn root_and_chain_ids_are_16_hex_and_deterministic() {
        let root = root_id("177b923f-8cf6-42fc-9f30-9a7b86236265");
        assert_eq!(root.len(), 16);
        assert_eq!(root, root_id("177b923f-8cf6-42fc-9f30-9a7b86236265"));
        assert_ne!(root, root_id("other"));
        let a = chain_id(&root, br#"{"role":"user","text":"hi"}"#);
        assert_eq!(a.len(), 16);
        assert_ne!(a, chain_id(&root, br#"{"role":"user","text":"ho"}"#));
    }

    #[test]
    fn derived_session_id_is_a_v8_uuid_distinct_from_the_key() {
        let key = "177b923f-8cf6-42fc-9f30-9a7b86236265";
        let id = derived_session_id(key);
        assert_eq!(id.len(), 36);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('8'), "version nibble: {id}");
        assert!(
            matches!(&parts[3][..1], "8" | "9" | "a" | "b"),
            "variant: {id}"
        );
        assert_ne!(id, key);
        assert_eq!(id, derived_session_id(key));
    }

    // Known answers computed independently with python3 hashlib/json.
    const KEY: &str = "177b923f-8cf6-42fc-9f30-9a7b86236265";

    #[test]
    fn root_id_known_answer() {
        // printf 'toolpath-otel/v1\0%s' KEY | shasum -a 256 | cut -c1-16
        assert_eq!(root_id(KEY), "eb06bce58435653d");
    }

    #[test]
    fn chain_id_known_answer_hashes_prev_as_hex_text() {
        // sha256(b"eb06bce58435653d" + canonical)[..16]
        assert_eq!(
            chain_id("eb06bce58435653d", br#"{"role":"user","text":"hi"}"#),
            "98469b84e469e2e7"
        );
    }

    #[test]
    fn derived_session_id_known_answer() {
        assert_eq!(
            derived_session_id(KEY),
            "218c08e0-b5d5-8a6a-8b60-a48793ec6a2b"
        );
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let v = serde_json::json!({"b": 1, "a": [{"d": 2, "c": "x"}], "e": null});
        assert_eq!(
            canonical_json(&v),
            r#"{"a":[{"c":"x","d":2}],"b":1,"e":null}"#
        );
    }

    #[test]
    fn canonical_json_is_jcs() {
        // RFC 8785 §3.2.3 orders keys by UTF-16 code unit; §3.2.2.3 numbers.
        let v = serde_json::json!({"\u{fb01}": 1, "\u{1f600}": 2, "n": [1.0, 1e21, 0.5e-6]});
        assert_eq!(
            canonical_json(&v),
            "{\"n\":[1,1e+21,5e-7],\"\u{1f600}\":2,\"\u{fb01}\":1}"
        );
    }
}
