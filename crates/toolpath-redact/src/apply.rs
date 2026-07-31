//! Rewriting a document from an approved plan.

use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    // An unkeyed `fp` is a truncated SHA-256 of the credential, which a
    // dictionary attack reverses (EDPB 01/2025 para 88). Refused before any
    // write, so a caller that forgot the key still holds its document.
    if cfg.key.is_empty() {
        return Err(RedactError::EmptyKey);
    }

    crate::plan::verify(plan, path, &cfg.key)?;

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

    let touched = touched_steps(plan);
    report.steps_touched = touched.len();

    // Every write below lands in a scratch clone, and the caller's document
    // is replaced only once the whole plan has applied: an error part-way
    // through must not leave a half-redacted document behind.
    let mut work = path.clone();
    report.signatures_dropped = guard_signatures(&mut work, &touched, cfg)?;

    let mut records: BTreeMap<String, BTreeMap<RecordKey, usize>> = BTreeMap::new();
    let mut renames: Renames = HashMap::new();
    {
        let mut cursor = crate::surface::SurfaceCursor { path: &mut work };
        for ((step, at), group) in group_by_field(plan) {
            let Some(text) = cursor.read(step, at) else {
                return Err(RedactError::BadPointer(format!("{at} on step {step:?}")));
            };

            // Both span guards are preconditions on the whole group, so they
            // run before the first edit is built: `apply_spans_desc` silently
            // drops a span an earlier splice invalidated, and by then the
            // report has already counted a replacement that will not happen.
            for f in &group {
                // A zero-width span splices a marker into text that holds no
                // secret, and an inverted one would panic the slice below; a
                // plan is hand-editable, so neither can be assumed away. This
                // also makes the spans orderable for the overlap check.
                if f.span.0 >= f.span.1 {
                    return Err(RedactError::PlanMismatch(format!(
                        "{}: span {}..{} is empty or inverted",
                        f.id, f.span.0, f.span.1
                    )));
                }
            }
            reject_overlaps(&group)?;

            let mut edits = Vec::with_capacity(group.len());
            let mut entries = Vec::with_capacity(group.len());
            for f in &group {
                // A span landing mid-codepoint would panic the slice, so the
                // bounds check has to happen here and not only in `verify`.
                let value = text.get(f.span.0..f.span.1).ok_or_else(|| {
                    RedactError::PlanMismatch(format!("{}: span does not land on {at}", f.id))
                })?;
                let fp = Fingerprint::new(&cfg.key, value);
                let op = resolve_transform(cfg, &f.rule, f.transform);
                edits.push((f.span.0..f.span.1, apply_transform(op, &f.rule, value, &fp)));
                entries.push((f.rule.clone(), fp.0, op_name(op)));

                *report.replaced.entry(f.rule.clone()).or_default() += 1;
            }

            let new_text = apply_spans_desc(&text, &mut edits);
            // An artifact key is the identity of the thing that changed, and
            // `surfaces` skips empty strings: emptying it would leave a key
            // no later pass could address, named by a `/change/` pointer that
            // routes nowhere.
            if new_text.is_empty() && artifact_key_of(at).is_some_and(|(_, is_key)| is_key) {
                return Err(RedactError::PlanMismatch(format!(
                    "{at}: redacting the whole artifact key would leave it empty"
                )));
            }

            let step_records = records.entry(step.to_string()).or_default();
            for (rule, fp, op) in entries {
                *step_records
                    .entry(RecordKey {
                        at: at.to_string(),
                        rule,
                        fp,
                        op,
                    })
                    .or_default() += 1;
            }
            if let Some((token, true)) = artifact_key_of(at) {
                renames
                    .entry(step.to_string())
                    .or_default()
                    .insert(token.to_string(), crate::surface::ptr_escape(&new_text));
            }
            cursor.write(step, at, &new_text)?;
        }
    }

    // Surfaces outside any step carry the empty step id and belong to no step
    // record; they are published on the rollup instead.
    let path_level = records.remove("").unwrap_or_default();
    write_step_records(&mut work, &records, &renames)?;
    write_rollup(&mut work, plan, cfg, &report, &path_level)?;
    *path = work;
    Ok(report)
}

/// Artifact keys this pass rewrote: step id -> escaped old key -> escaped new
/// key. A key group is written after everything beneath it, so the pointers
/// already recorded under the old key can only be re-pointed once the loop
/// has finished.
type Renames = HashMap<String, HashMap<String, String>>;

/// `verify` bounds-checks each span on its own, so two spans that each land
/// inside the field can still overlap. Splicing them right-to-left leaves
/// part of the first credential in place while the report claims both were
/// replaced, which is the same class of hand-edited-plan defect as an empty
/// or inverted span and is refused in the same place.
fn reject_overlaps(group: &[&PlanFinding]) -> Result<()> {
    let mut ordered = group.to_vec();
    ordered.sort_by_key(|f| f.span);
    for pair in ordered.windows(2) {
        if pair[0].span.1 > pair[1].span.0 {
            return Err(RedactError::PlanMismatch(format!(
                "{} and {} overlap",
                pair[0].id, pair[1].id
            )));
        }
    }
    Ok(())
}

/// The steps the plan rewrites. Surfaces outside any step (`path.base`,
/// `meta.vcs_remote`) carry the empty step id and belong to no step.
fn touched_steps(plan: &Plan) -> BTreeSet<&str> {
    plan.findings
        .iter()
        .filter(|f| f.action == Action::Redact && !f.step.is_empty())
        .map(|f| f.step.as_str())
        .collect()
}

/// Every edit to one string has to be spliced in a single right-to-left
/// pass, so the findings are grouped by the field they land in; applying
/// them one field-visit at a time would invalidate the offsets of the edits
/// still pending.
///
/// Groups come back with each artifact's own key last within that artifact,
/// because writing the key renames the map entry and invalidates every
/// pointer built from the old one. That constraint is a structural fact
/// about the pointers themselves, so it is read off them rather than off
/// `plan.surfaces`, which a hand-edited or stale plan need not agree with.
fn group_by_field(plan: &Plan) -> Vec<((&str, &str), Vec<&PlanFinding>)> {
    let mut groups: BTreeMap<(&str, &str), Vec<&PlanFinding>> = BTreeMap::new();
    for f in plan.findings.iter().filter(|f| f.action == Action::Redact) {
        groups
            .entry((f.step.as_str(), f.at.as_str()))
            .or_default()
            .push(f);
    }

    let mut out: Vec<((&str, &str), Vec<&PlanFinding>)> = groups.into_iter().collect();
    out.sort_by(|(a, _), (b, _)| write_order(a).cmp(&write_order(b)));
    out
}

/// A field's place in the write order: its step, the artifact it sits under,
/// and last within that artifact whether it *is* that artifact's key.
fn write_order<'a>(field: &(&'a str, &'a str)) -> (&'a str, Option<&'a str>, bool) {
    let (step, at) = *field;
    match artifact_key_of(at) {
        Some((token, is_key)) => (step, Some(token), is_key),
        None => (step, None, false),
    }
}

/// The escaped artifact key a pointer is built from - the token right after
/// `/change/` - paired with whether the pointer *is* that key rather than
/// something beneath it. `None` for the document-level surfaces, which sit
/// under no artifact. Mirrors `surface::route`'s `/change/` arm.
fn artifact_key_of(at: &str) -> Option<(&str, bool)> {
    let rest = at.strip_prefix("/change/")?;
    let token = rest.split('/').next().unwrap_or(rest);
    (!token.is_empty()).then_some((token, token.len() == rest.len()))
}

/// Re-point one recorded pointer at the artifact key this pass renamed it
/// to. Recording the pointer a finding was found at would publish the old
/// key - credential and all - in the record sitting next to the document the
/// credential was just removed from, and dangle besides.
fn renamed(at: &str, renames: &HashMap<String, String>) -> Option<String> {
    let (token, _) = artifact_key_of(at)?;
    let fresh = renames.get(token)?;
    Some(format!(
        "/change/{fresh}{}",
        &at["/change/".len() + token.len()..]
    ))
}

/// Apply `renames` to every entry of one step's record. Two entries can only
/// collide here if two keys renamed alike, which `SurfaceCursor::write`
/// already refuses; summing is the same merge the map does everywhere else.
fn rewrite_pointers(
    entries: BTreeMap<RecordKey, usize>,
    renames: &HashMap<String, String>,
) -> BTreeMap<RecordKey, usize> {
    let mut out: BTreeMap<RecordKey, usize> = BTreeMap::new();
    for (mut key, n) in entries {
        if let Some(at) = renamed(&key.at, renames) {
            key.at = at;
        }
        *out.entry(key).or_default() += n;
    }
    out
}

/// Strip the signatures this pass invalidates, returning how many there
/// were.
///
/// The path's own signature covers the whole document, and a touched step's
/// covers content about to be rewritten. A step the plan never names keeps
/// its signature: nothing under it changed, so it still verifies.
fn guard_signatures(
    path: &mut toolpath::v1::Path,
    touched: &BTreeSet<&str>,
    cfg: &RedactConfig,
) -> Result<usize> {
    let total = path.meta.as_ref().map_or(0, |m| m.signatures.len())
        + path
            .steps
            .iter()
            .filter(|s| touched.contains(s.step.id.as_str()))
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
    for s in path
        .steps
        .iter_mut()
        .filter(|s| touched.contains(s.step.id.as_str()))
    {
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
    renames: &Renames,
) -> Result<()> {
    for (id, fresh) in records {
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
        // After merging, so an earlier pass's pointers are re-pointed too:
        // they were built from the key this pass has just renamed.
        if let Some(renames) = renames.get(id) {
            merged = rewrite_pointers(merged, renames);
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
    json!({ "v": RECORD_V, "findings": record_entries(entries) })
}

fn record_entries(entries: &BTreeMap<RecordKey, usize>) -> Vec<Value> {
    entries
        .iter()
        .map(|(k, n)| json!({ "rule": k.rule, "at": k.at, "n": n, "fp": k.fp, "op": k.op }))
        .collect()
}

/// The document-level rollup: what the whole document has had done to it,
/// across every pass. One accumulation policy throughout - counts sum, name
/// lists union, finding entries merge by identity - with a single exception,
/// `flagged`, which names findings still sitting in the document and is
/// therefore this pass's tally rather than a running total.
fn write_rollup(
    path: &mut toolpath::v1::Path,
    plan: &Plan,
    cfg: &RedactConfig,
    report: &RedactReport,
    path_level: &BTreeMap<RecordKey, usize>,
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

    let replaced = merge_counts(previous.as_ref(), "replaced", &report.replaced)?;
    let signatures_dropped =
        previous_u64(previous.as_ref(), "signatures_dropped")? + report.signatures_dropped as u64;
    let detectors = merge_names(previous.as_ref(), "detectors", &plan.detectors)?;

    // `path.base` and `meta.vcs_remote` sit under no step, so no step record
    // can name them - and they are exactly where a URI-embedded credential
    // lives. Without this the pass would leave no `at`, `fp` or `op` for
    // them at all.
    let mut findings = seed_from_existing(previous.as_ref())?;
    for (k, n) in path_level {
        *findings.entry(k.clone()).or_default() += n;
    }

    let meta = path.meta.get_or_insert_with(Default::default);
    meta.extra.insert(
        RECORD_KEY.into(),
        json!({
            "v": RECORD_V,
            "at": cfg.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "tool": concat!("toolpath-redact/", env!("CARGO_PKG_VERSION")),
            "detectors": detectors,
            // No `mode`: a rollup spanning two passes could only ever name
            // one of them, and every finding's own `op` already records
            // exactly what it got.
            "steps_touched": steps_touched,
            "replaced": replaced,
            "flagged": report.flagged,
            "signatures_dropped": signatures_dropped,
            "findings": record_entries(&findings),
        }),
    );
    Ok(())
}

/// The three readers below share one rule, the same one `seed_from_existing`
/// follows: a field the previous rollup never carried starts from nothing,
/// but a field it carries in a shape this cannot read is a corrupt record,
/// not a zero. Silently discarding it would make a second pass under-report
/// what the first one did.
fn merge_counts(
    previous: Option<&Value>,
    field: &str,
    fresh: &BTreeMap<String, usize>,
) -> Result<BTreeMap<String, u64>> {
    let mut out: BTreeMap<String, u64> = match previous.and_then(|p| p.get(field)) {
        None => BTreeMap::new(),
        Some(Value::Object(m)) => m
            .iter()
            .map(|(k, v)| {
                v.as_u64().map(|n| (k.clone(), n)).ok_or_else(|| {
                    RedactError::PlanMismatch(format!(
                        "redaction record {field}.{k} is not a count"
                    ))
                })
            })
            .collect::<Result<_>>()?,
        Some(_) => {
            return Err(RedactError::PlanMismatch(format!(
                "redaction record {field} is not an object"
            )));
        }
    };
    for (k, n) in fresh {
        *out.entry(k.clone()).or_default() += *n as u64;
    }
    Ok(out)
}

/// A union, so the rollup names every detector that has ever run over this
/// document rather than only the last one's. Sorted, because a union has no
/// inherent order to preserve.
fn merge_names(previous: Option<&Value>, field: &str, fresh: &[String]) -> Result<Vec<String>> {
    let mut out: BTreeSet<String> = match previous.and_then(|p| p.get(field)) {
        None => BTreeSet::new(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str().map(str::to_owned).ok_or_else(|| {
                    RedactError::PlanMismatch(format!(
                        "redaction record {field} holds a non-string"
                    ))
                })
            })
            .collect::<Result<_>>()?,
        Some(_) => {
            return Err(RedactError::PlanMismatch(format!(
                "redaction record {field} is not an array"
            )));
        }
    };
    out.extend(fresh.iter().cloned());
    Ok(out.into_iter().collect())
}

fn previous_u64(previous: Option<&Value>, field: &str) -> Result<u64> {
    match previous.and_then(|p| p.get(field)) {
        None => Ok(0),
        Some(v) => v.as_u64().ok_or_else(|| {
            RedactError::PlanMismatch(format!("redaction record {field} is not a count"))
        }),
    }
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
    // was redacted twice".

    static AWS_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"AKIA[0-9A-Z]{16}").unwrap());
    static GH_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"ghp_[A-Za-z0-9]{36}").unwrap());

    /// A URI password: delimited by what surrounds it rather than by a prefix
    /// of its own, so a transform's output can look like a fresh credential
    /// here. The two rules above cannot - no output of `apply` starts `AKIA`
    /// or `ghp_` - which is why an idempotence test built only on them cannot
    /// fail whatever `apply` does. Group 1 is the secret, as in `internal`.
    static URI_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"://[^:/@]+:([^@]+)@").unwrap());

    /// What this crate's own output looks like. Mirrors `internal::marker_re`,
    /// and for the same reason: without it a second pass redacts the first
    /// pass's marker. `Hash` is deliberately not in it - a bare digest is
    /// indistinguishable from a fresh secret (`hash_re_redacts_its_own_output`).
    static MARKER_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"\[REDACTED:[^\]\n]*\]|\u{2588}+|\S{1,8}\u{2026}\S{1,8}").unwrap()
    });

    fn mini_scan(text: &str) -> Vec<(Range<usize>, &'static str)> {
        let markers: Vec<Range<usize>> = MARKER_RE.find_iter(text).map(|m| m.range()).collect();
        let mut out: Vec<(Range<usize>, &'static str)> = AWS_RE
            .find_iter(text)
            .map(|m| (m.range(), "aws-access-key-id"))
            .chain(GH_RE.find_iter(text).map(|m| (m.range(), "github-pat")))
            .chain(URI_RE.captures_iter(text).map(|c| {
                (
                    c.get(1).expect("group 1 always participates").range(),
                    "uri-credential",
                )
            }))
            .filter(|(r, _)| !markers.iter().any(|m| r.start < m.end && r.end > m.start))
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

    /// `text` is the field the span indexes into: `verify` recomputes each
    /// fingerprint from the document, so a plan built with a placeholder is
    /// refused before `apply` does anything.
    fn finding(
        s: &Surface,
        text: &str,
        span: Range<usize>,
        rule: &str,
        action: Action,
    ) -> PlanFinding {
        PlanFinding {
            id: String::new(),
            step: s.step.clone(),
            at: s.at.clone(),
            rule: rule.into(),
            span: (span.start, span.end),
            fingerprint: crate::transform::Fingerprint::new(&cfg().key, &text[span]).0,
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
                    .map(move |(span, rule)| finding(s, text, span, rule, action))
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

    /// `scope` is one of the base schema's five (`author|reviewer|witness|
    /// ci|release`); a made-up scope would make every document this fixture
    /// produces invalid for reasons unrelated to redaction.
    fn signature() -> Signature {
        Signature {
            signer: "human:alex".into(),
            key: "ssh:SHA256:abc".into(),
            scope: "author".into(),
            sig: "base64sig".into(),
            timestamp: None,
        }
    }

    fn fixture_signed() -> Path {
        let mut p = fixture_with_secret(AWS_KEY);
        p.meta.as_mut().unwrap().signatures = vec![signature()];
        p
    }

    fn sign_step(path: &mut Path, id: &str) {
        let step = path.steps.iter_mut().find(|s| s.step.id == id).unwrap();
        step.meta.get_or_insert_with(Default::default).signatures = vec![signature()];
    }

    /// The credential sits in the artifact key itself, so redacting it
    /// renames the map entry every pointer beneath it is built from.
    fn fixture_with_secret_in_the_artifact_key(secret: &str, extra: Value) -> Path {
        let mut step = append_step("turn-0f3a", extra);
        let change = step.change.remove("claude://sess-abc").unwrap();
        step.change
            .insert(format!("claude://sess-{secret}"), change);
        doc(vec![step])
    }

    fn fixture_with_secret_in_vcs_remote(secret: &str) -> Path {
        let mut d = doc(vec![append_step("turn-0f3a", json!({ "text": "clean" }))]);
        d.meta.as_mut().unwrap().extra.insert(
            "vcs_remote".into(),
            json!(format!("https://{secret}@github.com/o/r.git")),
        );
        d
    }

    fn fixture_with_uri_credential() -> Path {
        doc(vec![append_step(
            "turn-0f3a",
            json!({ "text": "DB_PASSWORD_URL=postgres://svc:h0rr1bl3pass@db.internal:5432/prod" }),
        )])
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
        // Driven by `mini_scan`'s URI rule, the one whose secret is delimited
        // by its surroundings: a rule keyed off a fixed prefix could never see
        // a transform's output as a fresh credential, so a test built on those
        // alone cannot fail whatever `apply` does. What keeps these four
        // stable is that the scan recognises its own markers - `Hash` leaves
        // nothing to recognise and has its own test below.
        for mode in [
            Transform::Marker,
            Transform::Remove,
            Transform::Mask,
            Transform::Partial,
        ] {
            let cfg = RedactConfig { mode, ..cfg() };
            let mut once = fixture_with_uri_credential();
            let plan = plan_for(&once);
            let report = apply(&mut once, &plan, &cfg).unwrap();
            assert!(
                !report.replaced.is_empty(),
                "{mode:?}: nothing was redacted, the test proves nothing"
            );

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
    fn hash_re_redacts_its_own_output() {
        // `Hash` emits a bare digest, which no scan can tell from a fresh
        // credential, so the next pass redacts the digest and the record grows
        // an entry under a rotated `fp`. A known design gap. The two things
        // that must hold either way are asserted rather than the inequality:
        // the credential is still gone, and the growth is what marks the gap
        // as open.
        const PASSWORD: &str = "h0rr1bl3pass";
        let cfg = RedactConfig {
            mode: Transform::Hash,
            ..cfg()
        };

        let mut doc = fixture_with_uri_credential();
        let plan = plan_for(&doc);
        apply(&mut doc, &plan, &cfg).unwrap();
        let once = entry_count(record_of(&doc, "turn-0f3a"));

        let plan = plan_for(&doc);
        apply(&mut doc, &plan, &cfg).unwrap();
        let twice = entry_count(record_of(&doc, "turn-0f3a"));

        assert_no_substring(&serde_json::to_string(&doc).unwrap(), PASSWORD, "hash");
        assert!(
            twice > once,
            "the digest was not re-redacted: the gap has closed, retire this test ({once} -> {twice})"
        );
    }

    fn entry_count(record: &Value) -> usize {
        record["findings"]
            .as_array()
            .expect("a record always carries a findings array")
            .len()
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
        const SECRET: &str = "AKIAIOSFODNN7REALKEY";

        // Three placements, because the record's `at` is a pointer and one of
        // these builds that pointer out of the credential itself.
        for (label, mut d, on_a_step) in [
            ("prose", fixture_with_secret(SECRET), true),
            (
                "artifact key",
                fixture_with_secret_in_the_artifact_key(SECRET, json!({ "text": "clean" })),
                true,
            ),
            (
                "vcs_remote",
                fixture_with_secret_in_vcs_remote(SECRET),
                false,
            ),
        ] {
            let plan = plan_for(&d);
            apply(&mut d, &plan, &cfg()).unwrap();

            let rollup =
                serde_json::to_string(&d.meta.as_ref().unwrap().extra[RECORD_KEY]).unwrap();
            assert_no_substring(&rollup, SECRET, label);
            if !on_a_step {
                continue;
            }
            let rec = serde_json::to_string(record_of(&d, "turn-0f3a")).unwrap();
            assert_no_substring(&rec, SECRET, label);
            // Only the step record: the rollup carries a timestamp, whose year
            // shares digits with any two-digit length.
            assert!(!rec.contains(&SECRET.len().to_string()), "{label}: {rec}");
        }
    }

    fn assert_no_substring(haystack: &str, secret: &str, label: &str) {
        assert!(!haystack.contains(secret), "{label}: {haystack}");
        for w in 6..secret.len() {
            for s in secret.as_bytes().windows(w) {
                let sub = std::str::from_utf8(s).unwrap();
                assert!(!haystack.contains(sub), "{label} leaked {sub}: {haystack}");
            }
        }
    }

    #[test]
    fn redacting_an_artifact_key_and_a_field_beneath_it_both_land() {
        // Writing the key renames the map entry, so every pointer under the
        // old key stops resolving: the key has to go last.
        let mut d = fixture_with_secret_in_the_artifact_key(
            GH_PAT,
            json!({ "text": format!("and {AWS_KEY} too") }),
        );
        let plan = plan_for(&d);
        apply(&mut d, &plan, &cfg()).unwrap();

        let key = d.steps[0].change.keys().next().unwrap();
        assert_eq!(
            key,
            &format!("claude://sess-{}", marker(GH_PAT, "github-pat"))
        );
        assert_eq!(
            text_at(&d, "/text"),
            format!("and {} too", marker(AWS_KEY, "aws-access-key-id"))
        );
    }

    #[test]
    fn every_recorded_pointer_resolves_after_the_pass() {
        // A record's `at` is built from the artifact key, and this pass
        // renames that key. Recorded as found, every pointer beneath it both
        // dangles and republishes the credential - in the audit record sitting
        // beside the document the credential was just removed from.
        let mut d = fixture_with_secret_in_the_artifact_key(
            GH_PAT,
            json!({ "text": format!("and {AWS_KEY} too") }),
        );
        let plan = plan_for(&d);
        apply(&mut d, &plan, &cfg()).unwrap();

        let recorded = recorded_pointers(&d);
        assert!(
            recorded.len() >= 2,
            "the fixture must record both placements: {recorded:?}"
        );
        let mut probe = d.clone();
        let cursor = SurfaceCursor { path: &mut probe };
        for (step, at) in &recorded {
            assert!(
                cursor.read(step, at).is_some(),
                "{at:?} on step {step:?} does not resolve after the pass"
            );
        }

        let doc = serde_json::to_string(&d).unwrap();
        assert_no_substring(&doc, GH_PAT, "artifact key");
        assert_no_substring(&doc, AWS_KEY, "field beneath the artifact key");
    }

    /// Every `at` this pass recorded, paired with the step it was recorded on
    /// - the empty id for the rollup's own, which sit under no step.
    fn recorded_pointers(path: &Path) -> Vec<(String, String)> {
        fn ats(record: &Value) -> Vec<String> {
            record["findings"]
                .as_array()
                .expect("a record always carries a findings array")
                .iter()
                .map(|e| {
                    e["at"]
                        .as_str()
                        .expect("a record entry always carries at")
                        .to_string()
                })
                .collect()
        }
        let mut out = Vec::new();
        for s in &path.steps {
            if let Some(rec) = s.meta.as_ref().and_then(|m| m.extra.get(RECORD_KEY)) {
                out.extend(ats(rec).into_iter().map(|at| (s.step.id.clone(), at)));
            }
        }
        if let Some(rec) = path.meta.as_ref().and_then(|m| m.extra.get(RECORD_KEY)) {
            out.extend(ats(rec).into_iter().map(|at| (String::new(), at)));
        }
        out
    }

    #[test]
    fn a_mid_loop_failure_leaves_the_document_untouched() {
        let before = fixture_with_secrets();
        let mut after = before.clone();
        let mut plan = plan_for(&before);
        // The second step's group is reached only after the first step's has
        // already been written.
        let late = plan
            .findings
            .iter_mut()
            .find(|f| f.step == "turn-9c21")
            .unwrap();
        late.span = (late.span.0, late.span.0);

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
    fn zero_width_span_is_refused() {
        let before = fixture_with_secret(AWS_KEY);
        let mut after = before.clone();
        let mut plan = plan_for(&before);
        plan.findings[0].span = (plan.findings[0].span.0, plan.findings[0].span.0);
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
    fn empty_key_is_refused() {
        let before = fixture_with_secret(AWS_KEY);
        let mut after = before.clone();
        let cfg = RedactConfig {
            key: Vec::new(),
            ..cfg()
        };
        let err = apply(&mut after, &plan_for(&before), &cfg).unwrap_err();
        // The variant, not its wording: the message is for humans and free to
        // change, the refusal is the contract.
        assert!(matches!(err, RedactError::EmptyKey), "{err:?}");
        assert_eq!(
            serde_json::to_string(&before).unwrap(),
            serde_json::to_string(&after).unwrap()
        );
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
    fn record_sits_where_both_schemas_allow_it() {
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
    fn an_untouched_steps_signature_survives() {
        let mut d = fixture_with_secrets();
        sign_step(&mut d, "turn-9c21");
        // Only the first step's text carries a finding, so the second step's
        // content - and its signature - still verify.
        let cfg = RedactConfig {
            drop_signatures: true,
            ..cfg()
        };
        let plan = plan_for_step(&d, "turn-0f3a");
        let report = apply(&mut d, &plan, &cfg).unwrap();

        assert_eq!(report.signatures_dropped, 0);
        let kept = &d.steps[1].meta.as_ref().unwrap().signatures;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].sig, signature().sig);
    }

    #[test]
    fn a_touched_steps_signature_is_dropped_and_an_untouched_ones_is_not() {
        // Both steps signed, one redacted: the signature over rewritten
        // content cannot still verify, and the signature over content nobody
        // touched still can. Dropping both would destroy a valid attestation;
        // dropping neither would leave one that no longer checks out.
        let mut d = fixture_with_secrets();
        sign_step(&mut d, "turn-0f3a");
        sign_step(&mut d, "turn-9c21");
        let cfg = RedactConfig {
            drop_signatures: true,
            ..cfg()
        };
        let plan = plan_for_step(&d, "turn-0f3a");
        let report = apply(&mut d, &plan, &cfg).unwrap();

        assert_eq!(report.signatures_dropped, 1);
        assert!(
            d.steps[0].meta.as_ref().unwrap().signatures.is_empty(),
            "the redacted step's signature no longer covers its content"
        );
        assert_eq!(d.steps[1].meta.as_ref().unwrap().signatures.len(), 1);
    }

    /// A plan naming only `step`'s findings, leaving every other step
    /// untouched by the pass.
    fn plan_for_step(path: &Path, step: &str) -> Plan {
        plan_with(
            path,
            surfaces(path),
            scan_findings(path, Action::Redact)
                .into_iter()
                .filter(|f| f.step == step)
                .collect(),
        )
    }

    #[test]
    fn signed_step_without_a_signed_path_is_also_refused() {
        let mut d = fixture_with_secret(AWS_KEY);
        sign_step(&mut d, "turn-0f3a");
        d.meta.as_mut().unwrap().signatures.clear();
        let plan = plan_for(&d);
        assert!(matches!(
            apply(&mut d, &plan, &cfg()),
            Err(RedactError::SignedDocument)
        ));
    }

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
