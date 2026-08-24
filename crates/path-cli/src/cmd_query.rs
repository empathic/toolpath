//! `path query` — load every cached step into one JSON array and transform it
//! with a jaq (jq) filter.
//!
//! This is a thin clap layer over [`crate::query`]; the scope flags choose
//! *which* documents load and the filter does the rest. See the design at
//! `docs/superpowers/specs/2026-06-22-path-query-command-design.md` and
//! `path kind` for the per-step field reference.

use anyhow::Result;
use clap::Parser;
use std::io::IsTerminal;
use std::path::PathBuf;

use crate::query::Scope;

/// Each array element is a Toolpath step (`step`/`change`/`meta` verbatim)
/// wrapped with three keys: `cache_id`, `path` (the parent path's `id`/`base`/
/// `meta`), and `dead_end` (whether the step sits off the head's ancestry).
/// Reach a step's fields with the usual jq paths, e.g.
/// `.change[].structural.token_usage`. Run `path kind` for the field reference.
#[derive(Parser, Debug)]
#[command(after_long_help = WRAPPER_HELP)]
pub struct QueryArgs {
    /// jaq (jq) filter to run over the scoped step array
    /// (use `.` to emit the array unchanged).
    filter: String,

    /// Select cached files by source prefix
    /// (claude/gemini/codex/opencode/cursor/pi/git/github).
    #[arg(long)]
    source: Option<String>,

    /// Load a specific cached document by id (repeatable).
    #[arg(long = "id")]
    ids: Vec<String>,

    /// Load an off-cache document (`-` for stdin; repeatable).
    #[arg(long)]
    input: Vec<String>,

    /// Keep only paths whose base resolves to this project directory.
    #[arg(long)]
    project: Option<PathBuf>,

    /// Keep only sessions whose project directory (the path base)
    /// lies under this directory (subtree match; the session's
    /// recorded working directory, not the files it touched), and
    /// scope the implicit sync to it likewise.
    #[arg(long)]
    project_under: Option<PathBuf>,

    /// Keep only paths whose meta.kind matches this selector
    /// (semver prefix, e.g. `agent-coding-session` or `…/v1.0`).
    #[arg(long)]
    kind: Option<String>,

    /// Force compact (single-line) JSON output.
    #[arg(short = 'c', long)]
    compact: bool,

    /// Output raw strings without JSON quotes/escaping (like `jq -r`).
    /// Applies only to string results; non-strings still print as JSON.
    /// Handy for piping a column of ids/paths into another command, or
    /// reading text/diff content. Composes with `-c`.
    #[arg(short = 'r', long)]
    raw: bool,

    /// Skip the automatic cache sync that runs before the query.
    #[arg(long)]
    no_sync: bool,
}

const WRAPPER_HELP: &str = "\
Each array element wraps a Toolpath step:

  {
    \"cache_id\": \"claude-abc123\",
    \"path\": { \"id\": …, \"base\": …, \"meta\": { \"kind\": …, \"source\": … } },
    \"step\":   { \"id\": …, \"parents\": [...], \"actor\": …, \"timestamp\": … },
    \"change\": { \"<artifact>\": { \"raw\": …, \"structural\": … } },
    \"meta\":   { … },
    \"dead_end\": false
  }

The fields under `change[].structural` are defined by the path's kind;
run `path kind <kind>` for the schema, or `path query --kind … '.[0]'` to
read a sample. Identity is the triple (cache_id, path.id, step.id).

A real cache mixes provider and tool versions, so a field's shape can vary
(e.g. `tool_uses[]` is sometimes an object, sometimes a bare name string).
Guard element access with `?` (`.name?`) and `// empty` so one odd value
doesn't abort the run; sample with `'.[0]'` when unsure.

Examples:
  path query 'map(select(any(.. | strings; test(\"RefCell\"))))'
  path query 'map(select(any(.change | keys[]; endswith(\"cmd_resume.rs\"))))'
  path query --source claude 'map(select(any(.change[].structural.tool_uses[]?; .name? == \"Bash\" and .result.is_error? == true)))'
  path query 'group_by(.path.meta.source) | map({source: .[0].path.meta.source, steps: length})'
  path query -r '.[].cache_id' | sort -u            # raw ids, pipeable to xargs/grep
  path query -r '.[0].change[].structural.text'     # read a turn's text, unescaped";

pub fn run(args: QueryArgs, pretty: bool) -> Result<()> {
    #[cfg(not(target_os = "emscripten"))]
    if !args.no_sync {
        sync_query_scope(&args);
    }

    let scope = Scope {
        source: args.source,
        ids: args.ids,
        inputs: args.input,
        project: args.project,
        project_under: args.project_under,
        kind: args.kind,
    };

    // Output policy mirrors jq: compact when forced with `-c` or when piped;
    // pretty on a TTY or when the global `--pretty` flag is set.
    let compact = args.compact || (!pretty && !std::io::stdout().is_terminal());

    crate::query::run(&scope, &args.filter, compact, args.raw)
}

/// Freshen the slice of the cache this query will read, before reading
/// it. Quiet unless something was actually ingested; a sync failure
/// degrades to querying the cache as-is.
#[cfg(not(target_os = "emscripten"))]
fn sync_query_scope(args: &QueryArgs) {
    let types = sync_types_for(args.source.as_deref(), &args.ids, &args.input);
    if types.is_empty() {
        return;
    }
    // Transitional: `query` does not take `&Config` yet; load one for
    // the config directory. A load failure degrades like a sync failure.
    let config_dir = match crate::config::Config::load().and_then(|c| c.config_dir()) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("warning: cache sync skipped: {e}");
            return;
        }
    };
    let bundle = crate::harness::HarnessBundle::from_environment();
    match crate::sync::sync_bundle(
        &config_dir,
        &bundle,
        &types,
        args.project_under.as_deref(),
        &mut (),
    ) {
        Ok(outcomes) => {
            for (t, o) in outcomes {
                if o.new + o.updated + o.failed > 0 {
                    let failed = if o.failed > 0 {
                        format!(", {} failed", o.failed)
                    } else {
                        String::new()
                    };
                    eprintln!(
                        "synced {}: {} new, {} updated{failed}",
                        t.name(),
                        o.new,
                        o.updated
                    );
                }
            }
        }
        Err(e) => eprintln!("warning: cache sync skipped: {e}"),
    }
}

/// Which artifact types the query's scope flags reach. `--input`-only
/// invocations never touch the cache; `--source` narrows to one type
/// (non-syncable sources like `pathbase` map to nothing); `--id`s
/// narrow to their prefixes; a bare cache-wide query syncs everything.
#[cfg(not(target_os = "emscripten"))]
fn sync_types_for(
    source: Option<&str>,
    ids: &[String],
    inputs: &[String],
) -> Vec<crate::artifact::ArtifactType> {
    use crate::artifact::ArtifactType;
    let scans_cache = source.is_some() || !ids.is_empty() || inputs.is_empty();
    if !scans_cache {
        return Vec::new();
    }
    if let Some(source) = source {
        return ArtifactType::parse(source).into_iter().collect();
    }
    if !ids.is_empty() {
        return ArtifactType::ALL
            .into_iter()
            .filter(|t| {
                ids.iter().any(|id| {
                    id.strip_prefix(t.name())
                        .is_some_and(|rest| rest.starts_with('-'))
                })
            })
            .collect();
    }
    ArtifactType::ALL.to_vec()
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::sync_types_for;
    use crate::artifact::ArtifactType;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn input_only_queries_sync_nothing() {
        assert!(sync_types_for(None, &[], &s(&["doc.json"])).is_empty());
    }

    #[test]
    fn source_flag_narrows_to_one_type() {
        assert_eq!(
            sync_types_for(Some("claude"), &[], &[]),
            vec![ArtifactType::Claude]
        );
        assert!(
            sync_types_for(Some("pathbase"), &[], &[]).is_empty(),
            "non-syncable sources sync nothing"
        );
    }

    #[test]
    fn ids_narrow_to_their_prefixes() {
        let types = sync_types_for(
            None,
            &s(&["claude-abc", "codex-def", "pathbase-x-y-z"]),
            &[],
        );
        assert_eq!(types, vec![ArtifactType::Claude, ArtifactType::Codex]);
        // `cursor-…` must not match on the `c` of another type or vice versa.
        assert_eq!(
            sync_types_for(None, &s(&["cursor-abc"]), &[]),
            vec![ArtifactType::Cursor]
        );
    }

    #[test]
    fn bare_cache_query_syncs_everything() {
        assert_eq!(sync_types_for(None, &[], &[]), ArtifactType::ALL.to_vec());
    }

    #[test]
    fn source_beats_ids_and_inputs_do_not_disable_cache_scan() {
        assert_eq!(
            sync_types_for(Some("git"), &s(&["claude-abc"]), &s(&["doc.json"])),
            vec![ArtifactType::Git]
        );
    }
}
