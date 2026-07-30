//! What a redacted span is replaced with.

use std::ops::Range;

/// How a detected span is rewritten. No variant except `Partial` emits a
/// substring of the value; `Mask` and `Partial` still publish its length.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transform {
    #[default]
    Marker,
    Remove,
    Hash,
    /// Length-preserving, and therefore publishes the exact length.
    Mask,
    /// Keeps 4 leading and 4 trailing chars: leaks provider and format.
    Partial,
}

/// A short stable handle for a secret value, used to correlate the same
/// secret across occurrences without publishing it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint(pub String);

impl Fingerprint {
    /// Keyed, never a bare hash: a hash of a low-entropy secret is a
    /// dictionary attack away from the secret (EDPB 01/2025 para 88).
    pub fn new(key: &[u8], value: &str) -> Self {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = <Hmac<Sha256>>::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(value.as_bytes());
        Fingerprint(hex(&mac.finalize().into_bytes())[..6].to_string())
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").unwrap();
    }
    s
}

/// Rewrite one value under a chosen `Transform`. `fp` is the value's own
/// fingerprint (callers compute it once per value and reuse it across
/// occurrences, since `Hash`/`Marker` both need it).
pub fn apply_transform(t: Transform, rule: &str, value: &str, fp: &Fingerprint) -> String {
    match t {
        Transform::Marker => format!("[REDACTED:{rule}:{}]", fp.0),
        Transform::Remove => String::new(),
        Transform::Hash => fp.0.clone(),
        Transform::Mask => "\u{2588}".repeat(value.chars().count()),
        Transform::Partial => {
            let n = value.chars().count();
            if n > 10 {
                let head: String = value.chars().take(4).collect();
                let tail: String = value.chars().skip(n - 4).collect();
                format!("{head}\u{2026}{tail}")
            } else {
                "\u{2588}".repeat(n)
            }
        }
    }
}

/// Splice every `(span, replacement)` edit into `text` in one pass.
///
/// Sorted descending by start so earlier offsets stay valid as later spans
/// are spliced out. Detector output reaching this point is already
/// overlap-free (see `detect::normalise`); an edit whose span no longer
/// lands on a valid boundary of the string as spliced so far is dropped
/// rather than risking a panic or corrupt UTF-8, so a normalisation bug
/// upstream degrades safely instead of crashing.
pub fn apply_spans_desc(text: &str, edits: &mut [(Range<usize>, String)]) -> String {
    edits.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));
    let mut out = text.to_string();
    for (span, repl) in edits.iter() {
        let lands_cleanly = span.start <= span.end
            && span.end <= out.len()
            && out.is_char_boundary(span.start)
            && out.is_char_boundary(span.end);
        if lands_cleanly {
            out.replace_range(span.clone(), repl);
        }
    }
    out
}

/// Precedence for which `Transform` applies to a finding: a per-finding
/// choice beats a per-rule default, which beats the global default.
pub fn resolve_transform(
    cfg: &crate::RedactConfig,
    rule: &str,
    per_finding: Option<Transform>,
) -> Transform {
    per_finding
        .or_else(|| {
            cfg.mode_for
                .iter()
                .find(|(r, _)| r == rule)
                .map(|(_, t)| *t)
        })
        .unwrap_or(cfg.mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fp() -> Fingerprint {
        Fingerprint::new(b"test-key", "fixture-value")
    }

    fn cfg_with(mode: Transform, mode_for: Vec<(String, Transform)>) -> crate::RedactConfig {
        crate::RedactConfig {
            threshold: 0.8,
            mode,
            mode_for,
            key: b"test-key".to_vec(),
            now: chrono::Utc.with_ymd_and_hms(2026, 7, 30, 0, 0, 0).unwrap(),
            drop_signatures: false,
            reveal: false,
        }
    }

    fn resolve(cfg: &crate::RedactConfig, rule: &str, per_finding: Option<Transform>) -> Transform {
        resolve_transform(cfg, rule, per_finding)
    }

    fn apply_spans(text: &str, spans: &[Range<usize>]) -> String {
        let mut edits: Vec<_> = spans.iter().cloned().map(|s| (s, String::new())).collect();
        apply_spans_desc(text, &mut edits)
    }

    fn apply_one_by_one_from_right(text: &str, spans: &[Range<usize>]) -> String {
        let mut sorted = spans.to_vec();
        sorted.sort_by_key(|s| std::cmp::Reverse(s.start));
        let mut out = text.to_string();
        for span in sorted {
            out.replace_range(span, "");
        }
        out
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let k = b"test-key";
        assert_eq!(Fingerprint::new(k, "abc"), Fingerprint::new(k, "abc"));
        assert_ne!(
            Fingerprint::new(k, "abc"),
            Fingerprint::new(b"other", "abc")
        );
    }

    #[test]
    fn mask_preserves_character_count_not_byte_count() {
        let out = apply_transform(Transform::Mask, "rule", "héllo", &fp());
        assert_eq!(out.chars().count(), 5);
    }

    #[test]
    fn partial_falls_back_to_mask_below_the_floor() {
        let out = apply_transform(Transform::Partial, "rule", "short", &fp());
        assert!(!out.contains("short"));
        assert_eq!(out.chars().count(), 5);
    }

    #[test]
    fn only_partial_ever_emits_a_substring_of_its_input() {
        let value = "AKIAIOSFODNN7REALKEY";
        for t in [
            Transform::Marker,
            Transform::Remove,
            Transform::Hash,
            Transform::Mask,
        ] {
            let out = apply_transform(t, "aws-access-key-id", value, &fp());
            for w in 4..=value.len() {
                for s in value.as_bytes().windows(w) {
                    let sub = std::str::from_utf8(s).unwrap();
                    assert!(!out.contains(sub), "{t:?} leaked {sub:?}");
                }
            }
        }
    }

    // `Hash`'s output alphabet (0-9a-f) overlaps the alphabet of
    // "AKIAIOSFODNN7REALKEY" above, so the check there is astronomically
    // unlikely to false-positive but not provably impossible. Pin the
    // impossible case: a value with no hex characters at all cannot share
    // a substring with a hex string, by construction.
    #[test]
    fn hash_output_cannot_share_a_substring_with_a_non_hex_value() {
        let value = "ZZZZ-QQQQ-WWWW-XXXX-YYYY-KKKK-JJJJ-VVVV";
        assert!(!value.chars().any(|c| c.is_ascii_hexdigit()));
        let out = apply_transform(Transform::Hash, "rule", value, &fp());
        assert!(out.chars().all(|c| c.is_ascii_hexdigit()));
        for w in 4..=value.len() {
            for s in value.as_bytes().windows(w) {
                let sub = std::str::from_utf8(s).unwrap();
                assert!(!out.contains(sub));
            }
        }
    }

    #[test]
    fn right_to_left_application_matches_one_at_a_time() {
        let text = "aaa BBB ccc DDD eee";
        let spans = vec![4..7, 12..15];
        assert_eq!(
            apply_spans(text, &spans),
            apply_one_by_one_from_right(text, &spans)
        );
    }

    #[test]
    fn per_rule_override_beats_global_and_per_finding_beats_both() {
        let cfg = cfg_with(Transform::Marker, vec![("us-ssn".into(), Transform::Mask)]);
        assert_eq!(resolve(&cfg, "us-ssn", None), Transform::Mask);
        assert_eq!(resolve(&cfg, "aws-access-key-id", None), Transform::Marker);
        assert_eq!(
            resolve(&cfg, "us-ssn", Some(Transform::Remove)),
            Transform::Remove
        );
    }

    #[test]
    fn fingerprint_accepts_an_empty_key() {
        let out = Fingerprint::new(b"", "abc");
        assert_eq!(out.0.len(), 6);
    }

    #[test]
    fn fingerprint_accepts_a_key_longer_than_the_sha256_block_size() {
        let long_key = vec![0x42u8; 100]; // SHA-256's block size is 64 bytes.
        let out = Fingerprint::new(&long_key, "abc");
        assert_eq!(out.0.len(), 6);
    }

    #[test]
    fn remove_then_apply_spans_desc_two_adjacent_spans() {
        let text = "aaaBBBCCCeee";
        let mut edits = vec![
            (3..6, apply_transform(Transform::Remove, "r", "BBB", &fp())),
            (6..9, apply_transform(Transform::Remove, "r", "CCC", &fp())),
        ];
        assert_eq!(apply_spans_desc(text, &mut edits), "aaaeee");
    }

    #[test]
    fn remove_then_apply_spans_desc_two_overlapping_spans_drops_the_invalidated_one() {
        // Once the rightmost span (3..8) is spliced, the leftmost span's
        // recorded end (5) no longer lands inside the shortened string, so
        // it is dropped instead of panicking or corrupting the result.
        let text = "abcdefgh";
        let mut edits = vec![
            (
                3..8,
                apply_transform(Transform::Remove, "r", "defgh", &fp()),
            ),
            (
                0..5,
                apply_transform(Transform::Remove, "r", "abcde", &fp()),
            ),
        ];
        let out = apply_spans_desc(text, &mut edits);
        assert_eq!(out, "abc");
    }

    #[test]
    fn apply_spans_desc_multibyte_text_stays_on_char_boundaries() {
        let text = "héllo wörld";
        let e = text.find('é').unwrap();
        let o = text.find('ö').unwrap();
        let mut edits = vec![
            (o..o + 'ö'.len_utf8(), "O".to_string()),
            (e..e + 'é'.len_utf8(), "E".to_string()),
        ];
        assert_eq!(apply_spans_desc(text, &mut edits), "hEllo wOrld");
    }

    #[test]
    fn mask_of_empty_value_is_empty() {
        assert_eq!(apply_transform(Transform::Mask, "rule", "", &fp()), "");
    }

    #[test]
    fn mask_of_single_char_value_is_one_block() {
        assert_eq!(
            apply_transform(Transform::Mask, "rule", "x", &fp()),
            "\u{2588}"
        );
    }

    #[test]
    fn partial_at_exactly_ten_chars_falls_back_to_mask() {
        let out = apply_transform(Transform::Partial, "rule", "0123456789", &fp());
        assert_eq!(out, "\u{2588}".repeat(10));
    }

    #[test]
    fn partial_at_eleven_chars_shows_head_and_tail() {
        let out = apply_transform(Transform::Partial, "rule", "01234567890", &fp());
        assert_eq!(out, "0123\u{2026}7890");
    }
}
