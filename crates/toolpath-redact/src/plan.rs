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
    path: &toolpath::v1::Path,
    detectors: &crate::detect::DetectorSet,
    cfg: &crate::RedactConfig,
) -> Plan {
    // This signature is fixed since T0 and cannot return `Result` (T5's own
    // byte-identity test calls it directly, unwrapped). A failing detector
    // is a bug in that detector, not a normal outcome, so it surfaces as a
    // panic here instead of silently degrading to an empty plan;
    // `generate_checked` propagates the same failure through `Result`.
    generate_inner(path, detectors, cfg).expect("detector failed while generating a redaction plan")
}

/// `generate`, plus the egress check: a detector that would send candidate
/// material off the machine is refused unless the caller allowed it.
pub fn generate_checked(
    path: &toolpath::v1::Path,
    detectors: &crate::detect::DetectorSet,
    cfg: &crate::RedactConfig,
    allow_network: bool,
) -> crate::Result<Plan> {
    if !allow_network
        && let Some(d) = detectors
            .detectors()
            .iter()
            .find(|d| d.egress() == crate::detect::Egress::Network)
    {
        return Err(crate::RedactError::NetworkDetectorRefused(
            d.id().to_string(),
        ));
    }
    generate_inner(path, detectors, cfg)
}

fn generate_inner(
    path: &toolpath::v1::Path,
    detectors: &crate::detect::DetectorSet,
    cfg: &crate::RedactConfig,
) -> crate::Result<Plan> {
    let surfaces = crate::surface::surfaces(path);

    // `SurfaceCursor` needs `&mut Path` (T2 uses the same struct for
    // writes); `generate` only takes `&Path` since `verify` re-checks
    // findings against the caller's own document later, so read through a
    // throwaway clone instead.
    let mut scratch = path.clone();
    let cursor = crate::surface::SurfaceCursor { path: &mut scratch };

    // `surfaces()` already visits steps in document order and sorts
    // artifacts/fields (see its own determinism test), and `detect_all`
    // returns spans sorted by start - so findings collected in this order
    // already satisfy the (step, pointer, span start) ordering `finding_id`
    // relies on, with no extra sort needed here.
    let mut findings = Vec::new();
    for s in &surfaces {
        let Some(text) = cursor.read(&s.step, &s.at) else {
            continue;
        };
        let ctx = context_for(path, &s.step, &s.at);
        let candidate = crate::detect::Candidate {
            text: &text,
            shape: s.shape,
            at: &s.at,
            ctx,
        };
        for finding in detectors.detect_all(&candidate)? {
            let action = if finding.score < cfg.threshold {
                Action::Skip
            } else {
                Action::Redact
            };
            let context = elide_context(&text, finding.span.clone(), &finding.rule, cfg.reveal);
            findings.push(PlanFinding {
                id: String::new(), // assigned below, once findings are in final order
                step: s.step.clone(),
                at: s.at.clone(),
                span: (finding.span.start, finding.span.end),
                rule: finding.rule,
                score: finding.score,
                detector: finding.detector.to_string(),
                shape: s.shape,
                context,
                action,
                transform: None,
            });
        }
    }
    for (i, finding) in findings.iter_mut().enumerate() {
        finding.id = finding_id(i);
    }

    Ok(Plan {
        v: 1,
        document: path.path.id.clone(),
        generated: cfg.now,
        detectors: detectors.ids().into_iter().map(String::from).collect(),
        defaults: PlanDefaults {
            transform: cfg.mode,
            threshold: cfg.threshold,
        },
        surfaces,
        findings,
    })
}

/// `change_type`/`actor` come from the step and artifact the pointer names.
/// `tool_name` stays `None`: resolving it would mean re-parsing the same
/// `tool_uses` JSON `surfaces()` already walked once, and no detector in
/// this crate reads it yet.
fn context_for<'a>(
    path: &'a toolpath::v1::Path,
    step_id: &str,
    at: &str,
) -> crate::detect::Context<'a> {
    let kind = path.meta.as_ref().and_then(|m| m.kind.as_deref());
    let Some(step) = path.steps.iter().find(|s| s.step.id == step_id) else {
        return crate::detect::Context {
            change_type: "",
            tool_name: None,
            actor: "",
            kind,
        };
    };
    let change_type = artifact_key_from_at(at)
        .and_then(|key| step.change.get(&key))
        .and_then(|c| c.structural.as_ref())
        .map(|s| s.change_type.as_str())
        .unwrap_or("");
    crate::detect::Context {
        change_type,
        tool_name: None,
        actor: step.step.actor.as_str(),
        kind,
    }
}

/// Recovers the artifact key a `/change/...` pointer names. Mirrors
/// `surface::ptr_decode` (private to that module) - decode `~1` before
/// `~0`, or `~01` round-trips wrong (RFC 6901).
fn artifact_key_from_at(at: &str) -> Option<String> {
    let token = at.strip_prefix("/change/")?.split('/').next()?;
    Some(token.replace("~1", "/").replace("~0", "~"))
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

#[cfg(test)]
mod plan_gen {
    use super::*;
    use crate::detect::{Candidate, Detector, DetectorSet, Egress, Finding, FixedDetector};
    use chrono::TimeZone;
    use std::collections::HashMap;
    use std::ops::Range;
    use toolpath::v1::{ArtifactChange, Path, PathIdentity, Step, StepIdentity, StructuralChange};

    fn cfg() -> crate::RedactConfig {
        crate::RedactConfig {
            threshold: 0.8,
            mode: Transform::Marker,
            mode_for: Vec::new(),
            key: b"test-key".to_vec(),
            now: Utc.with_ymd_and_hms(2026, 7, 30, 0, 0, 0).unwrap(),
            drop_signatures: false,
            reveal: false,
        }
    }

    fn f(span: Range<usize>, rule: &str, score: f32) -> Finding {
        Finding {
            span,
            rule: rule.into(),
            score,
            detector: "fixed",
        }
    }

    /// Matches the literal substring `SECRET-VALUE`, so `fixture_mixed`
    /// (below) can put a finding on one surface and leave a sibling clean.
    struct Needle;
    impl Detector for Needle {
        fn id(&self) -> &'static str {
            "needle"
        }
        fn detect(&self, c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
            Ok(c.text
                .match_indices("SECRET-VALUE")
                .map(|(i, m)| Finding {
                    span: i..i + m.len(),
                    rule: "test-secret".into(),
                    score: 0.95,
                    detector: "needle",
                })
                .collect())
        }
    }

    fn detectors() -> DetectorSet {
        let mut set = DetectorSet::default();
        set.push(Box::new(Needle));
        set
    }

    struct NetworkDetector;
    impl Detector for NetworkDetector {
        fn id(&self) -> &'static str {
            "network"
        }
        fn detect(&self, _c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
            Ok(Vec::new())
        }
        fn egress(&self) -> Egress {
            Egress::Network
        }
    }

    struct FailingDetector;
    impl Detector for FailingDetector {
        fn id(&self) -> &'static str {
            "failing"
        }
        fn detect(&self, _c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
            Err(crate::RedactError::BadPointer("boom".into()))
        }
    }

    fn step_with_text(id: &str, artifact: &str, text: &str) -> Step {
        let mut extra = HashMap::new();
        extra.insert(
            "text".to_string(),
            serde_json::Value::String(text.to_string()),
        );
        let mut change = HashMap::new();
        change.insert(
            artifact.to_string(),
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "conversation.append".to_string(),
                    extra,
                }),
            },
        );
        Step {
            step: StepIdentity {
                id: id.to_string(),
                parents: Vec::new(),
                actor: "human:t".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
            },
            change,
            meta: None,
        }
    }

    fn path_of(steps: Vec<Step>) -> Path {
        let head = steps.last().map(|s| s.step.id.clone()).unwrap_or_default();
        Path {
            path: PathIdentity {
                id: "p1".to_string(),
                base: None,
                head,
                graph_ref: None,
            },
            steps,
            meta: None,
        }
    }

    /// The artifact key is one byte, so the always-present `/change/a`
    /// surface (whose text is the key itself) is too short for a `0..20`
    /// span - only the `text` field can win the merge test below.
    fn fixture_one_field() -> Path {
        path_of(vec![step_with_text("s1", "a", "AAAAAAAAAAAAAAAAAAAA")])
    }

    fn fixture_mixed() -> Path {
        path_of(vec![
            step_with_text("s1", "a", "here is a SECRET-VALUE to find"),
            step_with_text("s2", "b", "nothing interesting here"),
        ])
    }

    fn findings_at(plan: &Plan, at: &str) -> usize {
        plan.findings.iter().filter(|pf| pf.at == at).count()
    }

    // ── Step 8.1, verbatim ────────────────────────────────────────────────

    #[test]
    fn surfaces_and_findings_are_both_populated() {
        let plan = generate(&fixture_mixed(), &detectors(), &cfg());
        assert!(plan.surfaces.iter().any(|s| findings_at(&plan, &s.at) == 0));
        assert!(!plan.findings.is_empty());
    }

    #[test]
    fn two_detectors_merge_through_one_resolution() {
        let mut set = DetectorSet::default();
        set.push(Box::new(FixedDetector(vec![f(0..20, "a", 0.9)])));
        set.push(Box::new(FixedDetector(vec![f(0..20, "b", 0.5)])));
        assert_eq!(
            generate(&fixture_one_field(), &set, &cfg()).findings.len(),
            1
        );
    }

    #[test]
    fn network_detector_is_refused_without_the_flag() {
        let mut set = DetectorSet::default();
        set.push(Box::new(NetworkDetector));
        assert!(matches!(
            generate_checked(&fixture_one_field(), &set, &cfg(), false),
            Err(crate::RedactError::NetworkDetectorRefused(_))
        ));
    }

    // ── Coverage a reviewer would demand ────────────────────────────────

    #[test]
    fn zero_finding_surface_still_appears_in_plan_surfaces() {
        let plan = generate(&fixture_one_field(), &DetectorSet::default(), &cfg());
        assert!(!plan.surfaces.is_empty());
        assert!(plan.findings.is_empty());
    }

    #[test]
    fn plan_detectors_lists_the_detector_ids_actually_run() {
        let mut set = DetectorSet::default();
        set.push(Box::new(FixedDetector(Vec::new())));
        set.push(Box::new(Needle));
        let plan = generate(&fixture_one_field(), &set, &cfg());
        assert_eq!(
            plan.detectors,
            vec!["fixed".to_string(), "needle".to_string()]
        );
    }

    #[test]
    fn score_exactly_at_threshold_is_redact_not_skip() {
        let mut set = DetectorSet::default();
        set.push(Box::new(FixedDetector(vec![f(0..20, "boundary", 0.8)])));
        let plan = generate(&fixture_one_field(), &set, &cfg());
        assert_eq!(plan.findings[0].action, Action::Redact);
    }

    #[test]
    fn score_just_below_threshold_is_skip() {
        let mut set = DetectorSet::default();
        set.push(Box::new(FixedDetector(vec![f(0..20, "boundary", 0.799)])));
        let plan = generate(&fixture_one_field(), &set, &cfg());
        assert_eq!(plan.findings[0].action, Action::Skip);
    }

    #[test]
    fn reveal_flag_propagates_into_generated_context() {
        let mut set = DetectorSet::default();
        set.push(Box::new(FixedDetector(vec![f(0..20, "boundary", 0.95)])));
        let cfg = crate::RedactConfig {
            reveal: true,
            ..cfg()
        };
        let plan = generate(&fixture_one_field(), &set, &cfg);
        assert!(plan.findings[0].context.contains("AAAAAAAAAAAAAAAAAAAA"));
    }

    #[test]
    fn empty_document_yields_empty_plan_with_configured_id_and_timestamp() {
        let empty = path_of(Vec::new());
        let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
        let cfg = crate::RedactConfig { now, ..cfg() };
        let plan = generate(&empty, &DetectorSet::default(), &cfg);
        assert_eq!(plan.document, "p1");
        assert_eq!(plan.generated, now);
        assert!(plan.surfaces.is_empty());
        assert!(plan.findings.is_empty());
    }

    #[test]
    fn regenerating_a_plan_yields_byte_identical_json() {
        let path = fixture_mixed();
        let set = detectors();
        let cfg = cfg();
        let a = generate(&path, &set, &cfg);
        let b = generate(&path, &set, &cfg);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn detector_error_propagates_through_generate_checked() {
        let mut set = DetectorSet::default();
        set.push(Box::new(FailingDetector));
        let err = generate_checked(&fixture_one_field(), &set, &cfg(), false).unwrap_err();
        assert!(matches!(err, crate::RedactError::BadPointer(_)));
    }
}
