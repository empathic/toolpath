//! The reviewable artifact between detection and rewriting.

use crate::{detect::FieldShape, transform::Transform};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Redact,
    Skip,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanFinding {
    pub id: String,
    pub step: String,
    pub at: String,
    pub rule: String,
    pub span: (usize, usize),
    pub score: f32,
    pub detector: String,
    pub shape: FieldShape,
    /// Surrounding line with the match replaced by its rule name. Never
    /// the value, never its length, unless `reveal` was set.
    pub context: String,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanDefaults {
    pub transform: Transform,
    pub threshold: f32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Plan {
    pub v: u32,
    pub document: String,
    pub generated: DateTime<Utc>,
    pub detectors: Vec<String>,
    pub defaults: PlanDefaults,
    pub surfaces: Vec<crate::surface::Surface>,
    pub findings: Vec<PlanFinding>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub predicate: Predicate,
    pub action: Action,
    pub transform: Option<Transform>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    Rule(String),
    Shape(FieldShape),
    Step(String),
    Detector(String),
    AtPrefix(String),
    Score(Cmp, f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
}

/// Persisted in the sync manifest so a re-derive can replay redaction.
/// Rule-based only: individual finding ids cannot be replayed against
/// content that has moved.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RedactionPolicy {
    pub detectors: Vec<String>,
    pub threshold: f32,
    pub mode: Transform,
    #[serde(default)]
    pub mode_for: Vec<(String, Transform)>,
    #[serde(default)]
    pub accept: Vec<String>,
    #[serde(default)]
    pub reject: Vec<String>,
    pub key_id: String,
}

// ── Plan machinery (T5) ────────────────────────────────────────────────

pub fn parse_predicate(s: &str) -> crate::Result<Predicate> {
    // Longest operators first, or `score>=0.95` splits on the bare `>` and
    // leaves a literal `=0.95` for the value parser to choke on.
    for (op, cmp) in [
        (">=", Cmp::Ge),
        ("<=", Cmp::Le),
        (">", Cmp::Gt),
        ("<", Cmp::Lt),
    ] {
        if let Some((k, v)) = s.split_once(op)
            && k.trim() == "score"
        {
            let value: f32 = v.trim().parse().map_err(|_| bad_predicate(s))?;
            return Ok(Predicate::Score(cmp, value));
        }
    }

    let (k, v) = s.split_once('=').ok_or_else(|| bad_predicate(s))?;
    let (k, v) = (k.trim(), v.trim());
    if v.is_empty() {
        return Err(bad_predicate(s));
    }
    Ok(match k {
        "rule" => Predicate::Rule(v.to_string()),
        "shape" => Predicate::Shape(parse_shape(v)?),
        "step" => Predicate::Step(v.to_string()),
        "detector" => Predicate::Detector(v.to_string()),
        "at" => Predicate::AtPrefix(v.to_string()),
        "score" => Predicate::Score(Cmp::Eq, v.parse().map_err(|_| bad_predicate(s))?),
        other => {
            return Err(crate::RedactError::BadPredicate(format!(
                "unknown field {other:?} in {s:?}"
            )));
        }
    })
}

fn parse_shape(s: &str) -> crate::Result<FieldShape> {
    Ok(match s {
        "prose" => FieldShape::Prose,
        "tool_input" => FieldShape::ToolInput,
        "tool_output" => FieldShape::ToolOutput,
        "unified_diff" => FieldShape::UnifiedDiff,
        "file_content" => FieldShape::FileContent,
        "uri" => FieldShape::Uri,
        "opaque_json" => FieldShape::OpaqueJson,
        other => {
            return Err(crate::RedactError::BadPredicate(format!(
                "unknown shape {other:?}"
            )));
        }
    })
}

fn bad_predicate(s: &str) -> crate::RedactError {
    crate::RedactError::BadPredicate(format!("not a valid predicate: {s:?}"))
}

/// Whether a finding satisfies a single predicate clause.
pub fn matches(p: &Predicate, f: &PlanFinding) -> bool {
    match p {
        Predicate::Rule(r) => &f.rule == r,
        Predicate::Shape(shape) => f.shape == *shape,
        Predicate::Step(s) => &f.step == s,
        Predicate::Detector(d) => &f.detector == d,
        Predicate::AtPrefix(prefix) => f.at.starts_with(prefix.as_str()),
        Predicate::Score(cmp, v) => match cmp {
            Cmp::Ge => f.score >= *v,
            Cmp::Gt => f.score > *v,
            Cmp::Le => f.score <= *v,
            Cmp::Lt => f.score < *v,
            Cmp::Eq => f.score == *v,
        },
    }
}

/// Later decisions override earlier ones, so a caller can express
/// "redact everything, except this" by ordering.
pub fn apply_decisions(plan: &mut Plan, decisions: &[Decision]) {
    for finding in &mut plan.findings {
        if let Some(d) = decisions
            .iter()
            .rev()
            .find(|d| matches(&d.predicate, finding))
        {
            finding.action = d.action;
            finding.transform = d.transform;
        }
    }
}

/// Stable, ordinal finding id (`f01`, `f02`, …). Stability is what makes a
/// regenerated plan byte-identical to its predecessor.
pub fn finding_id(index: usize) -> String {
    format!("f{:02}", index + 1)
}

/// The line around `span` with the match replaced by `<rule>` - never the
/// value and never anything from which its length can be read, unless
/// `reveal` was set.
pub fn elide_context(text: &str, span: std::ops::Range<usize>, rule: &str, reveal: bool) -> String {
    let line_start = text[..span.start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[span.end..]
        .find('\n')
        .map_or(text.len(), |i| span.end + i);
    let replacement = if reveal {
        text[span.start..span.end].to_string()
    } else {
        format!("<{rule}>")
    };
    format!(
        "{}{replacement}{}",
        &text[line_start..span.start],
        &text[span.end..line_end]
    )
}

/// Refuse a plan that no longer describes this document, naming the first
/// divergence.
///
/// Takes `path` by `&mut` (rather than the `&Path` the rest of this
/// function's job would suggest) because `SurfaceCursor` (T2) needs
/// exclusive access to resolve a pointer to text; `verify` itself never
/// mutates anything through it.
pub fn verify(plan: &Plan, path: &mut toolpath::v1::Path) -> crate::Result<()> {
    if plan.document != path.path.id {
        return Err(crate::RedactError::PlanMismatch(format!(
            "plan targets document {:?}, but path.id is {:?}",
            plan.document, path.path.id
        )));
    }

    let step_ids: std::collections::HashSet<String> =
        path.steps.iter().map(|s| s.step.id.clone()).collect();

    let cursor = crate::surface::SurfaceCursor { path };
    for finding in &plan.findings {
        if !finding.step.is_empty() && !step_ids.contains(&finding.step) {
            return Err(crate::RedactError::PlanMismatch(format!(
                "{}: step {:?} no longer exists",
                finding.id, finding.step
            )));
        }

        let current = cursor.read(&finding.step, &finding.at).ok_or_else(|| {
            crate::RedactError::PlanMismatch(format!(
                "{}: {} no longer resolves",
                finding.id, finding.at
            ))
        })?;

        let (start, end) = finding.span;
        let lands = end <= current.len()
            && current.is_char_boundary(start)
            && current.is_char_boundary(end);
        if !lands {
            return Err(crate::RedactError::PlanMismatch(format!(
                "{}: recorded span {start}..{end} no longer lands inside {}",
                finding.id, finding.at
            )));
        }
    }
    Ok(())
}

// ── Plan generation (T8) ───────────────────────────────────────────────

pub fn generate(
    _path: &toolpath::v1::Path,
    _detectors: &crate::detect::DetectorSet,
    _cfg: &crate::RedactConfig,
) -> Plan {
    todo!("T8")
}

/// `generate`, plus the egress check: a detector that would send candidate
/// material off the machine is refused unless the caller allowed it.
pub fn generate_checked(
    _path: &toolpath::v1::Path,
    _detectors: &crate::detect::DetectorSet,
    _cfg: &crate::RedactConfig,
    _allow_network: bool,
) -> crate::Result<Plan> {
    todo!("T8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use toolpath::v1::{ArtifactChange, Path, PathIdentity, Step, StepIdentity, StructuralChange};

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-30T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn sample_finding(id: &str, rule: &str, score: f32) -> PlanFinding {
        PlanFinding {
            id: id.to_string(),
            step: "step-1".to_string(),
            at: "/change/convo/structural/extra/text".to_string(),
            rule: rule.to_string(),
            span: (0, 4),
            score,
            detector: "internal".to_string(),
            shape: FieldShape::Prose,
            context: "<rule> context".to_string(),
            action: Action::Redact,
            transform: None,
        }
    }

    fn sample_plan(findings: Vec<PlanFinding>) -> Plan {
        Plan {
            v: 1,
            document: "doc-1".to_string(),
            generated: fixed_now(),
            detectors: vec!["internal".to_string()],
            defaults: PlanDefaults {
                transform: Transform::Marker,
                threshold: 0.8,
            },
            surfaces: vec![],
            findings,
        }
    }

    fn decision(pred: &str, action: Action) -> Decision {
        Decision {
            predicate: parse_predicate(pred).unwrap(),
            action,
            transform: None,
        }
    }

    /// One step whose `conversation.append` text field is `text`, addressable
    /// at `/change/<artifact_key>/structural/extra/text` - the pointer shape
    /// `surfaces()` (T2) assigns to that field.
    fn fixture_path_with_text(doc_id: &str, step_id: &str, artifact_key: &str, text: &str) -> Path {
        let mut extra = HashMap::new();
        extra.insert(
            "text".to_string(),
            serde_json::Value::String(text.to_string()),
        );

        let mut change = HashMap::new();
        change.insert(
            artifact_key.to_string(),
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "conversation.append".to_string(),
                    extra,
                }),
            },
        );

        let step = Step {
            step: StepIdentity {
                id: step_id.to_string(),
                parents: vec![],
                actor: "human:t".to_string(),
                timestamp: "2026-07-30T00:00:00Z".to_string(),
            },
            change,
            meta: None,
        };

        Path {
            path: PathIdentity {
                id: doc_id.to_string(),
                base: None,
                head: step_id.to_string(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        }
    }

    // ── parse_predicate ─────────────────────────────────────────────────

    #[test]
    fn parses_every_predicate_field() {
        assert!(matches!(
            parse_predicate("rule=aws-access-key-id").unwrap(),
            Predicate::Rule(_)
        ));
        assert!(matches!(
            parse_predicate("shape=unified_diff").unwrap(),
            Predicate::Shape(_)
        ));
        assert!(matches!(
            parse_predicate("step=turn-0f3a").unwrap(),
            Predicate::Step(_)
        ));
        assert!(matches!(
            parse_predicate("detector=internal").unwrap(),
            Predicate::Detector(_)
        ));
        assert!(matches!(
            parse_predicate("at=/change/x").unwrap(),
            Predicate::AtPrefix(_)
        ));
        assert!(matches!(
            parse_predicate("score>=0.95").unwrap(),
            Predicate::Score(Cmp::Ge, _)
        ));
    }

    #[test]
    fn rejects_anything_else_clearly() {
        let e = parse_predicate("colour=red").unwrap_err().to_string();
        assert!(e.contains("colour"), "error should name the bad field: {e}");
    }

    #[test]
    fn ge_is_not_confused_with_gt() {
        match parse_predicate("score>=0.95").unwrap() {
            Predicate::Score(Cmp::Ge, v) => assert_eq!(v, 0.95),
            other => panic!("expected Score(Ge, 0.95), got {other:?}"),
        }
    }

    #[test]
    fn le_is_not_confused_with_lt() {
        match parse_predicate("score<=0.3").unwrap() {
            Predicate::Score(Cmp::Le, v) => assert_eq!(v, 0.3),
            other => panic!("expected Score(Le, 0.3), got {other:?}"),
        }
    }

    #[test]
    fn gt_and_lt_parse_without_the_equals_form() {
        assert!(matches!(
            parse_predicate("score>0.5").unwrap(),
            Predicate::Score(Cmp::Gt, _)
        ));
        assert!(matches!(
            parse_predicate("score<0.5").unwrap(),
            Predicate::Score(Cmp::Lt, _)
        ));
    }

    #[test]
    fn rejects_missing_operator() {
        assert!(parse_predicate("just-some-text-with-no-operator").is_err());
    }

    #[test]
    fn rejects_empty_value() {
        assert!(parse_predicate("rule=").is_err());
    }

    #[test]
    fn rejects_non_numeric_score() {
        assert!(parse_predicate("score=not-a-number").is_err());
    }

    #[test]
    fn rejects_unknown_shape_name() {
        assert!(parse_predicate("shape=not-a-shape").is_err());
    }

    #[test]
    fn trims_whitespace_around_both_sides() {
        assert_eq!(
            parse_predicate("  rule = aws-access-key-id  ").unwrap(),
            Predicate::Rule("aws-access-key-id".to_string())
        );
        assert_eq!(
            parse_predicate("  score >= 0.5  ").unwrap(),
            Predicate::Score(Cmp::Ge, 0.5)
        );
    }

    // ── matches ──────────────────────────────────────────────────────────

    #[test]
    fn every_cmp_variant_compares_correctly() {
        let f = |score| sample_finding("f01", "r", score);
        assert!(matches(&Predicate::Score(Cmp::Ge, 0.5), &f(0.5)));
        assert!(matches(&Predicate::Score(Cmp::Ge, 0.5), &f(0.6)));
        assert!(!matches(&Predicate::Score(Cmp::Ge, 0.5), &f(0.4)));

        assert!(matches(&Predicate::Score(Cmp::Gt, 0.5), &f(0.6)));
        assert!(!matches(&Predicate::Score(Cmp::Gt, 0.5), &f(0.5)));

        assert!(matches(&Predicate::Score(Cmp::Le, 0.5), &f(0.5)));
        assert!(matches(&Predicate::Score(Cmp::Le, 0.5), &f(0.4)));
        assert!(!matches(&Predicate::Score(Cmp::Le, 0.5), &f(0.6)));

        assert!(matches(&Predicate::Score(Cmp::Lt, 0.5), &f(0.4)));
        assert!(!matches(&Predicate::Score(Cmp::Lt, 0.5), &f(0.5)));

        assert!(matches(&Predicate::Score(Cmp::Eq, 0.5), &f(0.5)));
        assert!(!matches(&Predicate::Score(Cmp::Eq, 0.5), &f(0.500_001)));
    }

    #[test]
    fn rule_shape_step_detector_match_exactly() {
        let f = sample_finding("f01", "aws-access-key-id", 0.9);
        assert!(matches(
            &Predicate::Rule("aws-access-key-id".to_string()),
            &f
        ));
        assert!(!matches(&Predicate::Rule("other".to_string()), &f));
        assert!(matches(&Predicate::Shape(FieldShape::Prose), &f));
        assert!(!matches(&Predicate::Shape(FieldShape::UnifiedDiff), &f));
        assert!(matches(&Predicate::Step("step-1".to_string()), &f));
        assert!(matches(&Predicate::Detector("internal".to_string()), &f));
    }

    #[test]
    fn at_prefix_matches_prefix_not_substring_or_equality() {
        let p = Predicate::AtPrefix("/change/x".to_string());
        let make = |at: &str| PlanFinding {
            at: at.to_string(),
            ..sample_finding("f01", "r", 0.9)
        };

        assert!(
            matches(&p, &make("/change/x/structural/extra/text")),
            "prefix should match"
        );
        assert!(
            matches(&p, &make("/change/x")),
            "exact equality is a trivial prefix match"
        );
        assert!(
            !matches(&p, &make("nested/change/x/structural")),
            "substring elsewhere in the string must not match"
        );
        assert!(
            !matches(&p, &make("/change/")),
            "a shorter string cannot have a longer prefix"
        );
    }

    // ── apply_decisions ──────────────────────────────────────────────────

    #[test]
    fn last_matching_decision_wins() {
        let mut plan = sample_plan(vec![sample_finding("f01", "aws-access-key-id", 0.99)]);
        apply_decisions(
            &mut plan,
            &[
                decision("rule=aws-access-key-id", Action::Redact),
                decision("score>=0.9", Action::Skip),
            ],
        );
        assert_eq!(plan.findings[0].action, Action::Skip);
    }

    #[test]
    fn apply_decisions_applies_the_winning_decisions_transform() {
        let mut plan = sample_plan(vec![sample_finding("f01", "us-ssn", 0.9)]);
        apply_decisions(
            &mut plan,
            &[Decision {
                predicate: parse_predicate("rule=us-ssn").unwrap(),
                action: Action::Redact,
                transform: Some(Transform::Mask),
            }],
        );
        assert_eq!(plan.findings[0].transform, Some(Transform::Mask));
    }

    #[test]
    fn apply_decisions_leaves_non_matching_findings_untouched() {
        let mut plan = sample_plan(vec![sample_finding("f01", "us-ssn", 0.9)]);
        let original_action = plan.findings[0].action;
        apply_decisions(&mut plan, &[decision("rule=other-rule", Action::Skip)]);
        assert_eq!(plan.findings[0].action, original_action);
    }

    // ── finding_id ───────────────────────────────────────────────────────

    #[test]
    fn finding_id_zero_padded_then_grows_without_collision() {
        assert_eq!(finding_id(0), "f01");
        assert_eq!(finding_id(8), "f09");
        assert_eq!(finding_id(98), "f99");
        assert_eq!(finding_id(99), "f100");
        assert_eq!(finding_id(100), "f101");
    }

    // ── elide_context ────────────────────────────────────────────────────

    #[test]
    fn elide_context_never_carries_the_value_or_its_length() {
        let value = "AKIAIOSFODNN7REALKEY";
        assert_eq!(value.len(), 20);
        let text = format!("key: {value}\n");
        let start = text.find(value).unwrap();
        let out = elide_context(
            &text,
            start..start + value.len(),
            "aws-access-key-id",
            false,
        );
        assert!(!out.contains(value));
        assert!(
            !out.contains("20"),
            "the value's length must not leak either: {out}"
        );
        assert!(out.contains("<aws-access-key-id>"));
        assert_eq!(out, "key: <aws-access-key-id>");
    }

    #[test]
    fn elide_context_spans_the_whole_line() {
        let text = "before\nSECRETVALUE\nafter";
        let start = text.find("SECRETVALUE").unwrap();
        let out = elide_context(text, start..start + "SECRETVALUE".len(), "rule", false);
        assert_eq!(out, "<rule>");
    }

    #[test]
    fn elide_context_at_the_very_start_of_the_text() {
        let text = "SECRETfoo bar\nnext line";
        let out = elide_context(text, 0.."SECRET".len(), "rule", false);
        assert_eq!(out, "<rule>foo bar");
    }

    #[test]
    fn elide_context_at_the_very_end_of_the_text() {
        let text = "prefix line\nend SECRETEND";
        let start = text.find("SECRETEND").unwrap();
        let out = elide_context(text, start..start + "SECRETEND".len(), "rule", false);
        assert_eq!(out, "end <rule>");
    }

    #[test]
    fn elide_context_handles_multibyte_text() {
        let text = "héllo wörld\nsécret：dröp\nmore lïnes";
        let needle = "dröp";
        let start = text.find(needle).unwrap();
        let out = elide_context(text, start..start + needle.len(), "rule", false);
        assert_eq!(out, "sécret：<rule>");
    }

    #[test]
    fn elide_context_with_no_newline_at_all() {
        let text = "just one line with a SECRET in it";
        let start = text.find("SECRET").unwrap();
        let out = elide_context(text, start..start + "SECRET".len(), "rule", false);
        assert_eq!(out, "just one line with a <rule> in it");
    }

    #[test]
    fn elide_context_reveal_includes_the_value() {
        let value = "AKIAIOSFODNN7REALKEY";
        let text = format!("key: {value}\n");
        let start = text.find(value).unwrap();
        let out = elide_context(&text, start..start + value.len(), "aws-access-key-id", true);
        assert!(out.contains(value));
    }

    // ── verify ───────────────────────────────────────────────────────────
    //
    // These exercise `verify` through `SurfaceCursor::read` (T2), which is
    // still `todo!()` as of this writing - see the report for status.

    #[test]
    fn verify_passes_on_an_unmodified_document() {
        let text = "hello world, this is prose";
        let mut path = fixture_path_with_text("doc-1", "step-1", "convo", text);
        let start = text.find("world").unwrap();
        let plan = sample_plan(vec![PlanFinding {
            span: (start, start + "world".len()),
            ..sample_finding("f01", "some-rule", 0.9)
        }]);
        assert!(verify(&plan, &mut path).is_ok());
    }

    #[test]
    fn verify_fails_naming_the_finding_whose_span_no_longer_lands() {
        let text = "hello world, this is prose";
        let start = text.find("world").unwrap();
        let plan = sample_plan(vec![PlanFinding {
            span: (start, start + "world".len()),
            ..sample_finding("f01", "some-rule", 0.9)
        }]);

        let mut mutated = fixture_path_with_text("doc-1", "step-1", "convo", "short");
        let e = verify(&plan, &mut mutated).unwrap_err().to_string();
        assert!(e.contains("f01"), "should name the first divergence: {e}");
    }

    #[test]
    fn verify_fails_when_a_step_disappears() {
        let plan = sample_plan(vec![PlanFinding {
            step: "step-missing".to_string(),
            span: (0, 5),
            ..sample_finding("f01", "some-rule", 0.9)
        }]);
        let mut path = fixture_path_with_text("doc-1", "step-1", "convo", "hello world");
        let e = verify(&plan, &mut path).unwrap_err().to_string();
        assert!(e.contains("f01"));
    }

    #[test]
    fn verify_fails_when_the_document_id_differs() {
        let plan = sample_plan(vec![]);
        let mut path = fixture_path_with_text("different-doc", "step-1", "convo", "text");
        assert!(verify(&plan, &mut path).is_err());
    }

    // ── serde round-trip ─────────────────────────────────────────────────

    #[test]
    fn plan_round_trips_through_json() {
        let plan = Plan {
            v: 1,
            document: "doc-1".to_string(),
            generated: fixed_now(),
            detectors: vec!["internal".to_string(), "gitleaks".to_string()],
            defaults: PlanDefaults {
                transform: Transform::Mask,
                threshold: 0.75,
            },
            surfaces: vec![crate::surface::Surface {
                step: "step-1".to_string(),
                at: "/change/convo/structural/extra/text".to_string(),
                shape: FieldShape::Prose,
                bytes: 42,
            }],
            findings: vec![PlanFinding {
                span: (10, 30),
                score: 0.97,
                context: "<aws-access-key-id> is the key".to_string(),
                transform: Some(Transform::Hash),
                ..sample_finding("f01", "aws-access-key-id", 0.97)
            }],
        };

        let json = serde_json::to_string(&plan).unwrap();
        let round: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, round);
        assert_eq!(json, serde_json::to_string(&round).unwrap());
    }
}
