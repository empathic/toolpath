//! Rewriting a document from an approved plan.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::plan::{Action, Plan, PlanFinding};
use crate::transform::{
    Fingerprint, Transform, apply_spans_desc, apply_transform, resolve_transform,
};
use crate::{RedactConfig, RedactError, RedactReport, Result};

/// Version of the `redaction` record written into `meta`. Bumping it is a
/// document-format change: `apply` refuses records it does not recognise
/// rather than merging into a shape it cannot read.
const RECORD_V: u64 = 1;

const RECORD_KEY: &str = "redaction";

/// Rewrite `path` in place according to `plan`.
///
/// Nothing the plan does not name is touched: with no findings, the
/// serialised output is byte-identical to the input.
pub fn apply(
    path: &mut toolpath::v1::Path,
    plan: &Plan,
    cfg: &RedactConfig,
) -> Result<RedactReport> {
    crate::plan::verify(plan, path)?;

    let mut report = RedactReport {
        surfaces_scanned: plan.surfaces.len(),
        ..Default::default()
    };
    for f in plan.findings.iter().filter(|f| f.action == Action::Skip) {
        *report.flagged.entry(f.rule.clone()).or_default() += 1;
    }

    // A plan that redacts nothing invalidates no signature and removes no
    // content, so it must leave the document byte-identical - including its
    // signatures and any record a previous pass left.
    if !plan.findings.iter().any(|f| f.action == Action::Redact) {
        return Ok(report);
    }

    report.signatures_dropped = guard_signatures(path, cfg)?;

    let mut records: BTreeMap<String, BTreeMap<RecordKey, usize>> = BTreeMap::new();
    let mut touched: BTreeSet<&str> = BTreeSet::new();
    {
        let mut cursor = crate::surface::SurfaceCursor { path: &mut *path };
        for ((step, at), group) in group_by_field(plan) {
            let Some(text) = cursor.read(step, at) else {
                return Err(RedactError::BadPointer(format!("{step}{at}")));
            };

            let mut edits = Vec::with_capacity(group.len());
            for f in group {
                // A span landing mid-codepoint would panic the slice, so the
                // bounds check has to happen here and not only in `verify`.
                let value = text.get(f.span.0..f.span.1).ok_or_else(|| {
                    RedactError::PlanMismatch(format!("{}: span does not land on {at}", f.id))
                })?;
                let fp = Fingerprint::new(&cfg.key, value);
                let op = resolve_transform(cfg, &f.rule, f.transform);
                edits.push((f.span.0..f.span.1, apply_transform(op, &f.rule, value, &fp)));

                *report.replaced.entry(f.rule.clone()).or_default() += 1;
                *records
                    .entry(step.to_string())
                    .or_default()
                    .entry(RecordKey {
                        at: at.to_string(),
                        rule: f.rule.clone(),
                        fp: fp.0,
                        op: op_name(op),
                    })
                    .or_default() += 1;
            }

            cursor.write(step, at, &apply_spans_desc(&text, &mut edits))?;
            touched.insert(step);
        }
    }

    // Surfaces outside any step (`path.base`, `meta.vcs_remote`) carry the
    // empty step id and have no step to be recorded on.
    touched.remove("");
    report.steps_touched = touched.len();

    write_step_records(path, &records)?;
    write_rollup(path, plan, cfg, &report)?;
    Ok(report)
}

/// Every edit to one string has to be spliced in a single right-to-left
/// pass, so the findings are grouped by the field they land in; applying
/// them one field-visit at a time would invalidate the offsets of the edits
/// still pending.
fn group_by_field(plan: &Plan) -> BTreeMap<(&str, &str), Vec<&PlanFinding>> {
    let mut out: BTreeMap<(&str, &str), Vec<&PlanFinding>> = BTreeMap::new();
    for f in plan.findings.iter().filter(|f| f.action == Action::Redact) {
        out.entry((f.step.as_str(), f.at.as_str()))
            .or_default()
            .push(f);
    }
    out
}

/// Strip every signature in the document, returning how many there were.
///
/// A signature covers content the pass is about to change, so leaving one in
/// place would publish a signature that no longer verifies.
fn guard_signatures(path: &mut toolpath::v1::Path, cfg: &RedactConfig) -> Result<usize> {
    let total = path.meta.as_ref().map_or(0, |m| m.signatures.len())
        + path
            .steps
            .iter()
            .filter_map(|s| s.meta.as_ref())
            .map(|m| m.signatures.len())
            .sum::<usize>();
    if total == 0 {
        return Ok(0);
    }
    if !cfg.drop_signatures {
        return Err(RedactError::SignedDocument);
    }
    if let Some(m) = path.meta.as_mut() {
        m.signatures.clear();
    }
    for s in &mut path.steps {
        if let Some(m) = s.meta.as_mut() {
            m.signatures.clear();
        }
    }
    Ok(total)
}

/// Identifies one line of the audit record. The record publishes `at`, so two
/// occurrences of one credential in two different fields cannot collapse into
/// a single entry without making `at` a lie.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RecordKey {
    at: String,
    rule: String,
    fp: String,
    op: String,
}

/// The record is part of the document contract, so `op` has to be exactly the
/// serde name of the `Transform` rather than a second spelling of it.
fn op_name(t: Transform) -> String {
    match serde_json::to_value(t) {
        Ok(Value::String(s)) => s,
        _ => unreachable!("Transform serialises as a string"),
    }
}

fn write_step_records(
    path: &mut toolpath::v1::Path,
    records: &BTreeMap<String, BTreeMap<RecordKey, usize>>,
) -> Result<()> {
    for (id, fresh) in records {
        if id.is_empty() {
            continue;
        }
        let step = path
            .steps
            .iter_mut()
            .find(|s| s.step.id == *id)
            .ok_or_else(|| RedactError::PlanMismatch(format!("no step {id}")))?;
        let meta = step.meta.get_or_insert_with(Default::default);

        let mut merged = seed_from_existing(meta.extra.get(RECORD_KEY))?;
        for (k, n) in fresh {
            *merged.entry(k.clone()).or_default() += n;
        }
        meta.extra.insert(RECORD_KEY.into(), record_value(&merged));
    }
    Ok(())
}

/// Re-read an existing record so a second pass merges into it. Appending a
/// second `redaction` object, or nesting one inside the other, would make the
/// step's own history unreadable.
fn seed_from_existing(existing: Option<&Value>) -> Result<BTreeMap<RecordKey, usize>> {
    let Some(v) = existing else {
        return Ok(BTreeMap::new());
    };
    check_version(v)?;
    let entries = v
        .get("findings")
        .and_then(Value::as_array)
        .ok_or_else(|| RedactError::PlanMismatch("redaction record has no findings".into()))?;

    let mut out = BTreeMap::new();
    for e in entries {
        let field = |k: &str| {
            e.get(k)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    RedactError::PlanMismatch(format!("redaction record entry lacks {k}"))
                })
        };
        let key = RecordKey {
            at: field("at")?,
            rule: field("rule")?,
            fp: field("fp")?,
            op: field("op")?,
        };
        let n = e
            .get("n")
            .and_then(Value::as_u64)
            .ok_or_else(|| RedactError::PlanMismatch("redaction record entry lacks n".into()))?;
        *out.entry(key).or_default() += n as usize;
    }
    Ok(out)
}

fn check_version(v: &Value) -> Result<()> {
    match v.get("v").and_then(Value::as_u64) {
        Some(RECORD_V) => Ok(()),
        other => Err(RedactError::PlanMismatch(format!(
            "unrecognised redaction record version {other:?}"
        ))),
    }
}

fn record_value(entries: &BTreeMap<RecordKey, usize>) -> Value {
    let findings: Vec<Value> = entries
        .iter()
        .map(|(k, n)| json!({ "rule": k.rule, "at": k.at, "n": n, "fp": k.fp, "op": k.op }))
        .collect();
    json!({ "v": RECORD_V, "findings": findings })
}

fn write_rollup(
    path: &mut toolpath::v1::Path,
    plan: &Plan,
    cfg: &RedactConfig,
    report: &RedactReport,
) -> Result<()> {
    let previous = path
        .meta
        .as_ref()
        .and_then(|m| m.extra.get(RECORD_KEY))
        .cloned();
    if let Some(p) = &previous {
        check_version(p)?;
    }

    // Counted off the step records rather than accumulated across passes, so
    // a step redacted twice stays one touched step.
    let steps_touched = path
        .steps
        .iter()
        .filter(|s| {
            s.meta
                .as_ref()
                .is_some_and(|m| m.extra.contains_key(RECORD_KEY))
        })
        .count();

    let replaced = merge_counts(previous.as_ref(), "replaced", &report.replaced);
    let signatures_dropped =
        previous_u64(previous.as_ref(), "signatures_dropped") + report.signatures_dropped as u64;

    let meta = path.meta.get_or_insert_with(Default::default);
    meta.extra.insert(
        RECORD_KEY.into(),
        json!({
            "v": RECORD_V,
            "at": cfg.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "tool": concat!("toolpath-redact/", env!("CARGO_PKG_VERSION")),
            "detectors": plan.detectors,
            "mode": cfg.mode,
            "steps_touched": steps_touched,
            "replaced": replaced,
            // `flagged` names findings still sitting in the document, so it is
            // this pass's tally and not a running total.
            "flagged": report.flagged,
            "signatures_dropped": signatures_dropped,
        }),
    );
    Ok(())
}

fn merge_counts(
    previous: Option<&Value>,
    field: &str,
    fresh: &BTreeMap<String, usize>,
) -> BTreeMap<String, u64> {
    let mut out: BTreeMap<String, u64> = previous
        .and_then(|p| p.get(field))
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
                .collect()
        })
        .unwrap_or_default();
    for (k, n) in fresh {
        *out.entry(k.clone()).or_default() += *n as u64;
    }
    out
}

fn previous_u64(previous: Option<&Value>, field: &str) -> u64 {
    previous
        .and_then(|p| p.get(field))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::PlanDefaults;
    use crate::surface::{Surface, SurfaceCursor, surfaces};
    use chrono::{TimeZone, Utc};
    use std::collections::HashMap;
    use std::ops::Range;
    use std::sync::LazyLock;
    use toolpath::v1::{
        ArtifactChange, Base, Path, PathIdentity, PathMeta, Signature, Step, StepIdentity,
        StepMeta, StructuralChange,
    };

    const AWS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
    const AWS_KEY_2: &str = "AKIAJ7EXAMPLEKEY1234";
    const GH_PAT: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

    fn cfg() -> RedactConfig {
        RedactConfig {
            threshold: 0.5,
            mode: Transform::Marker,
            mode_for: Vec::new(),
            key: b"test-key".to_vec(),
            now: Utc.with_ymd_and_hms(2026, 7, 30, 18, 4, 11).unwrap(),
            drop_signatures: false,
            reveal: false,
        }
    }

    // ── A stand-in detector ─────────────────────────────────────────────
    //
    // `plan_for` re-detects against whatever the document currently holds;
    // hard-coded spans would reduce the idempotence test to "the same span
    // was redacted twice". Both rules match a self-delimiting prefixed
    // format, which is what keeps a marker, mask, hash or partial output from
    // being mistaken for a fresh secret on the second pass.

    static AWS_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"AKIA[0-9A-Z]{16}").unwrap());
    static GH_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"ghp_[A-Za-z0-9]{36}").unwrap());

    fn mini_scan(text: &str) -> Vec<(Range<usize>, &'static str)> {
        let mut out: Vec<(Range<usize>, &'static str)> = AWS_RE
            .find_iter(text)
            .map(|m| (m.range(), "aws-access-key-id"))
            .chain(GH_RE.find_iter(text).map(|m| (m.range(), "github-pat")))
            .collect();
        out.sort_by_key(|(r, _)| r.start);
        out
    }

    // ── Plan construction ───────────────────────────────────────────────

    fn plan_with(path: &Path, surfaces: Vec<Surface>, mut findings: Vec<PlanFinding>) -> Plan {
        findings.sort_by(|a, b| (&a.step, &a.at, a.span.0).cmp(&(&b.step, &b.at, b.span.0)));
        for (i, f) in findings.iter_mut().enumerate() {
            f.id = crate::plan::finding_id(i);
        }
        Plan {
            v: 1,
            document: path.path.id.clone(),
            generated: cfg().now,
            detectors: vec!["fixed".into()],
            defaults: PlanDefaults {
                transform: Transform::Marker,
                threshold: 0.5,
            },
            surfaces,
            findings,
        }
    }

    fn empty_plan(path: &Path) -> Plan {
        plan_with(path, surfaces(path), Vec::new())
    }

    /// Reads every surface without a `&mut` borrow of the caller's copy, so a
    /// plan can be built inside the same call that mutates the document.
    fn read_surfaces(path: &Path) -> Vec<(Surface, String)> {
        let mut probe = path.clone();
        let all = surfaces(&probe);
        let cursor = SurfaceCursor { path: &mut probe };
        all.into_iter()
            .filter_map(|s| cursor.read(&s.step, &s.at).map(|t| (s, t)))
            .collect()
    }

    fn finding(s: &Surface, span: Range<usize>, rule: &str, action: Action) -> PlanFinding {
        PlanFinding {
            id: String::new(),
            step: s.step.clone(),
            at: s.at.clone(),
            rule: rule.into(),
            span: (span.start, span.end),
            score: 0.99,
            detector: "fixed".into(),
            shape: s.shape,
            context: format!("<{rule}>"),
            action,
            transform: None,
        }
    }

    fn scan_findings(path: &Path, action: Action) -> Vec<PlanFinding> {
        read_surfaces(path)
            .iter()
            .flat_map(|(s, text)| {
                mini_scan(text)
                    .into_iter()
                    .map(move |(span, rule)| finding(s, span, rule, action))
            })
            .collect()
    }

    fn plan_for(path: &Path) -> Plan {
        plan_with(path, surfaces(path), scan_findings(path, Action::Redact))
    }

    fn plan_touching_only_text(path: &Path) -> Plan {
        plan_with(path, surfaces(path), text_findings(path))
    }

    fn text_findings(path: &Path) -> Vec<PlanFinding> {
        scan_findings(path, Action::Redact)
            .into_iter()
            .filter(|f| f.at.ends_with("/text"))
            .collect()
    }

    // ── Fixtures ────────────────────────────────────────────────────────

    fn obj(v: Value) -> HashMap<String, Value> {
        match v {
            Value::Object(m) => m.into_iter().collect(),
            other => panic!("fixture is not an object: {other}"),
        }
    }

    fn append_step(id: &str, extra: Value) -> Step {
        Step {
            step: StepIdentity {
                id: id.into(),
                parents: Vec::new(),
                actor: "agent:claude-opus-5".into(),
                timestamp: "2026-07-30T18:00:00Z".into(),
            },
            change: HashMap::from([(
                "claude://sess-abc".to_string(),
                ArtifactChange {
                    raw: None,
                    structural: Some(StructuralChange {
                        change_type: "conversation.append".into(),
                        extra: obj(extra),
                    }),
                },
            )]),
            meta: None,
        }
    }

    fn doc(steps: Vec<Step>) -> Path {
        let head = steps.last().map_or(String::new(), |s| s.step.id.clone());
        Path {
            path: PathIdentity {
                id: "path-claude-code-0f3a2b71".into(),
                base: Some(Base {
                    uri: "file:///tmp/work".into(),
                    ref_str: None,
                    branch: Some("main".into()),
                }),
                head,
                graph_ref: None,
            },
            steps,
            meta: Some(PathMeta {
                title: Some("Claude session: 0f3a2b71".into()),
                kind: Some(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION.into()),
                ..Default::default()
            }),
        }
    }

    fn fixture_clean_document() -> Path {
        doc(vec![append_step(
            "turn-0f3a",
            json!({
                "text": "Refactored the loader; nothing sensitive here.",
                "thinking": "The cache path is the only thing worth checking.",
                "tool_uses": [{
                    "name": "Bash",
                    "input": { "command": "cargo test -p toolpath" },
                    "result": { "content": "test result: ok. 69 passed" }
                }]
            }),
        )])
    }

    fn fixture_with_secret(secret: &str) -> Path {
        doc(vec![append_step(
            "turn-0f3a",
            json!({
                "text": format!("exported {secret} into the shell"),
                "thinking": "Nothing to see here.",
            }),
        )])
    }

    fn fixture_with_secrets() -> Path {
        doc(vec![
            append_step(
                "turn-0f3a",
                json!({
                    "text": "Ran the deploy script.",
                    "tool_uses": [{
                        "name": "Bash",
                        "input": { "command": "aws s3 ls" },
                        "result": { "content": format!("AWS_ACCESS_KEY_ID={AWS_KEY}\ndone") }
                    }]
                }),
            ),
            append_step(
                "turn-9c21",
                json!({ "text": format!("then pushed with {GH_PAT}") }),
            ),
        ])
    }

    fn fixture_with_extra_keys(keys: &[&str]) -> Path {
        let mut extra = json!({ "text": format!("leaked {AWS_KEY} once") });
        for k in keys {
            extra[*k] = json!({ "kept": true, "n": 7 });
        }
        doc(vec![append_step("turn-0f3a", extra)])
    }

    fn fixture_file_write_with_secret_in_diff() -> Path {
        let raw = format!(
            "--- a/src/config.rs\n\
             +++ b/src/config.rs\n\
             @@ -1,3 +1,3 @@\n\
             \x20fn main() {{\n\
             -    let key = \"\";\n\
             +    let key = \"{AWS_KEY}\";\n\
             \x20}}\n"
        );
        let mut step = append_step("turn-0f3a", json!({ "text": "wrote the config" }));
        step.change.insert(
            "src/config.rs".into(),
            ArtifactChange {
                raw: Some(raw),
                structural: None,
            },
        );
        doc(vec![step])
    }

    fn fixture_signed() -> Path {
        let mut p = fixture_with_secret(AWS_KEY);
        p.meta.as_mut().unwrap().signatures = vec![Signature {
            signer: "human:alex".into(),
            key: "ssh:SHA256:abc".into(),
            scope: "path".into(),
            sig: "base64sig".into(),
            timestamp: None,
        }];
        p
    }

    // ── Inspection helpers ──────────────────────────────────────────────

    fn has_key_somewhere(path: &Path, key: &str) -> bool {
        fn walk(v: &Value, key: &str) -> bool {
            match v {
                Value::Object(m) => m.contains_key(key) || m.values().any(|i| walk(i, key)),
                Value::Array(a) => a.iter().any(|i| walk(i, key)),
                _ => false,
            }
        }
        walk(&serde_json::to_value(path).unwrap(), key)
    }

    fn record_of<'a>(path: &'a Path, step: &str) -> &'a Value {
        path.steps
            .iter()
            .find(|s| s.step.id == step)
            .and_then(|s| s.meta.as_ref())
            .and_then(|m| m.extra.get(RECORD_KEY))
            .unwrap_or_else(|| panic!("no redaction record on {step}"))
    }

    fn raw_diff_of(path: &Path) -> String {
        path.steps
            .iter()
            .flat_map(|s| s.change.values())
            .find_map(|c| c.raw.clone())
            .expect("fixture has a raw diff")
    }

    fn count_lines(h: &diffy::Hunk<'_, str>, side: char) -> usize {
        h.lines()
            .iter()
            .filter(|l| {
                matches!(
                    (l, side),
                    (diffy::Line::Context(_), _)
                        | (diffy::Line::Delete(_), '-')
                        | (diffy::Line::Insert(_), '+')
                )
            })
            .count()
    }

    fn at_ending(path: &Path, suffix: &str) -> String {
        surfaces(path)
            .into_iter()
            .find(|s| s.at.ends_with(suffix))
            .unwrap_or_else(|| panic!("no surface ending in {suffix}"))
            .at
    }

    fn text_at(path: &Path, suffix: &str) -> String {
        let mut probe = path.clone();
        let at = at_ending(path, suffix);
        let step = path.steps[0].step.id.clone();
        let cursor = SurfaceCursor { path: &mut probe };
        cursor
            .read(&step, &at)
            .unwrap_or_else(|| panic!("{at} does not resolve"))
    }

    fn marker(value: &str, rule: &str) -> String {
        apply_transform(
            Transform::Marker,
            rule,
            value,
            &Fingerprint::new(&cfg().key, value),
        )
    }

    // ── Step 7.1: the invariants ────────────────────────────────────────

    #[test]
    fn no_findings_means_byte_identical_output() {
        // The most important test here: the pass must not perturb anything it
        // is not redacting.
        let before = fixture_clean_document();
        let mut after = before.clone();
        apply(&mut after, &empty_plan(&before), &cfg()).unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&before).unwrap(),
            serde_json::to_string_pretty(&after).unwrap()
        );
    }

    #[test]
    fn unknown_provider_keys_survive() {
        // Guards against reimplementing this as extract -> derive:
        // `extra["edits"]` is written by toolpath-convo's derive and never
        // read back by extract, so a round-trip silently drops it.
        let mut doc = fixture_with_extra_keys(&["edits", "vendor_specific", "entry_extra"]);
        let plan = plan_touching_only_text(&doc);
        apply(&mut doc, &plan, &cfg()).unwrap();
        for k in ["edits", "vendor_specific", "entry_extra"] {
            assert!(has_key_somewhere(&doc, k), "lost {k}");
        }
    }

    #[test]
    fn idempotent_across_all_transforms() {
        for mode in [
            Transform::Marker,
            Transform::Remove,
            Transform::Hash,
            Transform::Mask,
            Transform::Partial,
        ] {
            let cfg = RedactConfig { mode, ..cfg() };
            let mut once = fixture_with_secrets();
            let plan = plan_for(&once);
            apply(&mut once, &plan, &cfg).unwrap();
            let mut twice = once.clone();
            let plan = plan_for(&twice);
            apply(&mut twice, &plan, &cfg).unwrap();
            assert_eq!(
                serde_json::to_string(&once).unwrap(),
                serde_json::to_string(&twice).unwrap(),
                "{mode:?} is not idempotent"
            );
        }
    }

    #[test]
    fn redacted_diff_still_parses_and_line_counts_hold() {
        let mut doc = fixture_file_write_with_secret_in_diff();
        let plan = plan_for(&doc);
        apply(&mut doc, &plan, &cfg()).unwrap();
        let raw = raw_diff_of(&doc);
        let patch = diffy::Patch::from_str(&raw).expect("redacted diff must still parse");
        for h in patch.hunks() {
            assert_eq!(h.old_range().len(), count_lines(h, '-'));
            assert_eq!(h.new_range().len(), count_lines(h, '+'));
        }
        assert!(!raw.contains(AWS_KEY));
    }

    #[test]
    fn audit_record_lands_on_the_step_and_merges_on_rerun() {
        let mut doc = fixture_with_secrets();
        let plan = plan_for(&doc);
        apply(&mut doc, &plan, &cfg()).unwrap();
        let first = record_of(&doc, "turn-0f3a").clone();
        let plan = plan_for(&doc);
        apply(&mut doc, &plan, &cfg()).unwrap();
        assert_eq!(
            record_of(&doc, "turn-0f3a"),
            &first,
            "record must merge, not append"
        );
    }

    #[test]
    fn audit_record_carries_no_value_substring_or_length() {
        let secret = "AKIAIOSFODNN7REALKEY";
        let mut doc = fixture_with_secret(secret);
        let plan = plan_for(&doc);
        apply(&mut doc, &plan, &cfg()).unwrap();
        let rec = serde_json::to_string(record_of(&doc, "turn-0f3a")).unwrap();
        assert!(!rec.contains(secret));
        for w in 6..secret.len() {
            for s in secret.as_bytes().windows(w) {
                assert!(!rec.contains(std::str::from_utf8(s).unwrap()));
            }
        }
        assert!(!rec.contains(&secret.len().to_string()));
    }

    #[test]
    fn signed_document_refuses_without_the_flag() {
        let mut doc = fixture_signed();
        let plan = plan_for(&doc);
        assert!(matches!(
            apply(&mut doc, &plan, &cfg()),
            Err(RedactError::SignedDocument)
        ));
        let cfg = RedactConfig {
            drop_signatures: true,
            ..cfg()
        };
        let plan = plan_for(&doc);
        assert_eq!(apply(&mut doc, &plan, &cfg).unwrap().signatures_dropped, 1);
    }

    #[test]
    fn output_validates_against_both_schemas() {
        // `jsonschema` is not a dev-dependency of this crate and `Cargo.toml`
        // belongs to another track, so this asserts the structural rules the
        // two schemas impose on where the record may sit. T11 runs the real
        // validators through `path p validate`.
        let mut doc = fixture_with_secrets();
        let plan = plan_for(&doc);
        apply(&mut doc, &plan, &cfg()).unwrap();
        let v = serde_json::to_value(&doc).unwrap();

        let top: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert!(
            top.is_subset(&BTreeSet::from(["path", "steps", "meta"])),
            "{top:?}"
        );

        for step in v["steps"].as_array().unwrap() {
            let keys: BTreeSet<&str> = step
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            // `step` is `additionalProperties: false`: the record cannot sit
            // as a sibling of step/change/meta.
            assert!(
                keys.is_subset(&BTreeSet::from(["step", "change", "meta"])),
                "{keys:?}"
            );
            let ident: BTreeSet<&str> = step["step"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert!(
                ident.is_subset(&BTreeSet::from(["id", "parents", "actor", "timestamp"])),
                "{ident:?}"
            );
            assert!(step["step"]["id"].is_string());
            assert!(step["step"]["actor"].is_string());
            assert!(step["step"]["timestamp"].is_string());
            assert!(step["change"].is_object());
        }

        assert_eq!(v["steps"][0]["meta"]["redaction"]["v"], json!(RECORD_V));
        assert_eq!(v["meta"]["redaction"]["v"], json!(RECORD_V));
        assert_eq!(
            v["meta"]["kind"],
            json!(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION)
        );
    }

    // ── Boundaries ──────────────────────────────────────────────────────

    #[test]
    fn refused_signed_document_is_left_untouched() {
        let before = fixture_signed();
        let mut after = before.clone();
        assert!(apply(&mut after, &plan_for(&before), &cfg()).is_err());
        assert_eq!(
            serde_json::to_string(&before).unwrap(),
            serde_json::to_string(&after).unwrap()
        );
    }

    #[test]
    fn two_findings_in_one_field_splice_right_to_left() {
        // A left-to-right splice corrupts the second span: the marker is
        // longer than the key it replaces.
        let mut d = doc(vec![append_step(
            "turn-0f3a",
            json!({ "text": format!("first {AWS_KEY} then {AWS_KEY_2} end") }),
        )]);
        let plan = plan_for(&d);
        apply(&mut d, &plan, &cfg()).unwrap();
        assert_eq!(
            text_at(&d, "/text"),
            format!(
                "first {} then {} end",
                marker(AWS_KEY, "aws-access-key-id"),
                marker(AWS_KEY_2, "aws-access-key-id")
            )
        );
    }

    #[test]
    fn finding_covering_the_entire_field_value() {
        let mut d = doc(vec![append_step("turn-0f3a", json!({ "text": AWS_KEY }))]);
        let plan = plan_for(&d);
        apply(&mut d, &plan, &cfg()).unwrap();
        assert_eq!(text_at(&d, "/text"), marker(AWS_KEY, "aws-access-key-id"));
    }

    #[test]
    fn remove_transform_empties_the_field() {
        let cfg = RedactConfig {
            mode: Transform::Remove,
            ..cfg()
        };
        let mut d = doc(vec![append_step("turn-0f3a", json!({ "text": AWS_KEY }))]);
        let at = at_ending(&d, "/text");
        let plan = plan_for(&d);
        apply(&mut d, &plan, &cfg).unwrap();
        let cursor = SurfaceCursor { path: &mut d };
        assert_eq!(cursor.read("turn-0f3a", &at).as_deref(), Some(""));
    }

    #[test]
    fn all_skips_leave_the_document_untouched_but_are_flagged() {
        let before = fixture_with_secrets();
        let mut after = before.clone();
        let plan = plan_with(
            &before,
            surfaces(&before),
            scan_findings(&before, Action::Skip),
        );
        let report = apply(&mut after, &plan, &cfg()).unwrap();
        assert_eq!(
            serde_json::to_string(&before).unwrap(),
            serde_json::to_string(&after).unwrap()
        );
        assert_eq!(report.flagged["aws-access-key-id"], 1);
        assert_eq!(report.flagged["github-pat"], 1);
        assert!(report.replaced.is_empty());
    }

    #[test]
    fn unknown_step_id_errors_rather_than_panicking() {
        let before = fixture_with_secrets();
        let mut after = before.clone();
        let mut plan = plan_for(&before);
        for f in &mut plan.findings {
            f.step = "turn-does-not-exist".into();
        }
        assert!(matches!(
            apply(&mut after, &plan, &cfg()),
            Err(RedactError::PlanMismatch(_))
        ));
        assert_eq!(
            serde_json::to_string(&before).unwrap(),
            serde_json::to_string(&after).unwrap()
        );
    }

    #[test]
    fn inverted_span_errors_rather_than_panicking() {
        // `verify` bounds-checks both ends but not their order, so an
        // end-before-start span reaches the slice.
        let before = fixture_with_secret(AWS_KEY);
        let mut after = before.clone();
        let mut plan = plan_for(&before);
        plan.findings[0].span = (plan.findings[0].span.1, plan.findings[0].span.0);
        assert!(matches!(
            apply(&mut after, &plan, &cfg()),
            Err(RedactError::PlanMismatch(_))
        ));
    }

    #[test]
    fn path_level_surface_is_redacted_without_a_step_record() {
        let mut d = fixture_with_secret(AWS_KEY);
        d.meta.as_mut().unwrap().extra.insert(
            "vcs_remote".into(),
            json!(format!("https://{GH_PAT}@github.com/o/r.git")),
        );
        let plan = plan_for(&d);
        let report = apply(&mut d, &plan, &cfg()).unwrap();

        let remote = d.meta.as_ref().unwrap().extra["vcs_remote"]
            .as_str()
            .unwrap();
        assert!(!remote.contains(GH_PAT), "{remote}");
        assert_eq!(report.replaced["github-pat"], 1);
        assert_eq!(report.steps_touched, 1, "the path itself is not a step");
        assert_eq!(
            d.meta.as_ref().unwrap().extra[RECORD_KEY]["steps_touched"],
            json!(1)
        );
    }

    #[test]
    fn multibyte_content_around_a_redacted_span() {
        let mut d = doc(vec![append_step(
            "turn-0f3a",
            json!({ "text": format!("鍵は {AWS_KEY} です - 気をつけて") }),
        )]);
        let plan = plan_for(&d);
        apply(&mut d, &plan, &cfg()).unwrap();
        assert_eq!(
            text_at(&d, "/text"),
            format!(
                "鍵は {} です - 気をつけて",
                marker(AWS_KEY, "aws-access-key-id")
            )
        );
    }

    #[test]
    fn document_with_zero_steps() {
        let before = doc(Vec::new());
        let mut after = before.clone();
        let report = apply(&mut after, &empty_plan(&before), &cfg()).unwrap();
        assert_eq!(report.steps_touched, 0);
        assert_eq!(
            serde_json::to_string(&before).unwrap(),
            serde_json::to_string(&after).unwrap()
        );
    }

    #[test]
    fn two_passes_over_different_secrets_merge_into_one_record() {
        let mut d = doc(vec![append_step(
            "turn-0f3a",
            json!({
                "text": format!("first {AWS_KEY}"),
                "thinking": format!("later {GH_PAT}"),
            }),
        )]);
        let text_only = plan_with(&d, surfaces(&d), text_findings(&d));
        apply(&mut d, &text_only, &cfg()).unwrap();
        let plan = plan_for(&d);
        apply(&mut d, &plan, &cfg()).unwrap();

        let rec = record_of(&d, "turn-0f3a");
        assert_eq!(rec["v"], json!(RECORD_V));
        let entries = rec["findings"].as_array().unwrap();
        assert_eq!(entries.len(), 2, "{rec}");
        let rules: BTreeSet<&str> = entries
            .iter()
            .map(|f| f["rule"].as_str().unwrap())
            .collect();
        assert_eq!(rules, BTreeSet::from(["aws-access-key-id", "github-pat"]));
    }

    #[test]
    fn repeated_secret_in_one_field_aggregates_to_one_entry() {
        let mut d = doc(vec![append_step(
            "turn-0f3a",
            json!({ "text": format!("{AWS_KEY} and again {AWS_KEY}") }),
        )]);
        let plan = plan_for(&d);
        apply(&mut d, &plan, &cfg()).unwrap();
        let entries = record_of(&d, "turn-0f3a")["findings"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["n"], json!(2));
    }

    #[test]
    fn rollup_counts_steps_not_fields() {
        let mut d = doc(vec![append_step(
            "turn-0f3a",
            json!({
                "text": format!("a {AWS_KEY}"),
                "thinking": format!("b {GH_PAT}"),
            }),
        )]);
        let plan = plan_for(&d);
        let report = apply(&mut d, &plan, &cfg()).unwrap();
        assert_eq!(report.steps_touched, 1);
        assert_eq!(
            d.meta.as_ref().unwrap().extra[RECORD_KEY]["steps_touched"],
            json!(1)
        );
    }

    #[test]
    fn unrecognised_record_version_is_refused() {
        let mut d = fixture_with_secret(AWS_KEY);
        d.steps[0].meta = Some(StepMeta {
            extra: HashMap::from([(RECORD_KEY.to_string(), json!({ "v": 99 }))]),
            ..Default::default()
        });
        let plan = plan_for(&d);
        assert!(matches!(
            apply(&mut d, &plan, &cfg()),
            Err(RedactError::PlanMismatch(_))
        ));
    }

    #[test]
    fn report_counts_surfaces_and_replacements() {
        let before = fixture_with_secrets();
        let mut after = before.clone();
        let plan = plan_for(&before);
        let report = apply(&mut after, &plan, &cfg()).unwrap();
        assert_eq!(report.surfaces_scanned, plan.surfaces.len());
        assert_eq!(report.replaced["aws-access-key-id"], 1);
        assert_eq!(report.replaced["github-pat"], 1);
        assert_eq!(report.steps_touched, 2);
        assert!(report.flagged.is_empty());
    }
}
