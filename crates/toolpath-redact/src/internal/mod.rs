//! The built-in detector: a vendored gitleaks ruleset, an entropy gate,
//! and a keyword prefilter.

pub mod entropy;
pub mod rules;

use crate::FieldShape;
use crate::detect::{Candidate, Detector, Finding};
use aho_corasick::{AhoCorasick, MatchKind};
use std::ops::Range;
use std::sync::LazyLock;

/// A bare rule match clears the 0.8 plan-default threshold on its own; the
/// remaining 0.15 is headroom for a modest entropy shortfall. A hotword is
/// corroboration, not the gate - when it *was* the gate (`BASE_SCORE` 0.6,
/// `HOTWORD_BONUS` 0.5) every credential without one of ten English words
/// beside it scored 0.6 and was silently skipped.
const BASE_SCORE: f32 = 0.85;
/// Large enough that the entropy penalty and the bonus cancel at a
/// shortfall of exactly one bit, and that base + bonus clamps.
const HOTWORD_BONUS: f32 = 0.2;
const PENALTY_PER_ENTROPY_BIT: f32 = 0.2;
/// In characters, not bytes: a byte window shrinks to a third of its
/// nominal size on CJK text, so the same secret detects worse in one
/// language than another.
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

/// The compiled ruleset, built once per process. Compiling the ruleset
/// costs ~1.5 s in release, and `InternalDetector::new()` is called per
/// document by sync replay. Pure: a function of an `include_str!` constant,
/// so no environment, filesystem, clock, or write-once global is involved.
static COMPILED: LazyLock<Compiled> = LazyLock::new(Compiled::build);

struct Compiled {
    rules: Vec<(rules::Rule, regex::Regex)>,
    /// Rule indices with no keyword of their own; nothing gates them.
    ungated: Vec<usize>,
    /// Pattern index in `keywords` -> index into `rules`.
    keyword_owner: Vec<usize>,
    keywords: AhoCorasick,
    hotwords: AhoCorasick,
    global_allow: Vec<rules::Allowlist>,
    /// Recognises this crate's own [`crate::Transform::Marker`],
    /// [`crate::Transform::Mask`] and [`crate::Transform::Partial`] output.
    /// A finding overlapping one is dropped, or a second pass would
    /// fingerprint the first pass's replacement and redaction would never
    /// reach a fixed point.
    ///
    /// [`crate::Transform::Hash`] emits bare 6-hex with no envelope and is
    /// **not** recognisable here; anything relying on idempotence must not
    /// assume that variant is covered.
    marker_re: regex::Regex,
}

impl Compiled {
    fn build() -> Self {
        let ruleset = rules::load_rules();
        let rules: Vec<(rules::Rule, regex::Regex)> = ruleset
            .rules
            .into_iter()
            .map(|r| {
                let re = rules::compile(&r.regex)
                    .unwrap_or_else(|e| panic!("rule {} failed to compile: {e}", r.id));
                (r, re)
            })
            .collect();

        let mut keywords: Vec<&str> = Vec::new();
        let mut keyword_owner: Vec<usize> = Vec::new();
        let mut ungated: Vec<usize> = Vec::new();
        for (i, (rule, _)) in rules.iter().enumerate() {
            if rule.keywords.is_empty() {
                ungated.push(i);
            }
            for k in &rule.keywords {
                keywords.push(k);
                keyword_owner.push(i);
            }
        }

        // `Standard`, not `LeftmostLongest`: the automaton now decides
        // *which* rules run, and overlapping keywords must all report -
        // under leftmost-longest "apikey" hides the rules keyed on "api".
        let build_automaton = |patterns: &[&str]| {
            AhoCorasick::builder()
                .ascii_case_insensitive(true)
                .match_kind(MatchKind::Standard)
                .build(patterns)
                .expect("keyword list is static and derived from the loaded ruleset")
        };

        Self {
            keywords: build_automaton(&keywords),
            hotwords: build_automaton(HOTWORDS),
            rules,
            ungated,
            keyword_owner,
            global_allow: ruleset.global,
            marker_re: regex::Regex::new(r"\[REDACTED:[^\]\n]*\]|\u{2588}+|\S{1,8}\u{2026}\S{1,8}")
                .expect("literal marker pattern"),
        }
    }

    /// Only the rules whose keyword actually appears. Gitleaks' `keywords`
    /// gate exists because running every regex over every string leaf costs
    /// ~1.2 s per 200 KB of clean text; gating picks 7 rules out of 224 on
    /// this repo's own CLAUDE.md and scans 224 KB of it in ~52 ms.
    fn candidate_rules(&self, text: &str) -> Vec<usize> {
        let mut seen = vec![false; self.rules.len()];
        let mut out = self.ungated.clone();
        for &i in &out {
            seen[i] = true;
        }
        for m in self.keywords.find_overlapping_iter(text) {
            let rule = self.keyword_owner[m.pattern().as_usize()];
            if !seen[rule] {
                seen[rule] = true;
                out.push(rule);
            }
        }
        out.sort_unstable();
        out
    }
}

pub struct InternalDetector {
    compiled: &'static Compiled,
}

impl InternalDetector {
    pub fn new() -> Self {
        Self {
            compiled: &COMPILED,
        }
    }
}

impl Default for InternalDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// The capture group holding the credential.
///
/// Group 1 is the secret in most gitleaks rules, but not all: it is the
/// literal `(login|token)` in `sonar-api-token`, and it does not
/// participate at all in `curl-auth-header` unless the header was Basic
/// auth - where falling back to group 0 would redact the whole command
/// line. Preferring the longest participating group gets both right, and
/// upstream's `secretGroup` (plus this crate's whole-match overrides) wins
/// outright where it is annotated.
fn secret_of<'t>(rule: &rules::Rule, caps: &regex::Captures<'t>) -> regex::Match<'t> {
    if let Some(m) = rule.secret_group.and_then(|g| caps.get(g)) {
        return m;
    }
    let mut best: Option<regex::Match<'t>> = None;
    for i in 1..caps.len() {
        let Some(m) = caps.get(i) else { continue };
        if best.is_none_or(|b| m.len() > b.len()) {
            best = Some(m);
        }
    }
    best.unwrap_or_else(|| caps.get(0).expect("group 0 always participates"))
}

fn line_around(text: &str, span: &Range<usize>) -> Range<usize> {
    let start = text[..span.start].rfind('\n').map_or(0, |i| i + 1);
    let end = text[span.end..]
        .find('\n')
        .map_or(text.len(), |i| span.end + i);
    start..end
}

/// Gitleaks' `private-key` rule spans a PEM block from header to footer,
/// crossing newlines by design. A unified diff interleaves `+`/`-` markers
/// and unrelated lines between them, so redacting the raw span would eat
/// the diff structure - but clipping to the first line instead leaves the
/// key body itself in the document. Split, and redact each line.
fn split_to_lines(text: &str, shape: FieldShape, span: Range<usize>) -> Vec<Range<usize>> {
    if shape != FieldShape::UnifiedDiff || !text[span.clone()].contains('\n') {
        return vec![span];
    }
    let mut out = Vec::new();
    let mut at = span.start;
    while at < span.end {
        let end = text[at..span.end].find('\n').map_or(span.end, |i| at + i);
        // A continuation line starts on the hunk's `+`/`-`/` ` marker,
        // which is structure rather than content; redacting it detaches the
        // line from its hunk.
        let start = match text.as_bytes().get(at) {
            Some(b'+' | b'-' | b' ') if at > span.start => at + 1,
            _ => at,
        };
        if start < end {
            out.push(start..end);
        }
        at = end + 1;
    }
    out
}

fn has_hotword_nearby(compiled: &Compiled, text: &str, span: &Range<usize>) -> bool {
    let start = text[..span.start]
        .char_indices()
        .rev()
        .nth(HOTWORD_WINDOW - 1)
        .map_or(0, |(i, _)| i);
    let end = text[span.end..]
        .char_indices()
        .nth(HOTWORD_WINDOW)
        .map_or(text.len(), |(i, _)| span.end + i);
    compiled.hotwords.is_match(&text[start..end])
}

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
        // Every vendored rule currently carries a keyword, but a rule with
        // none must never be skipped by the keyword automaton it is not in.
        !self.compiled.ungated.is_empty() || self.compiled.keywords.is_match(text)
    }

    fn detect(&self, c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
        let text = c.text;
        let markers: Vec<Range<usize>> = self
            .compiled
            .marker_re
            .find_iter(text)
            .map(|m| m.range())
            .collect();
        let mut out = Vec::new();
        for i in self.compiled.candidate_rules(text) {
            let (rule, re) = &self.compiled.rules[i];
            for caps in re.captures_iter(text) {
                let raw = secret_of(rule, &caps).range();
                if markers
                    .iter()
                    .any(|m| raw.start < m.end && raw.end > m.start)
                {
                    continue;
                }
                let whole = caps.get(0).expect("group 0 always participates").range();
                let secret = &text[raw.clone()];
                let allowed = rule
                    .allow
                    .iter()
                    .chain(&self.compiled.global_allow)
                    .any(|a| {
                        a.allows(
                            secret,
                            &text[whole.clone()],
                            &text[line_around(text, &whole)],
                        )
                    });
                if allowed {
                    continue;
                }
                for span in split_to_lines(text, c.shape, raw.clone()) {
                    let has_hotword = has_hotword_nearby(self.compiled, text, &span);
                    out.push(Finding {
                        score: score(rule, &text[span.clone()], has_hotword),
                        span,
                        rule: rule.id.clone(),
                        detector: self.id(),
                    });
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Context;

    /// The CLI's default `--threshold`. Every constant in this module is
    /// tuned against it, so it is asserted against directly.
    const DEFAULT_THRESHOLD: f32 = 0.8;

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

    /// The span the detector would actually rewrite, for the highest-scoring
    /// finding of a named rule.
    fn redacted_span<'a>(text: &'a str, rule: &str) -> &'a str {
        let findings = detect_one(text);
        let f = findings
            .iter()
            .filter(|f| f.rule == rule)
            .max_by(|a, b| a.score.total_cmp(&b.score))
            .unwrap_or_else(|| panic!("{rule} did not fire on {text:?}: {findings:?}"));
        &text[f.span.clone()]
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

    /// Split across `concat!` so the source text does not match the pattern
    /// the value is testing. These are synthetic and authenticate nothing,
    /// but GitHub push protection scans the file, not the compiled string,
    /// and rejects any push whose diff contains a well-formed token.
    #[test]
    fn detects_shipped_formats() {
        for (label, sample) in [
            ("aws", "AKIAIOSFODNN7REALKEY"),
            ("google", "AIzaSyD-0123456789abcdefghijklmnopqrstu"),
            ("jwt", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.QQQQQQQQQQ"),
            ("pem", "-----BEGIN RSA PRIVATE KEY-----"),
            ("dburi", "postgres://u:s3cr3tpass@db.internal:5432/prod"),
            ("github-pat", "ghp_Zt9xQw3pLm7RvB2kNc5YdA8jHf1UsE0iOqTg"),
            (
                "stripe",
                concat!("sk_", "live_", "Zt9xQw3pLm7RvB2kNc5YdA8j"),
            ),
            (
                "slack-bot",
                concat!(
                    "xo",
                    "xb-",
                    "901234567890-9012345678901-Zt9xQw3pLm7RvB2kNc5YdA8j"
                ),
            ),
            ("gitlab-pat", concat!("gl", "pat-", "Zt9xQw3pLm7RvB2kNc5Y")),
            (
                "anthropic",
                concat!("sk-", "ant-", "api03-Zt9xQw3pLm7RvB2kNc5YdA8jHf1UsE0iOqTg"),
            ),
            (
                "slack-webhook",
                concat!(
                    "https://hooks.sl",
                    "ack.com/services/T01234567/B01234567/Zt9xQw3pLm7RvB2kNc5YdA8j"
                ),
            ),
        ] {
            let findings = detect_one(sample);
            assert!(!findings.is_empty(), "missed {label}");
            let best = findings.iter().map(|f| f.score).fold(f32::MIN, f32::max);
            assert!(
                best >= DEFAULT_THRESHOLD,
                "{label} scored {best}, under the {DEFAULT_THRESHOLD} default threshold"
            );
        }
    }

    /// Verbatim from the `.env` block of a real cache document in which only
    /// the URI password was redacted. The other two credentials were invisible
    /// to the detector - one to a length-pinned vendored rule, one to
    /// gitleaks' documentation-key allowlist - so they are pinned here by
    /// literal rather than by shape.
    const LEAKED_ENV_BLOCK: &str = concat!(
        "ANTHROPIC_API_KEY=sk-ant-api03-EXAMPLEONLYnotarealkey000000000000000000000AA\n",
        "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n",
        "DATABASE_URL=postgres://svc_user:h0rr1bl3-p4ss@db.internal:5432/prod",
    );

    #[test]
    fn every_credential_in_the_leaked_env_block_is_detected() {
        let findings = detect_one(LEAKED_ENV_BLOCK);
        for secret in [
            "sk-ant-api03-EXAMPLEONLYnotarealkey000000000000000000000AA",
            "AKIAIOSFODNN7EXAMPLE",
            "h0rr1bl3-p4ss",
        ] {
            let at = LEAKED_ENV_BLOCK.find(secret).expect("fixture holds it");
            let best = findings
                .iter()
                .filter(|f| f.span.start <= at && f.span.end >= at + secret.len())
                .map(|f| f.score)
                .fold(f32::MIN, f32::max);
            assert!(
                best >= DEFAULT_THRESHOLD,
                "{secret} is covered at {best}, under the {DEFAULT_THRESHOLD} \
                 default threshold: {findings:?}"
            );
        }
    }

    /// Gitleaks allowlists AWS's own `...EXAMPLE` key because a README
    /// quoting it is not a leak. Redaction answers a different question -
    /// see `SUPPRESSED_ALLOWLIST_REGEXES` - so the exception is off here and
    /// the literal is treated like any other key-shaped string.
    #[test]
    fn aws_documentation_keys_are_redacted_like_any_other_key() {
        let findings = detect_one("AKIAIOSFODNN7EXAMPLE");
        let best = findings.iter().map(|f| f.score).fold(f32::MIN, f32::max);
        assert!(
            best >= DEFAULT_THRESHOLD,
            "AWS's documentation key scored {best}: {findings:?}"
        );
    }

    #[test]
    fn documented_false_positives_stay_below_threshold() {
        // `AKIAIOSFODNN7EXAMPLE` was once on this list. It is now a
        // deliberate true positive; see the test above.
        for sample in [
            "redis://localhost:6379", // no password
            // A real 40-hex git SHA next to a hotword: the shape
            // `sourcegraph-access-token` also accepts.
            "reverted at commit token 0e2b3d4e3dec5f38ae95f62519eb2736f73c0b91",
            "550e8400-e29b-41d4-a716-446655440000", // UUID
            "ThisIsAReallyLongString",              // high entropy, not a secret
        ] {
            let findings = detect_one(sample);
            assert!(
                findings.iter().all(|f| f.score < DEFAULT_THRESHOLD),
                "false positive on {sample}: {findings:?}"
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
    fn diff_findings_cover_the_private_key_body() {
        let c = diff_candidate();
        let findings = InternalDetector::new().detect(&c).unwrap();
        let body = c
            .text
            .find("MIIEpAIBAAKCAQEA")
            .expect("fixture holds the key body");
        let covered = findings
            .iter()
            .any(|f| f.span.start <= body && f.span.end >= body + "MIIEpAIBAAKCAQEA".len());
        assert!(covered, "the key body is covered by nothing: {findings:?}");
    }

    /// Every line of the fixture diff carries a `+`/`-`/` ` marker, so a
    /// finding that starts at a line start is one that would swallow the
    /// marker and detach the line from its hunk.
    #[test]
    fn diff_findings_keep_the_hunk_marker() {
        let c = diff_candidate();
        for f in InternalDetector::new().detect(&c).unwrap() {
            assert!(
                f.span.start > 0 && c.text.as_bytes()[f.span.start - 1] != b'\n',
                "redacting {:?} would take its hunk marker with it",
                &c.text[f.span.clone()]
            );
        }
    }

    #[test]
    fn uri_shape_redacts_only_the_password() {
        let c = uri_candidate("postgres://svc_user:h0rr1bl3@db.internal:5432/prod");
        let findings = InternalDetector::new().detect(&c).unwrap();
        assert_eq!(&c.text[findings[0].span.clone()], "h0rr1bl3");
    }

    #[test]
    fn sonar_token_redacts_the_credential_not_the_keyword() {
        let text = "sonar.token=squ_0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            redacted_span(text, "sonar-api-token"),
            "squ_0123456789abcdef0123456789abcdef01234567"
        );
    }

    #[test]
    fn teams_webhook_redacts_the_whole_url() {
        let text = "https://acme.webhook.office.com/webhookb2/0123abcd-0123-4567-89ab-0123456789ab@0123abcd-0123-4567-89ab-0123456789ab/IncomingWebhook/0123456789abcdef0123456789abcdef/0123abcd-0123-4567-89ab-0123456789ab";
        assert_eq!(redacted_span(text, "microsoft-teams-webhook"), text);
    }

    #[test]
    fn jwt_base64_redacts_the_whole_token() {
        let text = "ZXlKaGJHY2lPaUpJVXpJMU5pSjkuZXlKemRXSWlPaUl4SW4wLlFRUVFRUVFRUVE";
        assert_eq!(redacted_span(text, "jwt-base64"), text);
    }

    #[test]
    fn curl_auth_header_redacts_only_the_bearer_token() {
        let text = r#"curl -H "Authorization: Bearer Zt9xQw3pLm7RvB2kNc5YdA8j" https://api.example.com/v1/things"#;
        assert_eq!(
            redacted_span(text, "curl-auth-header"),
            "Zt9xQw3pLm7RvB2kNc5YdA8j"
        );
    }

    /// A marker blanked in place would delete the `:` separators that were
    /// the only reason `uri-credential` did not match the surrounding URI,
    /// and the rule id inside the marker carries the hotword "credential" -
    /// so the previous pass's output re-detected at 1.000 and redaction
    /// never reached a fixed point.
    #[test]
    fn existing_markers_are_never_re_detected() {
        for replacement in [
            "[REDACTED:uri-credential:e90e4c]",
            "\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}",
            "h0rr\u{2026}bl3x",
        ] {
            let text = format!("postgres://svc_user:{replacement}@db.internal:5432/prod");
            let findings = InternalDetector::new()
                .detect(&cand(&text, FieldShape::Uri))
                .unwrap();
            assert!(
                findings.is_empty(),
                "re-detected {replacement}: {findings:?}"
            );
        }
        let text = r#"curl -u "admin:[REDACTED:curl-basic-auth:a3c829]" https://api.example.com"#;
        let findings = detect_one(text);
        assert!(findings.is_empty(), "re-detected a marker: {findings:?}");
    }

    /// 30 three-byte chars of padding is 90 bytes: inside a 50-character
    /// window, outside a 50-*byte* one. The trailing space matters - `é` is
    /// a word character, so without it the rule's leading `\b` never
    /// matches and the key is missed for an unrelated reason.
    fn padded(chars: usize) -> String {
        format!("token {} AKIAIOSFODNN7REALKEY", "é".repeat(chars))
    }

    #[test]
    fn a_hotword_thirty_multibyte_chars_away_still_corroborates() {
        let text = padded(30);
        let findings = detect_one(&text);
        assert!(
            !findings.is_empty(),
            "multibyte padding hid the key entirely"
        );
        assert!(
            findings.iter().any(|f| f.score >= 1.0),
            "hotword 30 chars away did not corroborate: {findings:?}"
        );
    }

    #[test]
    fn a_hotword_beyond_the_window_does_not_corroborate() {
        let text = padded(60);
        let findings = detect_one(&text);
        assert!(!findings.is_empty());
        assert!(findings.iter().all(|f| f.score < 1.0), "{findings:?}");
    }

    #[test]
    fn only_rules_whose_keyword_appears_are_run() {
        let compiled = &*COMPILED;
        let all = compiled.rules.len();
        let picked = compiled.candidate_rules("AKIAIOSFODNN7REALKEY").len();
        assert!(
            picked < all / 10,
            "{picked} of {all} rules ran on one AWS key"
        );
        assert!(picked > 0);
    }

    /// Under `LeftmostLongest` the longer keyword swallows the shorter and
    /// every rule keyed on the shorter one stops running.
    #[test]
    fn overlapping_keywords_activate_every_owning_rule() {
        let compiled = &*COMPILED;
        let owners: Vec<&str> = compiled
            .candidate_rules("apikey")
            .into_iter()
            .map(|i| compiled.rules[i].0.id.as_str())
            .collect();
        assert!(owners.contains(&"generic-api-key"), "{owners:?}");
    }

    fn neutral_rule(entropy: Option<f64>) -> rules::Rule {
        rules::Rule {
            id: "test-rule".to_string(),
            regex: ".".to_string(),
            entropy,
            keywords: vec![],
            secret_group: None,
            allow: vec![],
        }
    }

    #[track_caller]
    fn assert_score(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    /// `shannon` is exactly 1.0 on this: two symbols, equal counts.
    const ONE_BIT: &str = "aaaabbbb";
    /// Exactly 2.0: four symbols, equal counts.
    const TWO_BITS: &str = "abcdabcd";

    #[test]
    fn penalty_is_proportional_to_shortfall() {
        assert_score(score(&neutral_rule(Some(2.0)), ONE_BIT, false), 0.65);
        assert_score(score(&neutral_rule(Some(3.0)), ONE_BIT, false), 0.45);
    }

    #[test]
    fn entropy_exactly_at_threshold_is_not_penalised() {
        assert_score(score(&neutral_rule(Some(2.0)), TWO_BITS, false), BASE_SCORE);
    }

    #[test]
    fn no_entropy_threshold_skips_the_penalty_entirely() {
        assert_score(score(&neutral_rule(None), "aaaaaaaaaa", false), BASE_SCORE);
    }

    #[test]
    fn hotword_alone_clears_the_default_threshold() {
        let rule = neutral_rule(Some(1.5));
        assert_score(score(&rule, ONE_BIT, false), 0.75);
        assert_score(score(&rule, ONE_BIT, true), 0.95);
    }

    #[test]
    fn entropy_shortfall_that_cancels_the_hotword_bonus() {
        // A one-bit shortfall costs exactly what a hotword pays.
        assert_score(score(&neutral_rule(Some(2.0)), ONE_BIT, true), BASE_SCORE);
    }

    #[test]
    fn hotword_bonus_applies_before_the_clamp() {
        // Clamping first would leave 0.0 + HOTWORD_BONUS.
        assert_score(score(&neutral_rule(Some(8.0)), "aaaaaaaaaa", true), 0.0);
        assert_score(score(&neutral_rule(None), "anything", true), 1.0);
    }

    #[test]
    fn above_entropy_keeps_base_score() {
        assert_score(
            score(&neutral_rule(Some(1.0)), "abcdefghij", false),
            BASE_SCORE,
        );
    }

    #[test]
    fn hotword_present_boosts_score() {
        let rule = neutral_rule(Some(2.0));
        let without = score(&rule, "aaaaaaaaaa", false);
        let with = score(&rule, "aaaaaaaaaa", true);
        assert_score(with - without, HOTWORD_BONUS);
    }

    #[test]
    fn clamp_low() {
        assert_score(score(&neutral_rule(Some(8.0)), "aaaaaaaaaa", false), 0.0);
    }
}
