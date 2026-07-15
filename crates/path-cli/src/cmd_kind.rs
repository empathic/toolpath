//! `path kind` — the cold-start companion to `path query`.
//!
//! A step's shape is set by its path's *kind*. `path kind` lists the kinds the
//! binary bundles a spec for; `path kind <kind>` prints that kind's bundled
//! `schema.json`, which names every field, its type, and (in `description`
//! fields) the semantics behind it. A trailing `/<version>` pins one version,
//! matching the same semver-prefix rule as `path query --kind`.

use anyhow::{Result, bail};
use clap::Parser;

use crate::kinds::{self, BUNDLED_KINDS};

#[derive(Parser, Debug)]
#[command(after_long_help = KIND_HELP)]
pub struct KindArgs {
    /// Kind to show, e.g. `agent-coding-session` (newest version) or
    /// `agent-coding-session/v1.0.0` (pinned). Omit to list bundled kinds.
    kind: Option<String>,
}

const KIND_HELP: &str = "\
Examples:
  path kind                                 list bundled kinds
  path kind agent-coding-session            print the newest bundled schema
  path kind agent-coding-session/v1.0.0     pin a specific version";

pub fn run(args: KindArgs) -> Result<()> {
    match args.kind {
        None => {
            list();
            Ok(())
        }
        Some(selector) => print_schema(&selector),
    }
}

/// List bundled kinds, one name per line with its available versions.
fn list() {
    let mut names: Vec<&str> = Vec::new();
    for k in BUNDLED_KINDS {
        if !names.contains(&k.name) {
            names.push(k.name);
        }
    }
    for name in names {
        let versions: Vec<&str> = BUNDLED_KINDS
            .iter()
            .filter(|k| k.name == name)
            .map(|k| k.version)
            .collect();
        println!("{name}\t{}", versions.join(", "));
    }
}

fn print_schema(selector: &str) -> Result<()> {
    match kinds::resolve(selector) {
        Some(k) => {
            print!("{}", k.schema);
            if !k.schema.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        None => {
            let available: Vec<String> = BUNDLED_KINDS
                .iter()
                .map(|k| format!("{}/{}", k.name, k.version))
                .collect();
            bail!(
                "no bundled spec for kind `{selector}`. Bundled kinds: {}",
                available.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_version_resolves_and_is_json() {
        let k = kinds::resolve("agent-coding-session").unwrap();
        assert_eq!(k.version, "v1.2.0");
        let _: serde_json::Value =
            serde_json::from_str(k.schema).expect("bundled schema is valid JSON");
    }

    #[test]
    fn pinned_version_resolves() {
        let k = kinds::resolve("agent-coding-session/v1.0.0").unwrap();
        assert_eq!(k.version, "v1.0.0");
    }

    #[test]
    fn print_schema_errors_for_unknown() {
        let err = print_schema("nope").unwrap_err();
        assert!(err.to_string().contains("Bundled kinds"));
    }
}
