//! In-process jaq (pure-Rust jq) execution for `path query`.
//!
//! A compiled jaq filter is fully owned (it borrows neither its source nor the
//! parse arena once built), so we compile once and run it across many inputs.
//! [`execute`] drives one of three strategies chosen by [`super::plan`]:
//!
//! - **PerFileStream** — run the filter on each file's step array and print
//!   outputs as they come. Nothing accumulates.
//! - **Decompose** — run the filter per file, gather the per-file outputs into
//!   one array, then run a `reduce` filter over it (top-N, sum, count, …).
//! - **Slurp** — accumulate every file's steps into one array and run the
//!   filter once. The always-correct fallback; still lean, since we hold the
//!   values once (no whole-cache byte buffer).
//!
//! Output mirrors jq: each yielded value on its own line, pretty by default,
//! compact under `--compact` (or when piped), raw strings under `--raw`.

use anyhow::{Result, anyhow};
use std::io::Write;

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, Vars, data, unwrap_valr};
use jaq_json::Val;

use super::plan::Plan;

/// A compiled jaq program over JSON values. Owned — safe to hold and reuse.
type Program = jaq_core::Filter<data::JustLut<Val>>;

/// Run `main_src` over the cache per `plan`, streaming files via `run_files`
/// (which invokes the supplied callback once per file with that file's wrapped
/// step array as a jaq value).
pub fn execute(
    plan: &Plan,
    main_src: &str,
    compact: bool,
    raw: bool,
    out: &mut dyn Write,
    run_files: impl FnOnce(&mut dyn FnMut(Val) -> Result<()>) -> Result<()>,
) -> Result<()> {
    let main = compile(main_src)?;
    let pp = pretty(compact);

    match plan {
        Plan::PerFileStream => {
            let mut emit = |val: Val| eval_print(&main, val, out, &pp, raw);
            run_files(&mut emit)?;
        }
        Plan::Slurp => {
            let mut all: Vec<Val> = Vec::new();
            let mut emit = |val: Val| {
                if let Val::Arr(items) = val {
                    all.extend(items.iter().cloned());
                }
                Ok(())
            };
            run_files(&mut emit)?;
            let merged: Val = all.into_iter().collect();
            eval_print(&main, merged, out, &pp, raw)?;
        }
        Plan::Decompose { reduce } => {
            let reducer = compile(reduce)?;
            let mut partials: Vec<Val> = Vec::new();
            let mut saw_file = false;
            let mut emit = |val: Val| {
                saw_file = true;
                partials.extend(eval_collect(&main, val)?);
                Ok(())
            };
            run_files(&mut emit)?;
            if saw_file {
                let merged: Val = partials.into_iter().collect();
                eval_print(&reducer, merged, out, &pp, raw)?;
            } else {
                // No document contributed a partial: the decomposition
                // identity `reduce(⋃ main(fᵢ)) == main(⋃ fᵢ)` degenerates to
                // `main([])`. Run the *main* filter over an empty array so the
                // answer matches slurp (`length` → 0, `sort_by|.[:N]` → []),
                // not the reducer over `[]` (which would give null / error).
                let empty: Val = std::iter::empty().collect();
                eval_print(&main, empty, out, &pp, raw)?;
            }
        }
    }
    Ok(())
}

/// Convert one file's wrapped steps into a jaq array value. The byte buffer is
/// per-file (bounded), so no whole-cache serialization is ever held.
pub fn steps_to_val(steps: Vec<serde_json::Value>) -> Result<Val> {
    let bytes = serde_json::to_vec(&serde_json::Value::Array(steps))?;
    jaq_json::read::parse_single(&bytes)
        .map_err(|e| anyhow!("internal: could not load steps into jaq: {e}"))
}

fn compile(code: &str) -> Result<Program> {
    let program = File { code, path: () };
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs());
    let funs = jaq_core::funs()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs());

    let loader = Loader::new(defs);
    let arena = Arena::default();
    let modules = loader
        .load(&arena, program)
        .map_err(|errs| format_load_errors(code, errs))?;
    Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|errs| format_compile_errors(code, errs))
}

fn eval_collect(prog: &Program, input: Val) -> Result<Vec<Val>> {
    let ctx = Ctx::<data::JustLut<Val>>::new(&prog.lut, Vars::new([]));
    prog.id
        .run((ctx, input))
        .map(unwrap_valr)
        .map(|r| r.map_err(|e| anyhow!("query filter error: {e}")))
        .collect()
}

fn eval_print(prog: &Program, input: Val, out: &mut dyn Write, pp: &Pp, raw: bool) -> Result<()> {
    let ctx = Ctx::<data::JustLut<Val>>::new(&prog.lut, Vars::new([]));
    for res in prog.id.run((ctx, input)).map(unwrap_valr) {
        let value = res.map_err(|e| anyhow!("query filter error: {e}"))?;
        print_val(out, pp, raw, &value)?;
    }
    Ok(())
}

use jaq_json::write::Pp;

fn pretty(compact: bool) -> Pp {
    // jq parity: compact is `{"a":1}`; pretty is 2-space indented with a
    // space after each colon.
    Pp {
        indent: (!compact).then(|| "  ".to_string()),
        sep_space: !compact,
        ..Default::default()
    }
}

fn print_val(out: &mut dyn Write, pp: &Pp, raw: bool, value: &Val) -> Result<()> {
    // `--raw`: print string values as their bytes, no quotes/escaping. Both
    // jaq string variants (UTF-8 `TStr`, byte `BStr`) hold `Bytes`, which
    // derefs to `&[u8]`. Non-strings fall through to JSON.
    if raw && let Val::TStr(b) | Val::BStr(b) = value {
        let bytes: &[u8] = b;
        out.write_all(bytes)?;
    } else {
        jaq_json::write::write(out, pp, 0, value)?;
    }
    out.write_all(b"\n")?;
    Ok(())
}

/// First non-empty line of `s`, truncated, for pointing at a syntax error.
fn snippet(s: &str) -> String {
    let line = s.trim().lines().next().unwrap_or("").trim();
    if line.chars().count() > 30 {
        format!("{}…", line.chars().take(30).collect::<String>())
    } else {
        line.to_string()
    }
}

fn format_load_errors(code: &str, errs: jaq_core::load::Errors<&str, ()>) -> anyhow::Error {
    let mut msgs = Vec::new();
    for (_file, err) in errs {
        match err {
            jaq_core::load::Error::Io(es) => {
                msgs.extend(es.into_iter().map(|(_, e)| format!("io error: {e}")));
            }
            jaq_core::load::Error::Lex(es) => {
                msgs.extend(es.into_iter().map(|(expect, at)| {
                    format!("expected {} at `{}`", expect.as_str(), snippet(at))
                }));
            }
            jaq_core::load::Error::Parse(es) => {
                msgs.extend(es.into_iter().map(|(expect, at)| {
                    format!("expected {} at `{}`", expect.as_str(), snippet(at))
                }));
            }
        }
    }
    if msgs.is_empty() {
        msgs.push("syntax error".to_string());
    }
    anyhow!("invalid jq filter `{code}`:\n  {}", msgs.join("\n  "))
}

fn format_compile_errors(code: &str, errs: jaq_core::compile::Errors<&str, ()>) -> anyhow::Error {
    let mut msgs = Vec::new();
    for (_file, es) in errs {
        for (name, undefined) in es {
            msgs.push(format!("undefined {}: {name}", undefined.as_str()));
        }
    }
    if msgs.is_empty() {
        msgs.push("compile error".to_string());
    }
    anyhow!(
        "could not compile jq filter `{code}`:\n  {}",
        msgs.join("\n  ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Run `code` over `files` (as separate cache documents) into a captured
    /// buffer, using an explicit plan.
    fn run_with(plan: &Plan, code: &str, files: &[serde_json::Value]) -> String {
        let mut out: Vec<u8> = Vec::new();
        execute(plan, code, true, false, &mut out, |emit| {
            for f in files {
                let arr = f.as_array().cloned().unwrap_or_default();
                emit(steps_to_val(arr)?)?;
            }
            Ok(())
        })
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    /// The heart of the validation: a filter the planner claims to *stream*
    /// must (a) actually be planned as non-slurp, and (b) produce byte-for-byte
    /// the same output as the always-correct whole-array slurp. The plan
    /// assertion is what stops this from silently vacating into slurp-vs-slurp.
    fn assert_streams(code: &str, files: &[serde_json::Value]) {
        let plan = crate::query::plan::analyze(code);
        assert_ne!(
            plan,
            Plan::Slurp,
            "`{code}` should be recognized as streamable, not slurp"
        );
        let planned = run_with(&plan, code, files);
        let slurped = run_with(&Plan::Slurp, code, files);
        assert_eq!(
            planned, slurped,
            "streamed output must equal slurp for `{code}`"
        );
    }

    /// A filter that must fall back to slurp (holistic, or deliberately not
    /// decomposed). Asserts the planner is conservative here, so a regression
    /// that wrongly decomposes it gets caught.
    fn assert_slurps(code: &str) {
        assert_eq!(
            crate::query::plan::analyze(code),
            Plan::Slurp,
            "`{code}` must slurp (not decompose)"
        );
    }

    fn fixture() -> Vec<serde_json::Value> {
        vec![
            json!([
                {"cache_id": "a", "step": {"id": "a1", "actor": "agent:x"}, "tokens": 10, "dead_end": false},
                {"cache_id": "a", "step": {"id": "a2", "actor": "human:y"}, "tokens": 90, "dead_end": true}
            ]),
            json!([
                {"cache_id": "b", "step": {"id": "b1", "actor": "agent:x"}, "tokens": 50, "dead_end": false}
            ]),
            json!([]),
            json!([
                {"cache_id": "c", "step": {"id": "c1", "actor": "agent:z"}, "tokens": 75, "dead_end": true},
                {"cache_id": "c", "step": {"id": "c2", "actor": "agent:z"}, "tokens": 30, "dead_end": false}
            ]),
        ]
    }

    #[test]
    fn stream_equals_slurp_for_elementwise() {
        let f = fixture();
        assert_streams(".[] | select(.dead_end)", &f);
        assert_streams(
            r#".[] | select(.step.actor | startswith("agent:")) | .step.id"#,
            &f,
        );
        assert_streams("map(select(.tokens > 40))", &f);
        assert_streams("[.[] | select(.dead_end) | .step.id]", &f);
    }

    #[test]
    fn stream_equals_slurp_for_top_n() {
        let f = fixture();
        assert_streams("sort_by(-.tokens) | .[:3]", &f);
        assert_streams("sort_by(.tokens) | .[:2]", &f);
        assert_streams("map({id: .step.id, t: .tokens}) | sort_by(-.t) | .[:2]", &f);
    }

    #[test]
    fn stream_equals_slurp_for_scalar_reductions() {
        let f = fixture();
        // Forms that genuinely decompose (bare `length`, `map(_) | add/length`).
        assert_streams("length", &f);
        assert_streams("map(.tokens) | add", &f);
        assert_streams("map(select(.dead_end)) | length", &f);
    }

    #[test]
    fn slurp_fallback_still_correct() {
        let f = fixture();
        // group_by slurps (holistic) — but must still produce the right answer.
        let planned = run_with(
            &crate::query::plan::analyze(
                "group_by(.step.actor) | map({actor: .[0].step.actor, n: length})",
            ),
            "group_by(.step.actor) | map({actor: .[0].step.actor, n: length})",
            &f,
        );
        let slurped = run_with(
            &Plan::Slurp,
            "group_by(.step.actor) | map({actor: .[0].step.actor, n: length})",
            &f,
        );
        assert_eq!(planned, slurped);
        assert_slurps("group_by(.step.actor) | map({n: length})");
        assert_slurps("unique_by(.step.actor) | length");
    }

    #[test]
    fn top_n_actually_bounds_the_merge() {
        // Sanity that the decompose path yields the true global top-2 across
        // files (90 from file a, 75 from file c), not a per-file artifact.
        let out = run_with(
            &Plan::Decompose {
                reduce: "add | sort_by(-.tokens) | .[:2]".to_string(),
            },
            "sort_by(-.tokens) | .[:2]",
            &fixture(),
        );
        assert!(out.contains("\"a2\""), "top row a2 (90) present: {out}");
        assert!(out.contains("\"c1\""), "second row c1 (75) present: {out}");
        assert!(!out.contains("\"a1\""), "a1 (10) must be excluded: {out}");
    }

    // ── Regressions for the review findings ──────────────────────────

    #[test]
    fn negative_and_dynamic_slice_bounds_slurp() {
        // Finding 1: only a literal nonnegative cutoff decomposes. Per-file
        // truncation of `.[:-1]` / `.[:length-1]` would drop the wrong rows.
        assert_slurps("sort_by(.tokens) | .[:-1]");
        assert_slurps("sort_by(.tokens) | .[:(length - 1)]");
        assert_streams("sort_by(.tokens) | .[:2]", &fixture());
    }

    #[test]
    fn min_max_slurp_to_avoid_empty_partition_null() {
        // Finding 2: `[] | min == null` poisons a per-file merge, so min/max
        // are not decomposed. The fixture includes an empty document.
        assert_slurps("map(.tokens) | min");
        assert_slurps("map(.tokens) | max");
    }

    #[test]
    fn zero_files_decompose_matches_main_over_empty() {
        // Finding 3: with no document contributing a partial, a Decompose plan
        // must equal `main([])`, i.e. slurp — not `reduce([])`.
        let none: &[serde_json::Value] = &[];
        for code in ["length", "map(.step)", "sort_by(.tokens) | .[:2]"] {
            let plan = crate::query::plan::analyze(code);
            assert_ne!(plan, Plan::Slurp, "`{code}` should decompose");
            assert_eq!(
                run_with(&plan, code, none),
                run_with(&Plan::Slurp, code, none),
                "zero-file `{code}` must match slurp"
            );
        }
    }

    #[test]
    fn parenthesized_tail_slurps() {
        // Finding 4: a source-span-recovered combine that doesn't reparse
        // (unbalanced by the paren) must fall back to slurp, not emit a broken
        // filter.
        assert_slurps("map({id: .step.id}) | (sort_by(.id)) | .[:1]");
    }
}
