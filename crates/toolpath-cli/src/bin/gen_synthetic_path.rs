//! Synthetic `Path` generator for desktop Preview benchmarks (issue #41).
//!
//! Emits a `.path.json` with N steps that approximate a Claude session:
//!
//! - ~70% linear `conversation.append` turns alternating user / assistant
//! - ~20% Edit / Write tool invocations (sibling children of the assistant)
//! - ~10% MultiEdit tool invocations
//!
//! Steps are deterministic given a seed so benches are comparable across runs.
//!
//! Usage:
//!
//! ```text
//! cargo run -p toolpath-cli --bin gen_synthetic_path -- \
//!     --steps 10000 --out bench/fixtures/synthetic-10k.path.json
//! ```
//!
//! The output is not intended to be semantically coherent — it's just big,
//! well-shaped JSON matching what the Preview's normalize/flattenTree code
//! actually walks.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use rand::{Rng, SeedableRng, rngs::StdRng};
use serde_json::{Value, json};
use toolpath::v1::{
    ActorDefinition, ArtifactChange, Base, Document, Path, PathIdentity, PathMeta, Step, StepMeta,
    StructuralChange,
};

#[derive(Parser, Debug)]
#[command(name = "gen_synthetic_path")]
#[command(about = "Generate a synthetic Toolpath Path document for benchmarking")]
struct Args {
    /// Number of steps to generate.
    #[arg(long, default_value_t = 1_000)]
    steps: usize,

    /// Output file path (parent dirs are created).
    #[arg(long)]
    out: PathBuf,

    /// Deterministic seed.
    #[arg(long, default_value_t = 42)]
    seed: u64,
}

const LOREM: &[&str] = &[
    "lorem ipsum dolor sit amet consectetur adipiscing elit",
    "sed do eiusmod tempor incididunt ut labore et dolore magna aliqua",
    "ut enim ad minim veniam quis nostrud exercitation ullamco laboris",
    "duis aute irure dolor in reprehenderit in voluptate velit esse",
    "excepteur sint occaecat cupidatat non proident sunt in culpa",
    "at vero eos et accusamus et iusto odio dignissimos ducimus",
    "nemo enim ipsam voluptatem quia voluptas sit aspernatur aut odit",
    "sed ut perspiciatis unde omnis iste natus error sit voluptatem",
];

const TOOLS: &[(&str, f64)] = &[("Edit", 0.50), ("Write", 0.30), ("MultiEdit", 0.20)];

const FILES: &[&str] = &[
    "src/main.rs",
    "src/lib.rs",
    "src/server.rs",
    "src/store.rs",
    "src/routes/index.ts",
    "src/routes/api.ts",
    "src/lib/tree.ts",
    "src/lib/viz.ts",
    "Cargo.toml",
    "package.json",
    "README.md",
];

fn lorem_block(rng: &mut StdRng, sentences: usize) -> String {
    (0..sentences)
        .map(|_| LOREM[rng.random_range(0..LOREM.len())])
        .collect::<Vec<_>>()
        .join(". ")
}

fn pick_tool(rng: &mut StdRng) -> &'static str {
    let r: f64 = rng.random();
    let mut acc = 0.0;
    for (name, w) in TOOLS {
        acc += w;
        if r < acc {
            return name;
        }
    }
    TOOLS[0].0
}

fn synth_diff(rng: &mut StdRng, path: &str) -> String {
    let lines = rng.random_range(3..12);
    let mut s = format!(
        "--- a/{}\n+++ b/{}\n@@ -1,{} +1,{} @@\n",
        path, path, lines, lines
    );
    for i in 0..lines {
        if rng.random_bool(0.5) {
            s.push_str(&format!("-old_line_{} = value;\n", i));
            s.push_str(&format!("+new_line_{} = value;\n", i));
        } else {
            s.push_str(&format!(" context_line_{};\n", i));
        }
    }
    s
}

fn assistant_step(i: usize, parent: Option<&str>, rng: &mut StdRng) -> Step {
    let sentences = rng.random_range(1..5);
    let text = lorem_block(rng, sentences);
    let mut extra = HashMap::new();
    extra.insert("role".into(), Value::String("assistant".into()));
    extra.insert("text".into(), Value::String(text));
    extra.insert("model".into(), Value::String("claude-opus-4-6".into()));

    let mut s = Step::new(
        format!("step-{:06}", i),
        "agent:claude-code",
        format!("2026-04-22T10:{:02}:{:02}Z", (i / 60) % 60, i % 60),
    );
    if let Some(p) = parent {
        s = s.with_parent(p);
    }
    s.change.insert(
        "conversation".into(),
        ArtifactChange {
            raw: None,
            structural: Some(StructuralChange {
                change_type: "conversation.append".into(),
                extra,
            }),
        },
    );
    s.meta = Some(StepMeta {
        intent: Some(format!("assistant turn {}", i)),
        ..Default::default()
    });
    s
}

fn user_step(i: usize, parent: Option<&str>, rng: &mut StdRng) -> Step {
    let sentences = rng.random_range(1..3);
    let text = lorem_block(rng, sentences);
    let mut extra = HashMap::new();
    extra.insert("role".into(), Value::String("user".into()));
    extra.insert("text".into(), Value::String(text));

    let mut s = Step::new(
        format!("step-{:06}", i),
        "human:bench",
        format!("2026-04-22T10:{:02}:{:02}Z", (i / 60) % 60, i % 60),
    );
    if let Some(p) = parent {
        s = s.with_parent(p);
    }
    s.change.insert(
        "conversation".into(),
        ArtifactChange {
            raw: None,
            structural: Some(StructuralChange {
                change_type: "conversation.append".into(),
                extra,
            }),
        },
    );
    s
}

fn tool_step(i: usize, parent: &str, rng: &mut StdRng) -> Step {
    let name = pick_tool(rng);
    let file = FILES[rng.random_range(0..FILES.len())];

    let mut extra = HashMap::new();
    extra.insert("name".into(), Value::String(name.into()));
    extra.insert("tool_id".into(), Value::String(format!("tool-{}", i)));

    let mut s = Step::new(
        format!("step-{:06}", i),
        "agent:claude-code",
        format!("2026-04-22T10:{:02}:{:02}Z", (i / 60) % 60, i % 60),
    )
    .with_parent(parent);

    // Conversation-side marker (the tool.invoke payload).
    s.change.insert(
        "conversation".into(),
        ArtifactChange {
            raw: None,
            structural: Some(StructuralChange {
                change_type: "tool.invoke".into(),
                extra,
            }),
        },
    );

    // File artifact with a raw diff (mirrors what `toolpath-convo` emits for
    // file-write tools) — this is the payload `diff.raw.split("\n")` chews on.
    let raw = synth_diff(rng, file);
    let mut file_extra = HashMap::new();
    file_extra.insert("tool".into(), Value::String(name.into()));
    file_extra.insert("tool_id".into(), Value::String(format!("tool-{}", i)));
    s.change.insert(
        file.into(),
        ArtifactChange {
            raw: Some(raw),
            structural: Some(StructuralChange {
                change_type: "file.write".into(),
                extra: file_extra,
            }),
        },
    );

    s
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut rng = StdRng::seed_from_u64(args.seed);
    let n = args.steps;

    // Weighted mix: ~70% conversation turns (alternating user/assistant),
    // ~20% Edit/Write, ~10% MultiEdit. Tool turns attach as sibling children
    // of the previous assistant step — advancing HEAD stays on the
    // conversation spine, matching the derived Claude shape the Preview was
    // designed against.
    let mut steps: Vec<Step> = Vec::with_capacity(n);
    let mut head_id: Option<String> = None;
    let mut last_assistant_id: Option<String> = None;

    // Seed with a user turn.
    if n > 0 {
        let s = user_step(0, None, &mut rng);
        head_id = Some(s.step.id.clone());
        steps.push(s);
    }

    for i in 1..n {
        let r: f64 = rng.random();
        let parent = head_id.clone();
        if r < 0.30 {
            // Tool invocation — sibling child of last assistant, does not
            // advance HEAD.
            if let Some(la) = &last_assistant_id {
                let s = tool_step(i, la, &mut rng);
                steps.push(s);
                continue;
            }
            // No assistant yet — fall through to a regular conv turn.
        }
        // Alternate user/assistant on the spine.
        if i % 2 == 1 {
            let s = assistant_step(i, parent.as_deref(), &mut rng);
            head_id = Some(s.step.id.clone());
            last_assistant_id = Some(s.step.id.clone());
            steps.push(s);
        } else {
            let s = user_step(i, parent.as_deref(), &mut rng);
            head_id = Some(s.step.id.clone());
            steps.push(s);
        }
    }

    let head = head_id.unwrap_or_else(|| "step-000000".into());

    let mut actors: HashMap<String, ActorDefinition> = HashMap::new();
    actors.insert(
        "agent:claude-code".into(),
        ActorDefinition {
            name: Some("Claude Code".into()),
            provider: Some("anthropic".into()),
            model: Some("claude-opus-4-6".into()),
            ..Default::default()
        },
    );
    actors.insert(
        "human:bench".into(),
        ActorDefinition {
            name: Some("Bench User".into()),
            ..Default::default()
        },
    );

    let path = Path {
        path: PathIdentity {
            id: format!("synthetic-{}-steps", n),
            base: Some(Base {
                uri: "file:///synthetic".into(),
                ref_str: None,
            }),
            head,
        },
        steps,
        meta: Some(PathMeta {
            title: Some(format!("Synthetic {}-step path", n)),
            source: Some(format!("synthetic://seed={}", args.seed)),
            actors: Some(actors),
            extra: {
                let mut m = HashMap::new();
                m.insert("bench".into(), json!({"seed": args.seed, "steps": n}));
                m
            },
            ..Default::default()
        }),
    };

    let doc = Document::Path(path);
    let json = doc.to_json()?;
    if let Some(parent) = args.out.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(&args.out, &json).with_context(|| format!("writing {}", args.out.display()))?;
    eprintln!(
        "wrote {} ({} bytes, {} steps)",
        args.out.display(),
        json.len(),
        n
    );
    Ok(())
}
