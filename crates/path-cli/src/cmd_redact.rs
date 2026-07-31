//! `path p redact` — remove credentials from a toolpath document in place
//! via a reviewable plan-then-apply flow.

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use clap::Args;
use std::path::PathBuf;
use toolpath::v1::{Graph, PathOrRef};
use toolpath_redact::{
    Action, Decision, DetectorSet, Plan, PlanFinding, Predicate, RedactConfig, RedactionPolicy,
    Transform, parse_predicate,
};

#[derive(Debug, Args)]
pub(crate) struct RedactArgs {
    /// Cache id or file path.
    #[arg(short, long)]
    pub input: String,

    /// Write elsewhere instead of in place.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    #[arg(long, conflicts_with = "plan")]
    pub dry_run: bool,
    #[arg(long)]
    pub plan: Option<PathBuf>,
    /// Include real values in the plan (written 0600).
    #[arg(long)]
    pub reveal: bool,

    #[arg(long, value_name = "PREDICATE")]
    pub accept: Vec<String>,
    #[arg(long, value_name = "PREDICATE")]
    pub reject: Vec<String>,
    #[arg(long)]
    pub interactive: bool,
    #[arg(long, value_name = "PREDICATE:TRANSFORM")]
    pub mode_for: Vec<String>,

    #[arg(long, default_values = &["internal"])]
    pub detector: Vec<String>,
    /// Minimum score a finding needs before it is redacted (0.0-1.0).
    #[arg(long, default_value_t = 0.8, value_parser = parse_threshold)]
    pub threshold: f32,
    #[arg(long)]
    pub allow_network_detectors: bool,

    #[arg(long, value_enum, default_value_t = TransformArg::Marker)]
    pub mode: TransformArg,
    #[arg(long)]
    pub key_file: Option<PathBuf>,

    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub drop_signatures: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum TransformArg {
    Marker,
    Remove,
    Hash,
    Mask,
    Partial,
}

/// Scores are clamped to `0.0..=1.0`, so an out-of-range threshold silently
/// inverts the command: `-1` redacts everything, `5` and `NaN` redact
/// nothing. `RangeInclusive::contains` rejects NaN and infinity for free.
fn parse_threshold(s: &str) -> std::result::Result<f32, String> {
    let v: f32 = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    if !(0.0..=1.0).contains(&v) {
        return Err(format!("--threshold must be in 0.0..=1.0, got {v}"));
    }
    Ok(v)
}

pub(crate) fn run(args: RedactArgs) -> Result<()> {
    run_with_picker(args, &RealPicker)
}

/// The seam the interactive tests inject through. Mirrors
/// `cmd_resume::run_with_strategy`, which exists for the same reason: the
/// alternative is a process-global picker override that one test poisons
/// for the whole binary.
pub(crate) fn run_with_picker(args: RedactArgs, picker: &dyn PickerStrategy) -> Result<()> {
    let execution = execute(&args, picker, crate::sync::build_detectors)?;
    if args.dry_run && execution.pending > 0 {
        std::process::exit(1);
    }
    Ok(())
}

pub(crate) trait PickerStrategy {
    /// Rows are TSV with the finding id in column 1. Returns the selected
    /// rows verbatim, the way `fzf` does - not bare ids.
    fn pick(&self, rows: &[String]) -> Result<Vec<String>>;
}

pub(crate) struct RealPicker;

impl PickerStrategy for RealPicker {
    fn pick(&self, rows: &[String]) -> Result<Vec<String>> {
        #[cfg(not(target_os = "emscripten"))]
        {
            if !crate::fuzzy::available() {
                bail!("--interactive needs a TTY (or fzf on PATH / the embedded-picker feature)");
            }
            let opts = crate::fuzzy::PickOptions {
                multi: true,
                prompt: "redact> ",
                header: Some("TAB to toggle which findings to redact"),
                ..Default::default()
            };
            match crate::fuzzy::pick(rows, &opts)? {
                crate::fuzzy::PickResult::Selected(v) => Ok(v),
                crate::fuzzy::PickResult::NoMatch | crate::fuzzy::PickResult::Cancelled => {
                    Ok(Vec::new())
                }
            }
        }
        #[cfg(target_os = "emscripten")]
        {
            let _ = rows;
            bail!("--interactive is not supported on this build");
        }
    }
}

/// Constructed by the dispatch tests, which arrive with `run_with_picker`.
#[cfg(test)]
pub(crate) struct RecordingPicker {
    pub selection: Vec<String>,
    pub seen: std::cell::RefCell<Vec<String>>,
}

#[cfg(test)]
impl PickerStrategy for RecordingPicker {
    fn pick(&self, rows: &[String]) -> Result<Vec<String>> {
        *self.seen.borrow_mut() = rows.to_vec();
        Ok(self.selection.clone())
    }
}

/// Splits on the LAST `:` so a predicate containing `:` still parses, e.g.
/// `detector=exec:/bin/gitleaks:hash`.
pub(crate) fn parse_mode_for(s: &str) -> Result<(String, TransformArg)> {
    let (pred, transform_str) = s
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("--mode-for format is PREDICATE:TRANSFORM, got: {}", s))?;
    if pred.is_empty() {
        anyhow::bail!(
            "--mode-for needs a predicate before the transform, got: {}",
            s
        );
    }
    // Resolved through clap's own value table rather than a second hand-rolled
    // one, which would silently drift from `TransformArg` as variants are added.
    let transform = <TransformArg as clap::ValueEnum>::from_str(transform_str, false)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    Ok((pred.to_string(), transform))
}

impl From<TransformArg> for toolpath_redact::Transform {
    fn from(arg: TransformArg) -> Self {
        match arg {
            TransformArg::Marker => toolpath_redact::Transform::Marker,
            TransformArg::Remove => toolpath_redact::Transform::Remove,
            TransformArg::Hash => toolpath_redact::Transform::Hash,
            TransformArg::Mask => toolpath_redact::Transform::Mask,
            TransformArg::Partial => toolpath_redact::Transform::Partial,
        }
    }
}

// ── dispatch (T9) ───────────────────────────────────────────────────────

/// How `execute` builds the detector set named by `--detector`. A parameter
/// (mirroring `sync::engine`'s own `DetectorFactory`) so tests can swap in a
/// deterministic registry instead of the vendored ruleset.
type DetectorFactory = fn(&[String]) -> Result<DetectorSet>;

/// What one run produced, independent of whether it wrote anything —
/// `run_with_picker` reads `pending` to decide the dry-run exit code; tests
/// read `plans` to inspect the final per-finding decisions.
struct Execution {
    /// Read only by tests, which inspect final per-finding decisions;
    /// production only needs `pending` for the dry-run exit code.
    #[cfg_attr(not(test), allow(dead_code))]
    plans: Vec<Plan>,
    pending: usize,
}

fn execute(
    args: &RedactArgs,
    picker: &dyn PickerStrategy,
    build_detectors: DetectorFactory,
) -> Result<Execution> {
    let (cache_id, doc_path) = resolve_input(&args.input)?;
    let mut doc = crate::io::read_document_auto(&doc_path)?;

    let key = resolve_key(args, cache_id.as_deref(), &doc)?;
    let cfg = RedactConfig {
        threshold: args.threshold,
        mode: args.mode.into(),
        // Per-finding overrides ride the plan via `--mode-for` decisions
        // below; nothing here needs the rule-keyed config fallback.
        mode_for: Vec::new(),
        key,
        now: Utc::now(),
        drop_signatures: args.drop_signatures,
        reveal: args.reveal,
    };

    let mode_for = resolve_mode_for(&args.mode_for)?;
    let mut decisions = mode_for_decisions(&mode_for);
    decisions.extend(predicate_decisions(&args.accept, Action::Redact)?);
    // Reject lands last: `apply_decisions` lets later entries win, so an
    // explicit skip survives a broader accept or mode-for (matches
    // `sync::engine::policy_decisions`).
    decisions.extend(predicate_decisions(&args.reject, Action::Skip)?);

    let mut plans: Vec<(usize, Plan)> = Vec::new();
    if let Some(plan_file) = &args.plan {
        let text = std::fs::read_to_string(plan_file)
            .with_context(|| format!("read {}", plan_file.display()))?;
        let plan: Plan = serde_json::from_str(&text)
            .with_context(|| format!("parse {}", plan_file.display()))?;
        let idx = doc
            .paths
            .iter()
            .position(|p| matches!(p, PathOrRef::Path(pp) if pp.path.id == plan.document))
            .ok_or_else(|| {
                anyhow!(
                    "plan targets document {:?}, which is not in {}",
                    plan.document,
                    doc_path.display()
                )
            })?;
        plans.push((idx, plan));
    } else {
        let detectors = build_detectors(&args.detector)?;
        for (idx, entry) in doc.paths.iter().enumerate() {
            if let PathOrRef::Path(path) = entry {
                let plan = toolpath_redact::plan::generate_checked(
                    path,
                    &detectors,
                    &cfg,
                    args.allow_network_detectors,
                )?;
                plans.push((idx, plan));
            }
        }
    }

    for (_, plan) in &mut plans {
        toolpath_redact::plan::apply_decisions(plan, &decisions);
    }
    if args.interactive {
        run_interactive(&mut plans, picker)?;
    }

    let pending: usize = plans
        .iter()
        .map(|(_, p)| {
            p.findings
                .iter()
                .filter(|f| f.action == Action::Redact)
                .count()
        })
        .sum();

    if args.dry_run {
        print_dry_run(args, &plans)?;
        return Ok(Execution {
            plans: plans.into_iter().map(|(_, p)| p).collect(),
            pending,
        });
    }

    let mut total_replaced = 0usize;
    let mut total_signatures_dropped = 0usize;
    for (idx, plan) in &plans {
        let PathOrRef::Path(path) = &mut doc.paths[*idx] else {
            unreachable!("index came from a PathOrRef::Path match above");
        };
        let report = toolpath_redact::apply(path, plan, &cfg)?;
        total_replaced += report.replaced.values().sum::<usize>();
        total_signatures_dropped += report.signatures_dropped;
    }
    let changed = total_replaced > 0 || total_signatures_dropped > 0;
    // Informational only: file inputs with no `--output` write the
    // redacted document itself to stdout (so `path p redact --input
    // doc.json | path p export pathbase --input -` composes), so this
    // summary must never share that stream.
    eprintln!("{total_replaced} finding(s) redacted");

    // Re-serialising an unchanged document is not a no-op: `extra` and
    // `change` are `#[serde(flatten)]` HashMaps whose iteration order is
    // seeded per process, so writing would churn the file's key order and
    // break "redacting twice is byte-identical to redacting once". A file
    // input with no `--output` still emits, since stdout is the result.
    if changed || cache_id.is_none() {
        write_output(args, cache_id.as_deref(), &doc)?;
    }

    if let (Some(id), Some((_, plan))) = (&cache_id, plans.first()) {
        let policy = RedactionPolicy {
            detectors: plan.detectors.clone(),
            threshold: plan.defaults.threshold,
            mode: plan.defaults.transform,
            mode_for: rule_mode_for(&mode_for),
            accept: args.accept.clone(),
            reject: args.reject.clone(),
            key_id: id.clone(),
        };
        crate::sync::record_redaction_policy(id, &policy)?;
    }

    Ok(Execution {
        plans: plans.into_iter().map(|(_, p)| p).collect(),
        pending,
    })
}

/// Mirrors the file-vs-cache-id heuristic `cache::cache_ref` applies
/// internally, so one `--input` string is never classified two different
/// ways by the two callers that need to know which it is.
fn is_file_ref(s: &str) -> bool {
    s.contains('/') || s.contains('\\') || s.ends_with(".json")
}

fn resolve_input(input: &str) -> Result<(Option<String>, PathBuf)> {
    let path = crate::cache::cache_ref(input)?;
    let cache_id = (!is_file_ref(input)).then(|| input.to_string());
    Ok((cache_id, path))
}

/// Mirrors `toolpath_redact::apply`'s private record key ("redaction");
/// there is no export for it, so the literal here must track that crate's
/// contract rather than being renamed independently.
const REDACTION_RECORD_KEY: &str = "redaction";

fn already_redacted(doc: &Graph) -> bool {
    doc.paths.iter().any(|entry| match entry {
        PathOrRef::Path(path) => {
            path.meta
                .as_ref()
                .is_some_and(|m| m.extra.contains_key(REDACTION_RECORD_KEY))
                || path.steps.iter().any(|s| {
                    s.meta
                        .as_ref()
                        .is_some_and(|m| m.extra.contains_key(REDACTION_RECORD_KEY))
                })
        }
        PathOrRef::Ref(_) => false,
    })
}

/// HMAC-SHA256 is block-size independent; 32 bytes is well past the point
/// where key length stops mattering (mirrors `cache::REDACT_KEY_LEN`,
/// private to that module).
const EPHEMERAL_KEY_LEN: usize = 32;

/// A cache id's key is stored and reused across runs; a file input with no
/// `--key-file` has nowhere durable to keep one, so each run mints its own
/// and fingerprints won't correlate across runs for that input alone.
fn resolve_key(args: &RedactArgs, cache_id: Option<&str>, doc: &Graph) -> Result<Vec<u8>> {
    if let Some(key_file) = &args.key_file {
        return std::fs::read(key_file).with_context(|| format!("read {}", key_file.display()));
    }
    let Some(id) = cache_id else {
        return Ok(random_key());
    };
    if already_redacted(doc) {
        return crate::cache::read_redact_key(id)?.ok_or_else(|| {
            anyhow!(
                "redaction key for {id} is missing; refusing to re-redact with a fresh key, \
                 which would silently break correlation with the existing markers"
            )
        });
    }
    crate::cache::load_or_create_redact_key(id)
}

fn random_key() -> Vec<u8> {
    use rand::RngCore;
    let mut key = vec![0u8; EPHEMERAL_KEY_LEN];
    rand::rng().fill_bytes(&mut key);
    key
}

fn resolve_mode_for(specs: &[String]) -> Result<Vec<(Predicate, Transform)>> {
    specs
        .iter()
        .map(|s| {
            let (pred, transform) = parse_mode_for(s)?;
            let predicate = parse_predicate(&pred).map_err(|e| anyhow!("{e}"))?;
            Ok((predicate, transform.into()))
        })
        .collect()
}

fn mode_for_decisions(mode_for: &[(Predicate, Transform)]) -> Vec<Decision> {
    mode_for
        .iter()
        .map(|(predicate, transform)| Decision {
            predicate: predicate.clone(),
            action: Action::Redact,
            transform: Some(*transform),
        })
        .collect()
}

/// Only `rule=`-shaped `--mode-for` entries survive into the persisted
/// policy: `RedactionPolicy.mode_for` rides `RedactConfig.mode_for`, which
/// `toolpath_redact::transform::resolve_transform` resolves by rule-name
/// equality only. Anything else (`shape=`, `at=`, `score`, …) is a
/// one-time choice for this run — the same asymmetry the accept/reject
/// replay already documents for an individually hand-picked finding.
fn rule_mode_for(mode_for: &[(Predicate, Transform)]) -> Vec<(String, Transform)> {
    mode_for
        .iter()
        .filter_map(|(predicate, transform)| match predicate {
            Predicate::Rule(name) => Some((name.clone(), *transform)),
            _ => None,
        })
        .collect()
}

fn predicate_decisions(specs: &[String], action: Action) -> Result<Vec<Decision>> {
    specs
        .iter()
        .map(|s| {
            Ok(Decision {
                predicate: parse_predicate(s).map_err(|e| anyhow!("{e}"))?,
                action,
                transform: None,
            })
        })
        .collect()
}

/// Interactive review is authoritative: every finding's action is set from
/// the picker's selection, overriding whatever `--accept`/`--reject`/
/// `--mode-for` decided, since a human is now looking at each one directly.
fn run_interactive(plans: &mut [(usize, Plan)], picker: &dyn PickerStrategy) -> Result<()> {
    let rows: Vec<String> = plans
        .iter()
        .flat_map(|(_, p)| p.findings.iter())
        .map(|f| format!("{}\t{}\t{}\t{}", f.id, f.rule, f.step, f.at))
        .collect();
    let selected = picker.pick(&rows)?;
    let accepted: std::collections::HashSet<&str> = selected
        .iter()
        .map(|row| row.split('\t').next().unwrap_or(row.as_str()))
        .collect();
    for (_, plan) in plans.iter_mut() {
        for finding in &mut plan.findings {
            finding.action = if accepted.contains(finding.id.as_str()) {
                Action::Redact
            } else {
                Action::Skip
            };
        }
    }
    Ok(())
}

fn print_dry_run(args: &RedactArgs, plans: &[(usize, Plan)]) -> Result<()> {
    if args.json {
        let payload: Vec<&Plan> = plans.iter().map(|(_, p)| p).collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    for (_, plan) in plans {
        println!("document {}", plan.document);
        for surface in &plan.surfaces {
            let hits: Vec<&PlanFinding> = plan
                .findings
                .iter()
                .filter(|f| f.step == surface.step && f.at == surface.at)
                .collect();
            if hits.is_empty() {
                println!(
                    "  {} {} ({} bytes): no findings",
                    surface.step, surface.at, surface.bytes
                );
            } else {
                for f in hits {
                    println!(
                        "  {} {}: {} (score {:.2}, {:?})",
                        surface.step, surface.at, f.rule, f.score, f.action
                    );
                }
            }
        }
    }
    Ok(())
}

fn write_output(args: &RedactArgs, cache_id: Option<&str>, doc: &Graph) -> Result<()> {
    if let Some(id) = cache_id {
        if args.output.is_some() {
            bail!("--output is not compatible with a cache id input; `{id}` redacts in place");
        }
        crate::cache::write_cached(id, doc, true)?;
        return Ok(());
    }
    let json = doc.to_json_pretty()?;
    match &args.output {
        Some(path) => {
            std::fs::write(path, json).with_context(|| format!("write {}", path.display()))?
        }
        None => println!("{json}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn try_parse(args: &[&str]) -> Result<RedactArgs> {
        use clap::Parser;
        #[derive(Parser)]
        struct TestCli {
            #[command(subcommand)]
            p: PCommand,
        }
        #[derive(clap::Subcommand)]
        enum PCommand {
            Redact {
                #[command(flatten)]
                args: RedactArgs,
            },
        }

        let mut argv = vec!["test"];
        argv.extend(args);
        let cli = TestCli::try_parse_from(argv)?;
        match cli.p {
            PCommand::Redact { args } => Ok(args),
        }
    }

    #[test]
    fn dry_run_conflicts_with_plan() {
        assert!(try_parse(&["redact", "-i", "x", "--dry-run", "--plan", "p.json"]).is_err());
    }

    #[test]
    fn mode_for_rejects_unknown_transform() {
        assert!(parse_mode_for("rule=x:invented").is_err());
    }

    #[test]
    fn detector_flag_is_repeatable() {
        let a = try_parse(&[
            "redact",
            "-i",
            "x",
            "--detector",
            "internal",
            "--detector",
            "exec:/bin/s",
        ])
        .unwrap();
        assert_eq!(a.detector.len(), 2);
    }

    #[test]
    fn threshold_rejects_non_numeric() {
        assert!(try_parse(&["redact", "-i", "x", "--threshold", "abc"]).is_err());
    }

    #[test]
    fn mode_rejects_unknown_transform() {
        assert!(try_parse(&["redact", "-i", "x", "--mode", "unknown"]).is_err());
    }

    #[test]
    fn input_is_required() {
        assert!(try_parse(&["redact"]).is_err());
    }

    #[test]
    fn parse_mode_for_without_colon() {
        assert!(parse_mode_for("rule=x").is_err());
    }

    #[test]
    fn parse_mode_for_with_colon_in_predicate() {
        let (pred, transform) = parse_mode_for("at=/change/claude:~1~1sess:marker").unwrap();
        assert_eq!(pred, "at=/change/claude:~1~1sess");
        assert_eq!(transform, TransformArg::Marker);
    }

    #[test]
    fn all_transform_args_convert_to_distinct_transforms() {
        let transforms: Vec<_> = vec![
            TransformArg::Marker,
            TransformArg::Remove,
            TransformArg::Hash,
            TransformArg::Mask,
            TransformArg::Partial,
        ]
        .into_iter()
        .map(|t| {
            let converted: toolpath_redact::Transform = t.into();
            converted
        })
        .collect();
        for i in 0..transforms.len() {
            for j in (i + 1)..transforms.len() {
                assert_ne!(
                    transforms[i], transforms[j],
                    "transform variants must map to distinct Transform values"
                );
            }
        }
    }

    // ── dispatch (Step 9.1) ──────────────────────────────────────────────
    //
    // The plan's `sandbox()`/`run_redact()`/`run_sync()` helpers don't
    // exist anywhere in this codebase (Task 10's report against
    // `sync/engine.rs` found the same); these tests drive `execute`
    // directly through the existing `$TOOLPATH_CONFIG_DIR`-sandboxing
    // `with_cfg` convention (see `cache.rs`, `sync/engine.rs`) instead.

    use crate::config::{CONFIG_DIR_ENV, TEST_ENV_LOCK};
    use std::collections::HashMap;
    use toolpath::v1::{ArtifactChange, StepIdentity, StructuralChange};

    fn with_cfg<F: FnOnce(&std::path::Path) -> R, R>(f: F) -> R {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var(CONFIG_DIR_ENV, temp.path());
        }
        let result = f(temp.path());
        unsafe {
            std::env::remove_var(CONFIG_DIR_ENV);
        }
        result
    }

    /// A detector matching literal `SECRET-<alnum run>` markers, so redact
    /// fixtures are deterministic and don't depend on the vendored
    /// ruleset's regex/entropy behaviour (mirrors the equivalent test
    /// detector in `sync/engine.rs`).
    struct LiteralSecret;

    impl toolpath_redact::Detector for LiteralSecret {
        fn id(&self) -> &'static str {
            "literal"
        }

        fn detect(
            &self,
            c: &toolpath_redact::Candidate<'_>,
        ) -> toolpath_redact::Result<Vec<toolpath_redact::Finding>> {
            Ok(c.text
                .match_indices("SECRET-")
                .map(|(start, m)| {
                    let tail = &c.text[start + m.len()..];
                    let run = tail
                        .find(|ch: char| !ch.is_ascii_alphanumeric())
                        .unwrap_or(tail.len());
                    toolpath_redact::Finding {
                        span: start..start + m.len() + run,
                        rule: "literal".to_string(),
                        score: 1.0,
                        detector: "literal",
                    }
                })
                .collect())
        }
    }

    fn literal_detectors(_names: &[String]) -> Result<DetectorSet> {
        let mut set = DetectorSet::default();
        set.push(Box::new(LiteralSecret));
        Ok(set)
    }

    /// One `conversation.append` step per `(id, text)` pair, addressable at
    /// `/change/convo/structural/extra/text` — the surface `toolpath_redact`
    /// generates a finding on when `text` contains a `SECRET-` marker.
    fn literal_secret_doc(steps: &[(&str, &str)]) -> Graph {
        let mut path =
            toolpath::v1::Path::new("doc-1", None, steps.last().expect("at least one step").0);
        for (id, text) in steps {
            let mut extra = HashMap::new();
            extra.insert(
                "text".to_string(),
                serde_json::Value::String((*text).to_string()),
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
            path.steps.push(toolpath::v1::Step {
                step: StepIdentity {
                    id: (*id).to_string(),
                    parents: Vec::new(),
                    actor: "human:t".to_string(),
                    timestamp: "2026-01-01T00:00:00Z".to_string(),
                },
                change,
                meta: None,
            });
        }
        Graph::from_path(path)
    }

    fn three_findings_fixture() -> Graph {
        literal_secret_doc(&[
            ("s1", "alpha SECRET-A here"),
            ("s2", "beta SECRET-B here"),
            ("s3", "gamma SECRET-C here"),
        ])
    }

    fn seed_cached_document(id: &str) {
        let doc = literal_secret_doc(&[("s1", "here is SECRET-A now")]);
        crate::cache::write_cached(id, &doc, true).unwrap();
    }

    fn reseed_same_document(id: &str) {
        seed_cached_document(id);
    }

    fn read_cached(id: &str) -> String {
        std::fs::read_to_string(crate::cache::cache_path(id).unwrap()).unwrap()
    }

    #[cfg(unix)]
    fn mode_of(p: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    /// Structural, not textual: walks the applied document's per-step
    /// `meta.redaction.findings[].fp` entries directly, so this doesn't
    /// need a `regex` dependency (optional in this crate, gated behind
    /// the `embedded-picker` feature) just to scrape a fixture.
    fn fingerprints_in(json: &str) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let mut out = Vec::new();
        if let Some(paths) = v.get("paths").and_then(|p| p.as_array()) {
            for p in paths {
                if let Some(steps) = p.get("steps").and_then(|s| s.as_array()) {
                    for step in steps {
                        if let Some(findings) = step
                            .pointer(&format!("/meta/{REDACTION_RECORD_KEY}/findings"))
                            .and_then(|f| f.as_array())
                        {
                            for f in findings {
                                if let Some(fp) = f.get("fp").and_then(|x| x.as_str()) {
                                    out.push(fp.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        out.sort();
        out
    }

    fn redacted_ids(plans: &[Plan]) -> Vec<String> {
        plans
            .iter()
            .flat_map(|p| p.findings.iter())
            .filter(|f| f.action == Action::Redact)
            .map(|f| f.id.clone())
            .collect()
    }

    struct NoPicker;
    impl PickerStrategy for NoPicker {
        fn pick(&self, _rows: &[String]) -> Result<Vec<String>> {
            panic!("picker invoked without --interactive");
        }
    }

    /// `run_redact(&["-i", ...])` from the plan, adapted: no `sandbox()`
    /// return value to thread through, since `with_cfg` already pins
    /// `$TOOLPATH_CONFIG_DIR` for the closure's duration. Returns the exit
    /// code `run_with_picker` would signal via `std::process::exit`, which
    /// a unit test cannot observe directly.
    fn run_redact(args: &[&str]) -> Result<i32> {
        let mut argv = vec!["redact"];
        argv.extend_from_slice(args);
        let parsed = try_parse(&argv)?;
        let execution = execute(&parsed, &NoPicker, literal_detectors)?;
        Ok(if parsed.dry_run && execution.pending > 0 {
            1
        } else {
            0
        })
    }

    #[test]
    fn cache_input_rewrites_in_place() {
        with_cfg(|_| {
            seed_cached_document("claude-abc123");
            run_redact(&["-i", "claude-abc123"]).unwrap();
            assert!(read_cached("claude-abc123").contains("[REDACTED:"));
            #[cfg(unix)]
            assert_eq!(
                mode_of(&crate::cache::cache_path("claude-abc123").unwrap()),
                0o600
            );
        });
    }

    #[test]
    fn key_is_generated_once_and_reused() {
        with_cfg(|_| {
            seed_cached_document("claude-abc123");
            run_redact(&["-i", "claude-abc123"]).unwrap();
            let first = fingerprints_in(&read_cached("claude-abc123"));
            reseed_same_document("claude-abc123");
            run_redact(&["-i", "claude-abc123"]).unwrap();
            assert_eq!(first, fingerprints_in(&read_cached("claude-abc123")));
        });
    }

    #[test]
    fn interactive_uses_the_injected_picker() {
        with_cfg(|root| {
            let doc = root.join("doc.json");
            std::fs::write(&doc, three_findings_fixture().to_json_pretty().unwrap()).unwrap();
            let picker = RecordingPicker {
                selection: vec!["f01".into()],
                seen: Default::default(),
            };
            let parsed =
                try_parse(&["redact", "-i", doc.to_str().unwrap(), "--interactive"]).unwrap();
            let out = execute(&parsed, &picker, literal_detectors).unwrap().plans;
            assert_eq!(picker.seen.borrow().len(), 3);
            assert_eq!(redacted_ids(&out), vec!["f01".to_string()]);
        });
    }

    #[test]
    fn dry_run_exits_one_when_findings_exist() {
        with_cfg(|root| {
            let doc = root.join("doc.json");
            std::fs::write(&doc, three_findings_fixture().to_json_pretty().unwrap()).unwrap();
            assert_eq!(
                run_redact(&["-i", doc.to_str().unwrap(), "--dry-run"]).unwrap(),
                1
            );
        });
    }
}
