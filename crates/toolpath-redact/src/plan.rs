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
    /// Keyed fingerprint of the bytes `span` covered when the plan was
    /// generated. `verify` recomputes it, which is the only thing standing
    /// between a mutated document and a marker spliced over the wrong text:
    /// a same-length edit inside a recorded span leaves every offset valid.
    pub fingerprint: String,
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

/// Plan format version. `verify` refuses anything else rather than reading a
/// shape it does not know the invariants of.
const PLAN_V: u32 = 1;

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
        // The `score` guard is load-bearing, not defensive: without it an
        // operator character *inside* a value (`at=/change/a>b`) is read as
        // the predicate's operator.
        if let Some((k, v)) = s.split_once(op)
            && k.trim() == "score"
        {
            return Ok(Predicate::Score(cmp, parse_score(s, v)?));
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
        "score" => Predicate::Score(Cmp::Eq, parse_score(s, v)?),
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

/// Scores are the same 0.0..=1.0 quantity `--threshold` is, so they get the
/// same range check. Left open, `score<=inf` silently matches everything,
/// `score>=nan` silently matches nothing, and neither reads as a mistake in
/// the output.
fn parse_score(s: &str, v: &str) -> crate::Result<f32> {
    let value: f32 = v.trim().parse().map_err(|_| bad_predicate(s))?;
    if !(0.0..=1.0).contains(&value) {
        return Err(crate::RedactError::BadPredicate(format!(
            "score must be within 0.0..=1.0, got {} in {s:?}",
            v.trim()
        )));
    }
    Ok(value)
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
///
/// Action and transform resolve independently. A decision that carries no
/// transform is not a decision to clear one - nothing in the surface can
/// express that - so `--mode-for rule=us-ssn:mask --accept score>=0.9` keeps
/// the mask. Overwriting it would make the run disagree with a replay of the
/// same policy, which reads the per-rule mode back out of the config.
pub fn apply_decisions(plan: &mut Plan, decisions: &[Decision]) {
    for finding in &mut plan.findings {
        let mut action = None;
        let mut transform = None;
        for d in decisions
            .iter()
            .rev()
            .filter(|d| matches(&d.predicate, finding))
        {
            action = action.or(Some(d.action));
            transform = transform.or(d.transform);
            if transform.is_some() {
                break;
            }
        }
        if let Some(action) = action {
            finding.action = action;
        }
        if let Some(transform) = transform {
            finding.transform = Some(transform);
        }
    }
}

/// Stable, ordinal finding id (`f01`, `f02`, …). Stability is what makes a
/// regenerated plan byte-identical to its predecessor.
pub fn finding_id(index: usize) -> String {
    format!("f{:02}", index + 1)
}

/// How much text either side of the match a reviewer gets. A "line" in a
/// tool output or a minified file is not a line: an 8 KB newline-free field
/// otherwise lands in the plan verbatim, carrying whatever else was on it -
/// including the credentials the detectors missed.
const CONTEXT_RADIUS: usize = 40;

/// The text around `span` with the match replaced by `<rule>` - never the
/// value and never anything from which its length can be read, unless
/// `reveal` was set.
///
/// Bounded to `CONTEXT_RADIUS` bytes either side, with `…` marking a cut.
/// `\r` terminates the window along with `\n`: a lone carriage return in a
/// plan a dry run prints rewinds the terminal over the line before it.
///
/// `span` must lie on char boundaries within `text` - `detect::normalise`
/// drops every finding that does not, and this is only ever called on its
/// output.
pub(crate) fn elide_context(
    text: &str,
    span: std::ops::Range<usize>,
    rule: &str,
    reveal: bool,
) -> String {
    let line_start = text[..span.start].rfind(['\n', '\r']).map_or(0, |i| i + 1);
    let line_end = text[span.end..]
        .find(['\n', '\r'])
        .map_or(text.len(), |i| span.end + i);

    let mut start = line_start.max(span.start.saturating_sub(CONTEXT_RADIUS));
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = line_end.min((span.end + CONTEXT_RADIUS).min(text.len()));
    while !text.is_char_boundary(end) {
        end -= 1;
    }

    let replacement = if reveal {
        text[span.start..span.end].to_string()
    } else {
        format!("<{rule}>")
    };
    format!(
        "{}{}{replacement}{}{}",
        if start > line_start { "\u{2026}" } else { "" },
        &text[start..span.start],
        &text[span.end..end],
        if end < line_end { "\u{2026}" } else { "" },
    )
}

/// Refuse a plan that no longer describes this document, naming the first
/// divergence.
///
/// `key` is the redaction key the plan was generated with. Offsets alone
/// prove nothing: a same-length edit inside a recorded span leaves every
/// bound valid, and the marker then lands over text nobody detected. So each
/// finding's fingerprint is recomputed from the bytes actually there now.
/// Reusing the key means a plan verifies only against the document it was
/// generated from, by whoever holds that key.
pub fn verify(plan: &Plan, path: &toolpath::v1::Path, key: &[u8]) -> crate::Result<()> {
    if plan.v != PLAN_V {
        return Err(crate::RedactError::PlanMismatch(format!(
            "plan version {} is not supported (expected {PLAN_V})",
            plan.v
        )));
    }
    if plan.document != path.path.id {
        return Err(crate::RedactError::PlanMismatch(format!(
            "plan targets document {:?}, but path.id is {:?}",
            plan.document, path.path.id
        )));
    }

    let steps: std::collections::HashMap<&str, &toolpath::v1::Step> =
        path.steps.iter().map(|s| (s.step.id.as_str(), s)).collect();

    for (i, finding) in plan.findings.iter().enumerate() {
        // Ids are positional, and `apply` reports by id. A renumbered or
        // reordered list makes every diagnostic name the wrong finding.
        let expected = finding_id(i);
        if finding.id != expected {
            return Err(crate::RedactError::PlanMismatch(format!(
                "finding {i} carries id {:?}, but ids are positional and this one is {expected:?}",
                finding.id
            )));
        }

        let step = steps.get(finding.step.as_str()).copied();
        if !finding.step.is_empty() && step.is_none() {
            return Err(crate::RedactError::PlanMismatch(format!(
                "{}: step {:?} no longer exists",
                finding.id, finding.step
            )));
        }

        let current = crate::surface::read_at_in(path, step, &finding.at).ok_or_else(|| {
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

        // An empty or inverted span holds no value to fingerprint. `apply`
        // refuses both by name, and its diagnostic is the more useful one.
        if start < end
            && crate::transform::Fingerprint::new(key, &current[start..end]).0
                != finding.fingerprint
        {
            return Err(crate::RedactError::PlanMismatch(format!(
                "{}: the text at {} changed since the plan was generated",
                finding.id, finding.at
            )));
        }
    }
    Ok(())
}

// ── Plan generation (T8) ───────────────────────────────────────────────

/// In-crate convenience over `generate_inner` for tests that supply their own
/// detectors and treat a detector failure as a bug in the test. Test-only
/// because it is the panicking form: every shipping caller goes through
/// `generate_checked`, which also runs the egress check.
#[cfg(test)]
pub(crate) fn generate(
    path: &toolpath::v1::Path,
    detectors: &crate::detect::DetectorSet,
    cfg: &crate::RedactConfig,
) -> Plan {
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
    let steps: std::collections::HashMap<&str, &toolpath::v1::Step> =
        path.steps.iter().map(|s| (s.step.id.as_str(), s)).collect();
    let kind = path.meta.as_ref().and_then(|m| m.kind.as_deref());

    // Finding ids are positional over the emission order of `surfaces()`,
    // which is itself deterministic (see its own determinism test), with each
    // surface's own findings sorted by span start. Nothing below reorders, so
    // an id can be assigned at push time.
    let mut findings = Vec::new();
    for s in &surfaces {
        let step = steps.get(s.step.as_str()).copied();
        // `surfaces()` named this field; if it no longer reads, the two
        // disagree about the document and the pass has silently skipped a
        // field it is claiming to have scanned.
        let Some(text) = crate::surface::read_at_in(path, step, &s.at) else {
            return Err(crate::RedactError::BadPointer(format!(
                "{}{}",
                s.step, s.at
            )));
        };
        let candidate = crate::detect::Candidate {
            text: &text,
            shape: s.shape,
            at: &s.at,
            ctx: context_for(kind, step, &s.at),
        };

        // Threshold before overlap resolution, never after: see
        // `detect_all_partitioned`.
        let (above, below) = detectors.detect_all_partitioned(&candidate, cfg.threshold)?;
        let mut resolved: Vec<(crate::detect::Finding, Action)> = above
            .into_iter()
            .map(|f| (f, Action::Redact))
            .chain(below.into_iter().map(|f| (f, Action::Skip)))
            .collect();
        resolved.sort_by_key(|(f, _)| f.span.start);

        for (finding, action) in resolved {
            let context = elide_context(&text, finding.span.clone(), &finding.rule, cfg.reveal);
            findings.push(PlanFinding {
                id: finding_id(findings.len()),
                step: s.step.clone(),
                at: s.at.clone(),
                span: (finding.span.start, finding.span.end),
                fingerprint: crate::transform::Fingerprint::new(&cfg.key, &text[finding.span]).0,
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

    Ok(Plan {
        v: PLAN_V,
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

/// Everything a detector is told about where its candidate came from.
/// `Context` is the whole interface a pluggable detector gets, so every
/// field it declares is resolved: a detector that only fires inside `Bash`
/// input has no other way to know.
fn context_for<'a>(
    kind: Option<&'a str>,
    step: Option<&'a toolpath::v1::Step>,
    at: &str,
) -> crate::detect::Context<'a> {
    let Some(step) = step else {
        return crate::detect::Context {
            change_type: "",
            tool_name: None,
            actor: "",
            kind,
        };
    };
    let structural = artifact_key_from_at(at)
        .and_then(|key| step.change.get(&key))
        .and_then(|c| c.structural.as_ref());
    crate::detect::Context {
        change_type: structural.map(|s| s.change_type.as_str()).unwrap_or(""),
        tool_name: structural.and_then(|s| tool_name_at(&s.extra, at)),
        actor: step.step.actor.as_str(),
        kind,
    }
}

/// Recovers the artifact key a `/change/...` pointer names.
fn artifact_key_from_at(at: &str) -> Option<String> {
    let token = at.strip_prefix("/change/")?.split('/').next()?;
    Some(crate::surface::ptr_decode(token))
}

/// The tool a `/tool_uses/{i}/…` surface sits under, read back out of the
/// step's own extras. Delegated turns nest their own `tool_uses`, so the
/// *last* such segment names the call this pointer belongs to.
fn tool_name_at<'a>(
    extra: &'a std::collections::HashMap<String, serde_json::Value>,
    at: &str,
) -> Option<&'a str> {
    const SEG: &str = "tool_uses/";

    // `StructuralChange::extra` is `#[serde(flatten)]`, so its keys sit
    // directly under `structural` with no `extra` segment of their own.
    let rest = at
        .strip_prefix("/change/")?
        .split_once('/')?
        .1
        .strip_prefix("structural/")?;
    let head = rest
        .rfind(SEG)
        .filter(|i| *i == 0 || rest.as_bytes()[i - 1] == b'/')?;
    let index = rest[head + SEG.len()..].split('/').next()?;

    // `{head}tool_uses/{index}/name`, split the way `surface::route` splits:
    // the first token is the `extra` key, the rest is a pointer into it.
    let leaf = format!("{}{SEG}{index}/name", &rest[..head]);
    let (field, tail) = leaf.split_once('/')?;
    extra
        .get(&crate::surface::ptr_decode(field))?
        .pointer(&format!("/{tail}"))?
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use toolpath::v1::{ArtifactChange, Path, PathIdentity, Step, StepIdentity, StructuralChange};

    const TEST_KEY: &[u8] = b"test-key";

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-30T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn fp(value: &str) -> String {
        crate::transform::Fingerprint::new(TEST_KEY, value).0
    }

    fn sample_finding(id: &str, rule: &str, score: f32) -> PlanFinding {
        PlanFinding {
            id: id.to_string(),
            step: "step-1".to_string(),
            at: "/change/convo/structural/text".to_string(),
            rule: rule.to_string(),
            span: (0, 4),
            score,
            detector: "internal".to_string(),
            shape: FieldShape::Prose,
            context: "<rule> context".to_string(),
            fingerprint: String::new(),
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
    /// at `/change/<artifact_key>/structural/text` - the pointer shape
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
    fn rejects_non_finite_and_out_of_range_scores() {
        // `score>=nan` matches nothing, `score<=inf` matches everything, and
        // `score>=-1` matches everything. All three read as a filter that did
        // not work, so none of them may be accepted silently.
        for s in [
            "score>=nan",
            "score<=inf",
            "score>=-1",
            "score<=2",
            "score=-0.5",
            "score=1.5",
            "score>inf",
            "score=nan",
        ] {
            assert!(parse_predicate(s).is_err(), "{s:?} should be refused");
        }
        assert!(parse_predicate("score>=0").is_ok());
        assert!(parse_predicate("score<=1").is_ok());
    }

    #[test]
    fn an_operator_inside_a_non_score_value_is_not_an_operator() {
        // Without the `score` guard on the operator split, the `>` inside the
        // pointer is read as the predicate's operator and `b` as its value.
        assert_eq!(
            parse_predicate("at=/change/a>b").unwrap(),
            Predicate::AtPrefix("/change/a>b".to_string())
        );
        assert_eq!(
            parse_predicate("rule=a<=b").unwrap(),
            Predicate::Rule("a<=b".to_string())
        );
        assert_eq!(
            parse_predicate("step=x<y").unwrap(),
            Predicate::Step("x<y".to_string())
        );
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
            matches(&p, &make("/change/x/structural/text")),
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
    fn a_later_decision_without_a_transform_keeps_the_earlier_one() {
        // `--mode-for rule=us-ssn:mask --accept score>=0.9`. Clearing the
        // transform here would make the run emit a marker while a replay of
        // the same policy - which reads the per-rule mode back out of the
        // config - emits a mask.
        let mut plan = sample_plan(vec![sample_finding("f01", "us-ssn", 0.95)]);
        apply_decisions(
            &mut plan,
            &[
                Decision {
                    predicate: parse_predicate("rule=us-ssn").unwrap(),
                    action: Action::Redact,
                    transform: Some(Transform::Mask),
                },
                decision("score>=0.9", Action::Redact),
            ],
        );
        assert_eq!(plan.findings[0].action, Action::Redact);
        assert_eq!(plan.findings[0].transform, Some(Transform::Mask));
    }

    #[test]
    fn a_later_decision_with_a_transform_still_replaces_the_earlier_one() {
        let mut plan = sample_plan(vec![sample_finding("f01", "us-ssn", 0.95)]);
        apply_decisions(
            &mut plan,
            &[
                Decision {
                    predicate: parse_predicate("rule=us-ssn").unwrap(),
                    action: Action::Redact,
                    transform: Some(Transform::Mask),
                },
                Decision {
                    predicate: parse_predicate("score>=0.9").unwrap(),
                    action: Action::Redact,
                    transform: Some(Transform::Hash),
                },
            ],
        );
        assert_eq!(plan.findings[0].transform, Some(Transform::Hash));
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
    fn elide_context_truncates_a_long_newline_free_field() {
        // A minified bundle or a tool's JSON output has no newlines, so the
        // "line" around a match is the whole field - and it goes into the plan
        // verbatim, carrying whatever the detectors missed on it.
        let mut text = "x".repeat(4000);
        let start = text.len();
        text.push_str("SECRET");
        text.push_str(&"y".repeat(4000));
        assert_eq!(text.len(), 8006);

        let out = elide_context(&text, start..start + 6, "rule", false);
        assert!(out.len() < 120, "context is unbounded: {} bytes", out.len());
        assert!(out.starts_with('\u{2026}'), "a cut must be marked: {out}");
        assert!(out.ends_with('\u{2026}'), "a cut must be marked: {out}");
        assert!(out.contains("<rule>"));
    }

    #[test]
    fn elide_context_does_not_carry_a_carriage_return() {
        // A lone `\r` in a plan the dry run prints rewinds the terminal over
        // the line before it, hiding whatever was there.
        let text = "line one\r\nkey: SECRET\r\nline three";
        let start = text.find("SECRET").unwrap();
        let out = elide_context(text, start..start + "SECRET".len(), "rule", false);
        assert_eq!(out, "key: <rule>");
        assert!(!out.contains('\r'));

        // Carriage returns alone are a line ending too.
        let cr_only = "line one\rkey: SECRET\rline three";
        let start = cr_only.find("SECRET").unwrap();
        let out = elide_context(cr_only, start..start + "SECRET".len(), "rule", false);
        assert_eq!(out, "key: <rule>");
    }

    #[test]
    fn elide_context_cuts_on_a_char_boundary() {
        let text = format!("{}SECRET", "é".repeat(200));
        let start = text.len() - "SECRET".len();
        let out = elide_context(&text, start..text.len(), "rule", false);
        assert!(out.starts_with('\u{2026}'));
        assert!(out.contains('é'), "the cut must not have split a codepoint");
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

    /// A plan naming `world` inside the fixture's prose, fingerprinted with
    /// `TEST_KEY` - what `generate` would have produced for that document.
    fn world_plan() -> (String, Plan) {
        let text = "hello world, this is prose".to_string();
        let start = text.find("world").unwrap();
        let plan = sample_plan(vec![PlanFinding {
            span: (start, start + "world".len()),
            fingerprint: fp("world"),
            ..sample_finding("f01", "some-rule", 0.9)
        }]);
        (text, plan)
    }

    #[test]
    fn verify_passes_on_an_unmodified_document() {
        let (text, plan) = world_plan();
        let path = fixture_path_with_text("doc-1", "step-1", "convo", &text);
        assert!(verify(&plan, &path, TEST_KEY).is_ok());
    }

    #[test]
    fn verify_refuses_a_same_length_in_span_mutation() {
        // The whole point of verifying: every offset still lands, the step is
        // still there, the pointer still resolves - and the marker would go
        // over five bytes nobody detected.
        let (_, plan) = world_plan();
        let mutated =
            fixture_path_with_text("doc-1", "step-1", "convo", "hello MONKE, this is prose");
        let e = verify(&plan, &mutated, TEST_KEY).unwrap_err().to_string();
        assert!(e.contains("f01"), "should name the first divergence: {e}");
    }

    #[test]
    fn verify_refuses_an_edit_that_shifts_the_span() {
        let (_, plan) = world_plan();
        let shifted =
            fixture_path_with_text("doc-1", "step-1", "convo", "hello  world, this is pros");
        assert!(verify(&plan, &shifted, TEST_KEY).is_err());
    }

    #[test]
    fn verify_refuses_a_plan_fingerprinted_under_another_key() {
        let (text, plan) = world_plan();
        let path = fixture_path_with_text("doc-1", "step-1", "convo", &text);
        assert!(verify(&plan, &path, b"a-different-key").is_err());
    }

    #[test]
    fn verify_fails_naming_the_finding_whose_span_no_longer_lands() {
        let (_, plan) = world_plan();
        let mutated = fixture_path_with_text("doc-1", "step-1", "convo", "short");
        let e = verify(&plan, &mutated, TEST_KEY).unwrap_err().to_string();
        assert!(e.contains("f01"), "should name the first divergence: {e}");
    }

    #[test]
    fn verify_fails_when_a_step_disappears() {
        let plan = sample_plan(vec![PlanFinding {
            step: "step-missing".to_string(),
            span: (0, 5),
            ..sample_finding("f01", "some-rule", 0.9)
        }]);
        let path = fixture_path_with_text("doc-1", "step-1", "convo", "hello world");
        let e = verify(&plan, &path, TEST_KEY).unwrap_err().to_string();
        assert!(e.contains("f01"));
    }

    #[test]
    fn verify_fails_when_the_document_id_differs() {
        let plan = sample_plan(vec![]);
        let path = fixture_path_with_text("different-doc", "step-1", "convo", "text");
        assert!(verify(&plan, &path, TEST_KEY).is_err());
    }

    #[test]
    fn verify_refuses_an_unrecognised_plan_version() {
        let (text, mut plan) = world_plan();
        plan.v = 2;
        let path = fixture_path_with_text("doc-1", "step-1", "convo", &text);
        let e = verify(&plan, &path, TEST_KEY).unwrap_err().to_string();
        assert!(e.contains('2'), "should name the version it refused: {e}");
    }

    #[test]
    fn verify_refuses_findings_whose_ids_are_not_positional() {
        // `apply` reports by id, so a renumbered list makes every diagnostic
        // - and every `--accept id=…` a reviewer writes - name a different
        // finding than the one they read.
        let (text, mut plan) = world_plan();
        plan.findings[0].id = "f07".to_string();
        let path = fixture_path_with_text("doc-1", "step-1", "convo", &text);
        assert!(verify(&plan, &path, TEST_KEY).is_err());
    }

    #[test]
    fn verify_tolerates_an_empty_or_inverted_span_for_apply_to_name() {
        let text = "hello world, this is prose";
        let path = fixture_path_with_text("doc-1", "step-1", "convo", text);
        for span in [(6, 6), (11, 6)] {
            let plan = sample_plan(vec![PlanFinding {
                span,
                ..sample_finding("f01", "some-rule", 0.9)
            }]);
            assert!(
                verify(&plan, &path, TEST_KEY).is_ok(),
                "{span:?} is apply's diagnosis to make, not verify's"
            );
        }
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
                at: "/change/convo/structural/text".to_string(),
                shape: FieldShape::Prose,
                bytes: 42,
            }],
            findings: vec![PlanFinding {
                span: (10, 30),
                score: 0.97,
                context: "<aws-access-key-id> is the key".to_string(),
                fingerprint: fp("AKIAIOSFODNN7EXAMPLE"),
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

    /// Pointer and resolved tool name, one entry per candidate.
    type SeenCtx = std::sync::Arc<std::sync::Mutex<Vec<(String, Option<String>)>>>;

    /// Records what every candidate was shown as. `Context` is the whole of
    /// what a detector is told about where its text came from, so the only
    /// place to observe it is inside one.
    struct CtxSpy(SeenCtx);
    impl Detector for CtxSpy {
        fn id(&self) -> &'static str {
            "ctx-spy"
        }
        fn detect(&self, c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
            self.0
                .lock()
                .unwrap()
                .push((c.at.to_string(), c.ctx.tool_name.map(str::to_owned)));
            Ok(Vec::new())
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

    fn rules_of(plan: &Plan) -> Vec<&str> {
        plan.findings.iter().map(|f| f.rule.as_str()).collect()
    }

    /// Several artifacts per step, several `extra` keys each, under a change
    /// type `surfaces()` has no model for - so both the artifact map's
    /// iteration order and the blind walk's are exercised.
    ///
    /// Built fresh on every call. `HashMap`'s iteration order is seeded per
    /// instance, so a byte-identity test that compares one document against
    /// itself cannot see an order leaking into the output.
    fn fixture_hash_ordered() -> Path {
        let steps = ["s1", "s2"]
            .into_iter()
            .map(|id| {
                let change = ["zeta", "alpha", "mu", "beta"]
                    .into_iter()
                    .map(|artifact| {
                        let extra = ["omega", "delta", "kappa", "iota", "chi"]
                            .into_iter()
                            .map(|key| {
                                (
                                    key.to_string(),
                                    serde_json::Value::String(format!(
                                        "{key} holds a SECRET-VALUE somewhere"
                                    )),
                                )
                            })
                            .collect();
                        (
                            artifact.to_string(),
                            ArtifactChange {
                                raw: None,
                                structural: Some(StructuralChange {
                                    change_type: "provider.blob".to_string(),
                                    extra,
                                }),
                            },
                        )
                    })
                    .collect();
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
            })
            .collect();
        path_of(steps)
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
        // Two separately built fixtures, not one document generated twice: the
        // `HashMap`s a document is made of are seeded per instance, and only a
        // second instance can catch that seed reaching the output.
        let set = detectors();
        let cfg = cfg();
        let a = generate(&fixture_hash_ordered(), &set, &cfg);
        let b = generate(&fixture_hash_ordered(), &set, &cfg);
        assert!(a.findings.len() > 10, "fixture must exercise the ordering");
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    // ── Threshold before overlap resolution (T8) ─────────────────────────

    #[test]
    fn a_low_score_container_never_evicts_an_above_threshold_finding() {
        // Overlap resolution is score-blind on length. Resolve first and the
        // 0.6 whole-line hit wins; threshold afterwards and it is discarded
        // too - and the key it swallowed ships in the clear, unreported.
        let text = "prefix AKIAIOSFODNN7REALKEY suffix";
        assert_eq!(text.len(), 34);
        let path = path_of(vec![step_with_text("s1", "a", text)]);

        let mut set = DetectorSet::default();
        set.push(Box::new(FixedDetector(vec![
            f(0..34, "generic-entropy", 0.6),
            f(7..27, "aws-access-key-id", 0.99),
        ])));

        let plan = generate(&path, &set, &cfg());
        assert_eq!(rules_of(&plan), vec!["aws-access-key-id"]);
        assert_eq!(plan.findings[0].action, Action::Redact);
        assert_eq!(plan.findings[0].span, (7, 27));
    }

    #[test]
    fn a_sub_threshold_finding_that_contests_nothing_stays_in_the_plan() {
        // `--accept score>=0.5` has to have something to accept.
        let path = path_of(vec![step_with_text("s1", "a", "0123456789abcdefghij")]);
        let mut set = DetectorSet::default();
        set.push(Box::new(FixedDetector(vec![
            f(0..5, "certain", 0.99),
            f(10..15, "unsure", 0.4),
        ])));

        let plan = generate(&path, &set, &cfg());
        assert_eq!(rules_of(&plan), vec!["certain", "unsure"]);
        assert_eq!(plan.findings[0].action, Action::Redact);
        assert_eq!(plan.findings[1].action, Action::Skip);
        assert_eq!(plan.findings[0].id, "f01");
        assert_eq!(plan.findings[1].id, "f02");
    }

    // ── Coverage the reviewer asked for ─────────────────────────────────

    #[test]
    fn a_credential_in_path_base_becomes_a_finding_with_an_empty_step() {
        let mut path = path_of(vec![step_with_text("s1", "a", "nothing to see")]);
        path.path.base = Some(toolpath::v1::Base {
            uri: "https://x-token-auth:SECRET-VALUE@example.com/o/r".to_string(),
            ref_str: None,
            branch: None,
        });

        let plan = generate(&path, &detectors(), &cfg());
        let finding = plan
            .findings
            .iter()
            .find(|f| f.at == "/path/base/uri")
            .expect("a document-level surface must be scanned like any other");
        assert_eq!(finding.step, "", "document-level fields belong to no step");
        assert_eq!(finding.action, Action::Redact);
    }

    #[test]
    fn a_plan_always_round_trips_through_json() {
        // JSON has no infinity: serde writes it as `null`, `score` stops
        // parsing as an `f32`, and the plan file cannot be read back at all.
        let mut set = DetectorSet::default();
        set.push(Box::new(FixedDetector(vec![
            f(0..10, "infinite", f32::INFINITY),
            f(10..20, "finite", 0.9),
        ])));

        let plan = generate(&fixture_one_field(), &set, &cfg());
        assert_eq!(rules_of(&plan), vec!["finite"]);

        let json = serde_json::to_string(&plan).unwrap();
        assert!(!json.contains("null"), "a score serialised as null: {json}");
        assert_eq!(serde_json::from_str::<Plan>(&json).unwrap(), plan);
    }

    #[test]
    fn a_surface_that_does_not_read_back_is_refused_not_skipped() {
        // Two steps sharing an id: `surfaces()` names both steps' fields, but
        // a `(step, pointer)` pair can only resolve to one of them. Skipping
        // the half that does not read reports those fields as scanned when
        // nothing ever looked at them.
        let path = path_of(vec![
            step_with_text("dup", "a", "first"),
            step_with_text("dup", "b", "second"),
        ]);
        assert!(matches!(
            generate_checked(&path, &detectors(), &cfg(), false),
            Err(crate::RedactError::BadPointer(_))
        ));
    }

    #[test]
    fn a_tool_use_surface_carries_the_tool_name() {
        let spy = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut set = DetectorSet::default();
        set.push(Box::new(CtxSpy(spy.clone())));

        let mut extra = HashMap::new();
        extra.insert(
            "tool_uses".to_string(),
            serde_json::json!([
                {"name": "Bash", "input": {"command": "echo hi"}},
                {"name": "Read", "input": {"file_path": "/etc/hosts"}},
            ]),
        );
        extra.insert(
            "delegations".to_string(),
            serde_json::json!([{
                "turns": [{"tool_uses": [{"name": "Grep", "input": {"pattern": "x"}}]}],
            }]),
        );
        extra.insert(
            "text".to_string(),
            serde_json::Value::String("plain prose".to_string()),
        );

        let mut change = HashMap::new();
        change.insert(
            "convo".to_string(),
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: "conversation.append".to_string(),
                    extra,
                }),
            },
        );
        let path = path_of(vec![Step {
            step: StepIdentity {
                id: "s1".to_string(),
                parents: Vec::new(),
                actor: "agent:claude-code".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
            },
            change,
            meta: None,
        }]);

        generate(&path, &set, &cfg());

        let seen = spy.lock().unwrap();
        let named = |suffix: &str| -> Option<String> {
            seen.iter()
                .find(|(at, _)| at.ends_with(suffix))
                .and_then(|(_, name)| name.clone())
        };
        assert_eq!(named("/tool_uses/0/input/command").as_deref(), Some("Bash"));
        assert_eq!(
            named("/tool_uses/1/input/file_path").as_deref(),
            Some("Read")
        );
        assert_eq!(
            named("/delegations/0/turns/0/tool_uses/0/input/pattern").as_deref(),
            Some("Grep"),
            "a delegated turn's own tool_uses name its own calls"
        );
        assert_eq!(
            named("/structural/text"),
            None,
            "prose belongs to no tool call"
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
