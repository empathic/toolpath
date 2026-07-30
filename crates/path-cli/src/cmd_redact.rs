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
    #[arg(long, default_value_t = 0.8)]
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

pub(crate) fn run(args: RedactArgs) -> Result<()> {
    todo!("T9")
}

pub(crate) trait PickerStrategy {
    fn pick(&self, rows: &[String]) -> Result<Vec<String>>;
}

pub(crate) struct RealPicker;

impl PickerStrategy for RealPicker {
    fn pick(&self, rows: &[String]) -> Result<Vec<String>> {
        todo!("T9")
    }
}

pub(crate) struct RecordingPicker {
    pub selection: Vec<String>,
    pub seen: std::cell::RefCell<Vec<String>>,
}

impl PickerStrategy for RecordingPicker {
    fn pick(&self, rows: &[String]) -> Result<Vec<String>> {
        *self.seen.borrow_mut() = rows.to_vec();
        Ok(self.selection.clone())
    }
}

/// Helper to parse a "PREDICATE:TRANSFORM" string.
/// Splits on the LAST `:` so predicates containing `:` still parse.
pub(crate) fn parse_mode_for(s: &str) -> Result<(String, TransformArg)> {
    let (pred, transform_str) = s
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("--mode-for format is PREDICATE:TRANSFORM, got: {}", s))?;

    let transform = match transform_str {
        "marker" => TransformArg::Marker,
        "remove" => TransformArg::Remove,
        "hash" => TransformArg::Hash,
        "mask" => TransformArg::Mask,
        "partial" => TransformArg::Partial,
        other => anyhow::bail!("unknown transform: {}", other),
    };

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
