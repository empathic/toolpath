//! `path target` — read or set where `path share` uploads.
//!
//! A top-level verb rather than a flag on `share` or a subcommand of
//! `auth`: it's a persistent setting, not a per-call option, and it is
//! not authentication. "Where do my shares go?" is a question people
//! ask without knowing which command owns the answer, so the command is
//! named after the thing itself.
//!
//! The setting and its resolution live in [`crate::target`]; this
//! module is only the CLI surface over them.

#![cfg(not(target_os = "emscripten"))]

use anyhow::Result;
use clap::Args;

use crate::store;
use crate::target::{self, Target};

#[derive(Args, Debug)]
pub struct TargetArgs {
    /// Where `path share` should upload: `pathbase`, an S3 bucket
    /// (`s3://bucket/prefix`), or a folder (`~/Dropbox/traces`,
    /// `/srv/traces`). Omit to print the current target and where it
    /// came from.
    #[arg(index = 1, value_name = "TARGET")]
    pub target: Option<String>,

    /// Forget the configured target, falling back to Pathbase
    #[arg(long, conflicts_with = "target")]
    pub clear: bool,

    /// Store the target without checking that it works. For a bucket
    /// you haven't created yet, or when you're offline.
    #[arg(long, conflicts_with = "clear")]
    pub no_verify: bool,
}

pub fn run(args: TargetArgs) -> Result<()> {
    if args.clear {
        let path = target::clear_default()?;
        println!("Share target cleared ({}).", path.display());
        println!("`path share` now uploads to Pathbase.");
        return Ok(());
    }

    let Some(spec) = args.target else {
        return print_current();
    };

    let parsed = Target::parse(&spec)?;

    // Prove it works before storing it. A target set once and used many
    // times is exactly the thing worth checking at the moment it's
    // chosen — storing an unusable one just defers the failure to the
    // middle of a share, where it costs a session pick and a
    // derivation. `--no-verify` is the escape hatch for the cases where
    // the user genuinely knows better.
    if !args.no_verify {
        let settings = store::effective_settings()?;
        target::verify(&parsed, &settings)?;
    }

    let path = target::set_default(&parsed)?;
    println!("Share target set to {parsed}");
    println!("  stored in: {}", path.display());
    if args.no_verify {
        println!("  (not verified)");
    }
    Ok(())
}

/// Answer "where does my next share go?" in one command, including why.
fn print_current() -> Result<()> {
    let (stored, path) = target::default_target()?;
    match &stored {
        Some(t) => println!("Share target: {t} (from {})", path.display()),
        None => println!("No share target configured."),
    }
    // The stored value isn't the whole story — an env var or the
    // built-in fallback may be what actually applies right now.
    println!("In effect now: {}", target::describe_effective()?);
    if stored.is_none() {
        println!();
        println!("Set one with:");
        println!("  path target s3://my-bucket/traces");
        println!("  path target ~/Dropbox/toolpath   # a folder needs no credentials");
        println!("  path target pathbase");
    }
    Ok(())
}
