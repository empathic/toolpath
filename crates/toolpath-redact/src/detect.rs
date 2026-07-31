//! The detection contract.
//!
//! Detectors see strings and a little context, never the document. That is
//! what makes them testable in isolation and leaves a harness-time hook
//! path open.

use std::ops::Range;

/// What kind of text a candidate holds. Detectors use it to decide how to
/// read the string; the plan reports it so a reviewer can filter on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldShape {
    Prose,
    ToolInput,
    ToolOutput,
    UnifiedDiff,
    FileContent,
    Uri,
    OpaqueJson,
}

/// Where in the document a candidate came from, in terms a detector can
/// act on without resolving pointers itself.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub change_type: &'a str,
    pub tool_name: Option<&'a str>,
    pub actor: &'a str,
    pub kind: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub text: &'a str,
    pub shape: FieldShape,
    /// RFC 6901 pointer relative to the step. Passed through to the audit
    /// record verbatim, so a detector never constructs one.
    pub at: &'a str,
    pub ctx: Context<'a>,
}

/// One span a detector claims is a secret. `span` indexes `Candidate::text`
/// in bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub span: Range<usize>,
    /// Lands in the audit record, so it is part of the document contract.
    pub rule: String,
    pub score: f32,
    pub detector: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Egress {
    LocalOnly,
    Network,
}

pub trait Detector: Send + Sync {
    fn id(&self) -> &'static str;

    fn detect(&self, c: &Candidate<'_>) -> crate::Result<Vec<Finding>>;

    /// A cheap rejection test run before `detect`. Returning `false` skips
    /// this detector for this candidate entirely.
    fn prefilter(&self, _text: &str) -> bool {
        true
    }

    /// The host refuses a `Network` detector unless explicitly allowed:
    /// validating a candidate against its issuing provider sends secret
    /// material off the machine.
    fn egress(&self) -> Egress {
        Egress::LocalOnly
    }
}

#[derive(Default)]
pub struct DetectorSet(Vec<Box<dyn Detector>>);

impl DetectorSet {
    pub fn push(&mut self, d: Box<dyn Detector>) {
        self.0.push(d);
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.0.iter().map(|d| d.id()).collect()
    }

    /// The detectors in the set, in insertion order.
    pub fn detectors(&self) -> &[Box<dyn Detector>] {
        &self.0
    }

    /// Run every detector and reconcile their output into one set of
    /// non-overlapping, applicable spans.
    ///
    /// ORDERING CONSTRAINT, and getting it wrong publishes a secret: overlap
    /// resolution is score-blind on length, so a low-scoring container evicts
    /// a high-scoring finding nested inside it. Threshold BEFORE this runs,
    /// never after - thresholding the output lets a container that is about
    /// to be discarded take the survivor down with it. A caller that has a
    /// threshold wants `detect_all_partitioned`.
    pub fn detect_all(&self, c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
        Ok(normalise(c.text, self.raw(c)?))
    }

    /// `detect_all`, split on `threshold`, with overlaps resolved on each
    /// side separately. Returns `(above, below)`.
    ///
    /// This is the ordering constraint on `detect_all` made structural.
    /// Resolution is score-blind on length, so a whole-line 0.6 generic-entropy
    /// hit would evict the 0.99 AWS key nested inside it, and thresholding
    /// afterwards would then discard the container too - publishing the key.
    /// Resolving the above-threshold set on its own means nothing the caller
    /// is about to discard can contest it.
    ///
    /// `below` is what survives resolution among the sub-threshold findings,
    /// minus anything overlapping an above-threshold winner: it exists so a
    /// reviewer can lower the bar after the fact (`--accept score>=0.5`), and
    /// a row that lost its bytes to a redaction is not offerable.
    ///
    /// Each side is sorted by span start, and the union of the two is
    /// non-overlapping.
    pub fn detect_all_partitioned(
        &self,
        c: &Candidate<'_>,
        threshold: f32,
    ) -> crate::Result<(Vec<Finding>, Vec<Finding>)> {
        let (above, below): (Vec<Finding>, Vec<Finding>) =
            self.raw(c)?.into_iter().partition(|f| f.score >= threshold);

        let above = normalise(c.text, above);
        let below = normalise(c.text, below)
            .into_iter()
            .filter(|f| {
                !above
                    .iter()
                    .any(|w| f.span.start < w.span.end && f.span.end > w.span.start)
            })
            .collect();
        Ok((above, below))
    }

    /// Every detector's unreconciled output, in registration order.
    fn raw(&self, c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
        let mut raw = Vec::new();
        for d in &self.0 {
            if !d.prefilter(c.text) {
                continue;
            }
            raw.extend(d.detect(c)?);
        }
        Ok(raw)
    }
}

/// Drop what cannot be applied, then resolve overlaps.
///
/// Policy from Presidio: identical spans, higher score wins; nested, the
/// container wins regardless of score. Every comparison ends in a total
/// order over rule and detector id, so the result cannot depend on the order
/// detectors were registered in or on any iteration order upstream.
fn normalise(text: &str, mut findings: Vec<Finding>) -> Vec<Finding> {
    findings.retain(|f| {
        // A NaN score is not orderable, and a single non-comparable element
        // makes the whole sort's outcome depend on input order. An infinite
        // one is orderable but not representable in JSON - serde writes it as
        // `null`, and the plan file it lands in stops parsing.
        f.score.is_finite()
            && f.span.start < f.span.end
            && f.span.end <= text.len()
            && text.is_char_boundary(f.span.start)
            && text.is_char_boundary(f.span.end)
    });

    findings.sort_by(|a, b| {
        let (a_len, b_len) = (a.span.end - a.span.start, b.span.end - b.span.start);
        b_len
            .cmp(&a_len)
            .then(b.score.total_cmp(&a.score))
            .then(a.span.start.cmp(&b.span.start))
            .then(a.rule.cmp(&b.rule))
            .then(a.detector.cmp(b.detector))
    });

    // Best-first: keep whatever no earlier winner has already claimed.
    // Resolving clashes pairwise instead loses a finding whose only conflict
    // was itself evicted later - with A overlapping B, B overlapping C, and A
    // disjoint from C, C displaces B and A never comes back.
    //
    // Equal length and equal score break on leftmost, not on rule name: a
    // rule-name tie-break would let an untrusted detector win contested bytes
    // by naming its rule `aaa`.
    let mut out: Vec<Finding> = Vec::new();
    for f in findings {
        let claimed = out
            .iter()
            .any(|o| f.span.start < o.span.end && f.span.end > o.span.start);
        if !claimed {
            out.push(f);
        }
    }

    out.sort_by_key(|f| f.span.start);
    out
}

/// Canned findings, for tests exercising traversal or transform without
/// depending on regex behaviour.
pub struct FixedDetector(pub Vec<Finding>);

impl Detector for FixedDetector {
    fn id(&self) -> &'static str {
        "fixed"
    }

    fn detect(&self, _c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
        Ok(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct HostileDetector(Vec<Finding>);
    impl Detector for HostileDetector {
        fn id(&self) -> &'static str {
            "hostile"
        }
        fn detect(&self, _c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
            Ok(self.0.clone())
        }
    }

    fn cand(text: &str) -> Candidate<'_> {
        Candidate {
            text,
            shape: FieldShape::Prose,
            at: "/change/x/structural/extra/text",
            ctx: Context {
                change_type: "conversation.append",
                tool_name: None,
                actor: "human:t",
                kind: None,
            },
        }
    }

    fn f(span: Range<usize>, rule: &str, score: f32) -> Finding {
        Finding {
            span,
            rule: rule.into(),
            score,
            detector: "hostile",
        }
    }

    #[test]
    fn drops_out_of_range_spans() {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![f(0..999, "x", 0.9)])));
        assert!(s.detect_all(&cand("short")).unwrap().is_empty());
    }

    // A reversed range is exactly the hostile input under test, so clippy's
    // "this range is empty" lint is the wrong call here.
    #[allow(clippy::reversed_empty_ranges)]
    #[test]
    fn drops_reversed_spans() {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![Finding {
            span: 5..2,
            rule: "x".into(),
            score: 0.9,
            detector: "hostile",
        }])));
        assert!(s.detect_all(&cand("abcdefgh")).unwrap().is_empty());
    }

    #[test]
    fn drops_mid_codepoint_spans() {
        // "é" is two bytes; 0..1 splits it.
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![f(0..1, "x", 0.9)])));
        assert!(s.detect_all(&cand("é-tail")).unwrap().is_empty());
    }

    #[test]
    fn identical_spans_higher_score_wins() {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![
            f(0..4, "low", 0.4),
            f(0..4, "high", 0.9),
        ])));
        let out = s.detect_all(&cand("abcdefgh")).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rule, "high");
    }

    #[test]
    fn nested_span_container_wins_regardless_of_score() {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![
            f(2..4, "inner", 0.99),
            f(0..8, "outer", 0.20),
        ])));
        let out = s.detect_all(&cand("abcdefgh")).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rule, "outer");
    }

    #[test]
    fn output_is_sorted_and_deterministic() {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![
            f(6..8, "b", 0.9),
            f(0..2, "a", 0.9),
            f(3..5, "c", 0.9),
        ])));
        let a = s.detect_all(&cand("abcdefgh")).unwrap();
        let b = s.detect_all(&cand("abcdefgh")).unwrap();
        assert_eq!(a, b);
        assert!(a.windows(2).all(|w| w[0].span.start <= w[1].span.start));
    }

    #[test]
    fn prefilter_short_circuits_detect() {
        struct NeverCalled;
        impl Detector for NeverCalled {
            fn id(&self) -> &'static str {
                "never"
            }
            fn prefilter(&self, _t: &str) -> bool {
                false
            }
            fn detect(&self, _c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
                panic!("detect() must not run when prefilter() is false")
            }
        }
        let mut s = DetectorSet::default();
        s.push(Box::new(NeverCalled));
        assert!(s.detect_all(&cand("anything")).unwrap().is_empty());
    }

    const ALPHABET: &str = "abcdefghijklmnopqrstuvwxyz";

    fn spans(out: &[Finding]) -> Vec<(usize, usize)> {
        out.iter().map(|f| (f.span.start, f.span.end)).collect()
    }

    fn rules(out: &[Finding]) -> Vec<&str> {
        out.iter().map(|f| f.rule.as_str()).collect()
    }

    fn detect(text: &str, findings: Vec<Finding>) -> Vec<Finding> {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(findings)));
        s.detect_all(&cand(text)).unwrap()
    }

    #[test]
    fn adjacent_spans_both_survive() {
        let out = detect("abcdefgh", vec![f(0..4, "a", 0.9), f(4..8, "b", 0.9)]);
        assert_eq!(spans(&out), vec![(0, 4), (4, 8)]);
    }

    #[test]
    fn three_way_overlap_keeps_the_span_vacated_by_an_evicted_rival() {
        // A-B overlap, B-C overlap, A-C disjoint. C evicts B, which is the
        // only thing that ever contested A, so A must survive.
        let out = detect(
            ALPHABET,
            vec![f(0..5, "a", 0.5), f(4..9, "b", 0.9), f(8..20, "c", 0.5)],
        );
        assert_eq!(spans(&out), vec![(0, 5), (8, 20)]);
        assert_eq!(rules(&out), vec!["a", "c"]);
    }

    #[test]
    fn three_way_overlap_through_one_container() {
        let out = detect(
            ALPHABET,
            vec![f(0..4, "a", 0.9), f(2..12, "b", 0.1), f(10..14, "c", 0.9)],
        );
        assert_eq!(spans(&out), vec![(2, 12)]);
    }

    #[test]
    fn empty_text_drops_everything() {
        assert!(detect("", vec![f(0..0, "a", 0.9), f(0..1, "b", 0.9)]).is_empty());
    }

    #[test]
    fn span_covering_whole_text_survives() {
        let out = detect("abcdefgh", vec![f(0..8, "a", 0.9)]);
        assert_eq!(spans(&out), vec![(0, 8)]);
    }

    #[test]
    fn drops_zero_length_spans() {
        assert!(detect("abcdefgh", vec![f(3..3, "a", 0.9)]).is_empty());
    }

    #[test]
    fn drops_nan_scores() {
        assert!(detect("abcdefgh", vec![f(0..4, "a", f32::NAN)]).is_empty());
    }

    #[test]
    fn drops_infinite_scores() {
        assert!(detect("abcdefgh", vec![f(0..4, "a", f32::INFINITY)]).is_empty());
        assert!(detect("abcdefgh", vec![f(0..4, "a", f32::NEG_INFINITY)]).is_empty());
    }

    fn partition(
        text: &str,
        findings: Vec<Finding>,
        threshold: f32,
    ) -> (Vec<Finding>, Vec<Finding>) {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(findings)));
        s.detect_all_partitioned(&cand(text), threshold).unwrap()
    }

    #[test]
    fn a_sub_threshold_container_does_not_evict_the_finding_nested_in_it() {
        // What `detect_all` gets wrong: score-blind on length, the 0.6
        // container wins, and thresholding its output then drops the 0.99
        // finding along with it.
        let evicted = detect(
            ALPHABET,
            vec![f(0..20, "container", 0.6), f(4..9, "key", 0.99)],
        );
        assert_eq!(rules(&evicted), vec!["container"]);

        let (above, below) = partition(
            ALPHABET,
            vec![f(0..20, "container", 0.6), f(4..9, "key", 0.99)],
            0.8,
        );
        assert_eq!(rules(&above), vec!["key"]);
        assert!(
            below.is_empty(),
            "the container overlaps a winner: {below:?}"
        );
    }

    #[test]
    fn a_sub_threshold_finding_clear_of_every_winner_survives() {
        let (above, below) = partition(
            ALPHABET,
            vec![f(0..5, "high", 0.9), f(10..15, "low", 0.2)],
            0.8,
        );
        assert_eq!(rules(&above), vec!["high"]);
        assert_eq!(rules(&below), vec!["low"]);
    }

    #[test]
    fn partitioned_output_is_non_overlapping_across_both_sides() {
        let (above, below) = partition(
            ALPHABET,
            vec![
                f(0..5, "a", 0.9),
                f(3..8, "b", 0.2),
                f(9..12, "c", 0.1),
                f(14..20, "d", 0.95),
            ],
            0.8,
        );
        let mut all: Vec<Finding> = above.into_iter().chain(below).collect();
        all.sort_by_key(|f| f.span.start);
        assert!(all.windows(2).all(|w| w[0].span.end <= w[1].span.start));
        assert_eq!(rules(&all), vec!["a", "c", "d"]);
    }

    #[test]
    fn multibyte_span_on_char_boundaries_survives() {
        // "héllo": h=0, é=1..3, l=3, l=4, o=5.
        let out = detect("héllo", vec![f(1..3, "a", 0.9), f(3..6, "b", 0.9)]);
        assert_eq!(spans(&out), vec![(1, 3), (3, 6)]);
    }

    #[test]
    fn detector_error_propagates() {
        struct Failing;
        impl Detector for Failing {
            fn id(&self) -> &'static str {
                "failing"
            }
            fn detect(&self, _c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
                Err(crate::RedactError::BadPointer("boom".into()))
            }
        }
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![f(0..4, "a", 0.9)])));
        s.push(Box::new(Failing));
        assert!(s.detect_all(&cand("abcdefgh")).is_err());
    }

    #[test]
    fn identical_spans_equal_score_break_on_rule() {
        let out = detect(
            "abcdefgh",
            vec![f(0..4, "zeta", 0.5), f(0..4, "alpha", 0.5)],
        );
        assert_eq!(rules(&out), vec!["alpha"]);
    }

    #[test]
    fn result_is_independent_of_detector_order() {
        let all = vec![
            f(0..5, "a", 0.5),
            f(4..9, "b", 0.9),
            f(8..20, "c", 0.5),
            f(2..6, "d", 0.5),
            f(19..26, "e", 0.7),
        ];
        let expected = detect(ALPHABET, all.clone());

        let mut reversed = all.clone();
        reversed.reverse();
        assert_eq!(detect(ALPHABET, reversed), expected);

        let mut split = DetectorSet::default();
        split.push(Box::new(HostileDetector(all[3..].to_vec())));
        split.push(Box::new(HostileDetector(all[..3].to_vec())));
        assert_eq!(split.detect_all(&cand(ALPHABET)).unwrap(), expected);
    }

    #[allow(clippy::reversed_empty_ranges)]
    #[test]
    fn output_spans_never_overlap() {
        let out = detect(
            ALPHABET,
            vec![
                f(0..3, "a", 0.1),
                f(1..9, "b", 0.2),
                f(2..4, "c", 0.9),
                f(8..12, "d", 0.5),
                f(11..11, "e", 0.5),
                f(12..26, "g", 0.4),
                f(25..99, "h", 0.9),
                f(3..2, "i", 0.9),
            ],
        );
        assert!(out.windows(2).all(|w| w[0].span.end <= w[1].span.start));
        assert!(!out.is_empty());
    }

    #[test]
    fn fixed_detector_reports_its_findings() {
        let mut s = DetectorSet::default();
        s.push(Box::new(FixedDetector(vec![Finding {
            span: 2..5,
            rule: "canned".into(),
            score: 1.0,
            detector: "fixed",
        }])));
        assert_eq!(s.ids(), vec!["fixed"]);
        let out = s.detect_all(&cand("abcdefgh")).unwrap();
        assert_eq!(spans(&out), vec![(2, 5)]);
    }

    // `drops_mid_codepoint_spans` only ever exercises the `end` boundary
    // check, because its span starts at 0. "é" occupies bytes 0..2, so 1 is
    // an invalid start and 3 is a valid end.
    #[test]
    fn drops_span_starting_mid_codepoint() {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![f(1..3, "x", 0.9)])));
        assert!(s.detect_all(&cand("é-tail")).unwrap().is_empty());
    }

    #[test]
    fn equal_length_equal_score_leftmost_wins() {
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![
            f(4..6, "a", 0.1),
            f(3..5, "b", 0.1),
        ])));
        let out = s.detect_all(&cand("abcdefgh")).unwrap();
        assert_eq!(spans(&out), vec![(3, 5)]);
        assert_eq!(out[0].rule, "b");
    }

    #[test]
    fn distinct_detectors_break_ties_on_detector_id() {
        struct Alpha;
        impl Detector for Alpha {
            fn id(&self) -> &'static str {
                "alpha"
            }
            fn detect(&self, _c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
                Ok(vec![Finding {
                    span: 0..4,
                    rule: "same".into(),
                    score: 0.5,
                    detector: "alpha",
                }])
            }
        }
        let mut s = DetectorSet::default();
        s.push(Box::new(HostileDetector(vec![f(0..4, "same", 0.5)])));
        s.push(Box::new(Alpha));
        let out = s.detect_all(&cand("abcdefgh")).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].detector, "alpha");
    }
}
