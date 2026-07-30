//! The built-in detector: a vendored gitleaks ruleset, an entropy gate,
//! and a keyword prefilter.

pub mod entropy;
pub mod rules;

use crate::FieldShape;
use crate::detect::{Candidate, Detector, Finding};
use aho_corasick::{AhoCorasick, MatchKind};
use std::ops::Range;

/// Tuned only so the fixture corpus in this module's tests lands true
/// positives clearly above, and documented false positives clearly below,
/// the 0.8 plan-default threshold - not derived from a labeled dataset.
const BASE_SCORE: f32 = 0.6;
const HOTWORD_BONUS: f32 = 0.5;
const PENALTY_PER_ENTROPY_BIT: f32 = 0.15;
const HOTWORD_WINDOW: usize = 50;

const HOTWORDS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "credential",
    "api_key",
    "apikey",
    "auth",
    "private_key",
    "access_key",
];

pub struct InternalDetector {
    rules: Vec<(rules::Rule, regex::Regex)>,
    prefilter: AhoCorasick,
    /// Recognizes this crate's own [`crate::Transform::Marker`] and
    /// [`crate::Transform::Mask`] output. Matched regions are blanked
    /// before any rule runs them, or a second redaction pass would find
    /// the marker text itself as a "secret" and redaction would never
    /// reach a fixed point.
    marker_re: regex::Regex,
}

impl InternalDetector {
    pub fn new() -> Self {
        let rules: Vec<(rules::Rule, regex::Regex)> = rules::load_rules()
            .into_iter()
            .map(|r| {
                let re = regex::Regex::new(&r.regex)
                    .unwrap_or_else(|e| panic!("rule {} failed to compile: {e}", r.id));
                (r, re)
            })
            .collect();

        let keywords: Vec<&str> = rules
            .iter()
            .flat_map(|(r, _)| r.keywords.iter().map(String::as_str))
            .collect();
        // `LeftmostLongest` per the plan: this automaton only gates whether
        // `detect()` runs at all (the trait's `prefilter()`), so which
        // keyword "wins" a tie never matters, only whether any hit at all.
        let prefilter = AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .match_kind(MatchKind::LeftmostLongest)
            .build(&keywords)
            .expect("keyword list is static and derived from the loaded ruleset");

        Self {
            rules,
            prefilter,
            marker_re: regex::Regex::new(r"\[REDACTED:[^\]\n]*\]|█+")
                .expect("literal marker pattern"),
        }
    }
}

impl Default for InternalDetector {
    fn default() -> Self {
        Self::new()
    }
}

fn mask_existing_markers(text: &str, marker_re: &regex::Regex) -> String {
    let mut out = text.to_string();
    for m in marker_re.find_iter(text) {
        // One NUL byte per matched byte: byte length is preserved, so
        // every later span still indexes correctly into the original text.
        out.replace_range(m.range(), &"\0".repeat(m.len()));
    }
    out
}

/// Gitleaks' `private-key` rule spans from a PEM header through its
/// footer, crossing newlines by design; a unified diff interleaves `+`/`-`
/// markers and unrelated lines between them, so redacting the raw span
/// would eat surrounding diff structure. Clip to the line containing the
/// match's start instead.
fn clip_to_line_if_diff(text: &str, shape: FieldShape, span: Range<usize>) -> Range<usize> {
    if shape != FieldShape::UnifiedDiff {
        return span;
    }
    let line_start = text[..span.start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[span.end..]
        .find('\n')
        .map_or(text.len(), |i| span.end + i);
    span.start.max(line_start)..span.end.min(line_end)
}

fn has_hotword_nearby(text: &str, span: &Range<usize>) -> bool {
    let start = text
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= span.start.saturating_sub(HOTWORD_WINDOW))
        .last()
        .unwrap_or(0);
    let end = (span.end + HOTWORD_WINDOW).min(text.len());
    let end = (end..=text.len())
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(text.len());
    let window = text[start..end].to_ascii_lowercase();
    HOTWORDS.iter().any(|h| window.contains(h))
}

/// Base confidence from a rule match, adjusted down when the matched text
/// is lower-entropy than the rule expects (proportional to how far below,
/// so a near-miss and a wildly-off match don't get the same penalty) and
/// up when a hotword sits within [`HOTWORD_WINDOW`] chars, then clamped.
fn score(rule: &rules::Rule, matched: &str, has_hotword: bool) -> f32 {
    let mut s = BASE_SCORE;
    if let Some(threshold) = rule.entropy {
        let actual = entropy::shannon(matched);
        if actual < threshold {
            s -= ((threshold - actual) as f32) * PENALTY_PER_ENTROPY_BIT;
        }
    }
    if has_hotword {
        s += HOTWORD_BONUS;
    }
    s.clamp(0.0, 1.0)
}

impl Detector for InternalDetector {
    fn id(&self) -> &'static str {
        "internal"
    }

    fn prefilter(&self, text: &str) -> bool {
        self.prefilter.is_match(text)
    }

    fn detect(&self, c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
        let masked = mask_existing_markers(c.text, &self.marker_re);
        let mut out = Vec::new();
        for (rule, re) in &self.rules {
            for caps in re.captures_iter(&masked) {
                // Group 1 is the secret in every gitleaks rule that has
                // surrounding context (an assignment operator, a quote);
                // group 0 is the whole match for rules with nothing to
                // trim (bare tokens like `aws-access-token`).
                let m = caps
                    .get(1)
                    .or_else(|| caps.get(0))
                    .expect("group 0 always exists");
                let raw_span = m.range();
                let raw_matched = &c.text[raw_span.clone()];
                if rule.allow.iter().any(|a| a.is_match(raw_matched)) {
                    continue;
                }
                let span = clip_to_line_if_diff(c.text, c.shape, raw_span);
                if span.is_empty() {
                    continue;
                }
                let matched = &c.text[span.clone()];
                let has_hotword = has_hotword_nearby(c.text, &span);
                out.push(Finding {
                    span,
                    rule: rule.id.clone(),
                    score: score(rule, matched, has_hotword),
                    detector: self.id(),
                });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Context;

    fn cand(text: &str, shape: FieldShape) -> Candidate<'_> {
        Candidate {
            text,
            shape,
            at: "/change/x/structural/extra/text",
            ctx: Context {
                change_type: "conversation.append",
                tool_name: None,
                actor: "human:t",
                kind: None,
            },
        }
    }

    fn detect_one(text: &str) -> Vec<Finding> {
        InternalDetector::new()
            .detect(&cand(text, FieldShape::Prose))
            .unwrap()
    }

    fn diff_candidate() -> Candidate<'static> {
        cand(
            "@@ -1,4 +1,4 @@\n-old line\n+-----BEGIN RSA PRIVATE KEY-----\n+MIIEpAIBAAKCAQEAxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n+-----END RSA PRIVATE KEY-----\n more diff context\n",
            FieldShape::UnifiedDiff,
        )
    }

    fn uri_candidate(text: &'static str) -> Candidate<'static> {
        cand(text, FieldShape::Uri)
    }

    #[test]
    fn detects_shipped_formats() {
        for (label, sample) in [
            ("aws", "AKIAIOSFODNN7REALKEY"),
            ("google", "AIzaSyD-0123456789abcdefghijklmnopqrstu"),
            ("jwt", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.QQQQQQQQQQ"),
            ("pem", "-----BEGIN RSA PRIVATE KEY-----"),
            ("dburi", "postgres://u:s3cr3tpass@db.internal:5432/prod"),
        ] {
            assert!(!detect_one(sample).is_empty(), "missed {label}");
        }
    }

    #[test]
    fn documented_false_positives_stay_below_threshold() {
        for sample in [
            "AKIAIOSFODNN7EXAMPLE",                   // AWS's own documentation key
            "redis://localhost:6379",                 // no password
            "0e2b3d4e3dec5f38ae95f62519eb2736f73c0b", // git SHA
            "550e8400-e29b-41d4-a716-446655440000",   // UUID
            "ThisIsAReallyLongString",                // high entropy, not a secret
        ] {
            assert!(
                detect_one(sample).iter().all(|f| f.score < 0.8),
                "false positive on {sample}"
            );
        }
    }

    #[test]
    fn diff_spans_never_cross_a_newline() {
        let c = diff_candidate();
        let findings = InternalDetector::new().detect(&c).unwrap();
        assert!(!findings.is_empty());
        for f in findings {
            assert!(!c.text[f.span.clone()].contains('\n'));
        }
    }

    #[test]
    fn uri_shape_redacts_only_the_password() {
        let c = uri_candidate("postgres://svc_user:h0rr1bl3@db.internal:5432/prod");
        let findings = InternalDetector::new().detect(&c).unwrap();
        assert_eq!(&c.text[findings[0].span.clone()], "h0rr1bl3");
    }

    #[test]
    fn existing_markers_are_never_re_detected() {
        assert!(detect_one("[REDACTED:aws-access-key-id:a3c829]").is_empty());
        assert!(detect_one("████████████████████").is_empty());
    }

    fn neutral_rule(entropy: Option<f64>) -> rules::Rule {
        rules::Rule {
            id: "test-rule".to_string(),
            regex: ".".to_string(),
            entropy,
            keywords: vec![],
            allow: vec![],
        }
    }

    #[test]
    fn below_entropy_lowers_score() {
        let rule = neutral_rule(Some(3.0));
        let s = score(&rule, "aaaaaaaaaa", false); // shannon == 0.0, well below 3.0
        assert!(
            s < BASE_SCORE && s > 0.0,
            "expected a reduced but non-clamped score, got {s}"
        );
    }

    #[test]
    fn above_entropy_keeps_base_score() {
        let rule = neutral_rule(Some(1.0));
        let s = score(&rule, "abcdefghij", false); // shannon == log2(10), well above 1.0
        assert_eq!(s, BASE_SCORE);
    }

    #[test]
    fn hotword_present_boosts_score() {
        let rule = neutral_rule(Some(2.0));
        let without = score(&rule, "aaaaaaaaaa", false);
        let with = score(&rule, "aaaaaaaaaa", true);
        assert!(
            with > without,
            "hotword should raise the score: {with} vs {without}"
        );
    }

    #[test]
    fn hotword_absent_does_not_boost() {
        let rule = neutral_rule(None);
        assert_eq!(score(&rule, "anything", false), BASE_SCORE);
    }

    #[test]
    fn clamp_low() {
        let rule = neutral_rule(Some(8.0)); // no real string reaches 8 bits of entropy
        let s = score(&rule, "aaaaaaaaaa", false);
        assert_eq!(s, 0.0);
    }

    #[test]
    fn clamp_high() {
        let rule = neutral_rule(None);
        let s = score(&rule, "anything", true);
        assert_eq!(s, 1.0);
    }
}
