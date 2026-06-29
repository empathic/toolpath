//! In-process jaq (pure-Rust jq) execution for `path query`.
//!
//! The scoped step array is handed to jaq as a single input value; the filter
//! does all matching, projection, ranking, and aggregation. Output mirrors jq:
//! each value the filter yields is printed on its own line, pretty-printed by
//! default and compact under `--compact` (or when stdout is not a TTY). With
//! `--raw`, string results print unquoted (like `jq -r`).

use anyhow::{Result, anyhow};
use std::io::Write;

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, Vars, data, unwrap_valr};
use jaq_json::Val;

/// Compile `code` and run it over `input`, printing each output value.
///
/// `raw` mirrors `jq -r`: string results print without JSON quoting or
/// escaping; every other value still prints as JSON.
pub fn run(input: &serde_json::Value, code: &str, compact: bool, raw: bool) -> Result<()> {
    // serde_json::Value → jaq Val via a JSON round-trip. The array is one we
    // just built, so parsing it back can't realistically fail.
    let bytes = serde_json::to_vec(input)?;
    let val = jaq_json::read::parse_single(&bytes)
        .map_err(|e| anyhow!("internal: could not load step array into jaq: {e}"))?;

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

    let filter = Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|errs| format_compile_errors(code, errs))?;

    // jq parity: compact is `{"a":1}`; pretty is 2-space indented with a
    // space after each colon.
    let pp = jaq_json::write::Pp {
        indent: (!compact).then(|| "  ".to_string()),
        sep_space: !compact,
        ..Default::default()
    };

    let ctx = Ctx::<data::JustLut<Val>>::new(&filter.lut, Vars::new([]));
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for res in filter.id.run((ctx, val)).map(unwrap_valr) {
        let value = res.map_err(|e| anyhow!("query filter error: {e}"))?;
        // `--raw`: print string values as their bytes, no quotes/escaping.
        // Both jaq string variants (UTF-8 `TStr`, byte `BStr`) hold `Bytes`,
        // which derefs to `&[u8]`. Non-strings fall through to JSON.
        if raw && let Val::TStr(b) | Val::BStr(b) = &value {
            let bytes: &[u8] = b;
            out.write_all(bytes)?;
        } else {
            jaq_json::write::write(&mut out, &pp, 0, &value)?;
        }
        out.write_all(b"\n")?;
    }
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

    fn array() -> serde_json::Value {
        json!([
            {"cache_id": "a", "step": {"id": "s1", "actor": "human:alex"}, "tokens": 10},
            {"cache_id": "b", "step": {"id": "s2", "actor": "agent:claude"}, "tokens": 90},
        ])
    }

    /// Capture-free smoke test: a valid filter compiles and runs without error.
    #[test]
    fn identity_filter_runs() {
        run(&array(), ".", true, false).unwrap();
    }

    #[test]
    fn select_and_aggregate_run() {
        run(&array(), "map(select(.tokens > 50))", true, false).unwrap();
        run(&array(), "[.[].tokens] | add", true, false).unwrap();
        run(&array(), "sort_by(-.tokens) | .[0].cache_id", false, false).unwrap();
    }

    #[test]
    fn regex_test_is_available() {
        // `test` requires the regex feature; this would error if it weren't on.
        run(
            &array(),
            r#"map(select(.step.actor | test("claude")))"#,
            true,
            false,
        )
        .unwrap();
    }

    #[test]
    fn raw_mode_runs_for_strings_and_nonstrings() {
        // String, non-string, and a stream that mixes both — none should error.
        run(&array(), ".[].cache_id", true, true).unwrap();
        run(&array(), ".[].tokens", true, true).unwrap();
        run(&array(), ".[0]", true, true).unwrap();
    }

    #[test]
    fn syntax_error_is_reported() {
        let err = run(&array(), "map(select(", true, false).unwrap_err();
        assert!(err.to_string().contains("jq filter"), "{err}");
    }

    #[test]
    fn unknown_function_is_reported() {
        let err = run(&array(), "no_such_function", true, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("compile") || msg.contains("undefined"),
            "{msg}"
        );
    }
}
