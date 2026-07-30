//! `path p redact` — remove credentials from a toolpath document in place
//! via a reviewable plan-then-apply flow.

use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

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
pub(crate) fn run_with_picker(_args: RedactArgs, _picker: &dyn PickerStrategy) -> Result<()> {
    todo!("T9")
}

pub(crate) trait PickerStrategy {
    /// Rows are TSV with the finding id in column 1. Returns the selected
    /// rows verbatim, the way `fzf` does - not bare ids.
    ///
    /// Unused until `run_with_picker` stops being a `todo!()`.
    #[allow(dead_code)]
    fn pick(&self, rows: &[String]) -> Result<Vec<String>>;
}

pub(crate) struct RealPicker;

impl PickerStrategy for RealPicker {
    fn pick(&self, _rows: &[String]) -> Result<Vec<String>> {
        todo!("T9")
    }
}

/// Constructed by the dispatch tests, which arrive with `run_with_picker`.
#[cfg(test)]
#[allow(dead_code)]
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
#[cfg_attr(not(test), allow(dead_code))]
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
}
