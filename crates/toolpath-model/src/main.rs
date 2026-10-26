//! `toolpath-model`: generate `docs/model/model.json` from the repositories.
//!
//!     cargo run -p toolpath-model -- \
//!       --pathbase ../pathbase --pathbase-rev origin/wip/bdelanghe \
//!       --feedback http://clax.localhost/a/hennefgqrc50=docs/model/feedback/goldens-demo.threads.json

use anyhow::{Context, Result};
use clap::Parser;
use toolpath_model::{Inputs, Repo, check, counts, generate};

#[derive(Parser)]
#[command(
    about = "Generate the goldens object model from the toolpath (and pathbase) repositories"
)]
struct Args {
    /// The toolpath checkout to read.
    #[arg(long, default_value = ".")]
    toolpath: String,
    /// The toolpath revision to read.
    #[arg(long, default_value = "HEAD")]
    toolpath_rev: String,
    /// A pathbase checkout to read as well.
    #[arg(long)]
    pathbase: Option<String>,
    /// The pathbase revision to read.
    #[arg(long, default_value = "origin/wip/bdelanghe")]
    pathbase_rev: String,
    /// `<artifact url>=<Clax threads export>`, repeatable.
    #[arg(long)]
    feedback: Vec<String>,
    /// Where to write the model.
    #[arg(long, default_value = "docs/model/model.json")]
    out: String,
}

fn main() -> Result<()> {
    let a = Args::parse();
    let tp = Repo::open("toolpath", &a.toolpath, &a.toolpath_rev)?;
    let pb = a
        .pathbase
        .as_deref()
        .map(|d| Repo::open("pathbase", d, &a.pathbase_rev))
        .transpose()?;
    let mut feedback = vec![];
    for f in &a.feedback {
        let (url, path) = f.split_once('=').context("--feedback takes <url>=<file>")?;
        let text = std::fs::read_to_string(path).with_context(|| path.to_string())?;
        feedback.push((url.to_string(), serde_json::from_str(&text)?));
    }
    let model = generate(&Inputs {
        toolpath: &tp,
        pathbase: pb.as_ref(),
        feedback,
    })?;
    std::fs::write(&a.out, serde_json::to_string_pretty(&model)? + "\n")
        .with_context(|| a.out.clone())?;
    for (k, n) in counts(&model) {
        println!("{k}\t{n}");
    }
    for b in check(&model) {
        println!("rule broken: {b}");
    }
    Ok(())
}
