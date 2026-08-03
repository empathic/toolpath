//! Embedded fuzzy picker — used when external `fzf` isn't on `PATH`.
//!
//! Mirrors the `fzf::PickOptions` surface so callers don't have to know
//! which backend is in play. Skim is a Rust fzf-clone that supports the
//! same `--with-nth`/`--preview`/`--multi`/`--tiebreak` knobs we care
//! about, and its `{1}`, `{2}` preview-placeholder syntax matches fzf's,
//! so the existing preview commands (`path show ...`) work unchanged.
#![cfg(not(target_os = "emscripten"))]
// The native picker (crate::tui) replaced this backend; the module is
// deleted in the next commit — the allow keeps this one green.
#![allow(dead_code)]

use anyhow::{Context, Result};
use std::io::Cursor;

use crate::fuzzy::{PickOptions, PickResult};

use regex::Regex;
use skim::Skim;
use skim::prelude::{RankCriteria, SkimItemReader, SkimItemReaderOption, SkimOptionsBuilder};

/// Run the embedded fuzzy picker. Same semantics as `fzf::pick`: returns
/// `Selected` for accepted picks, `Cancelled` for Esc/Ctrl-C, `NoMatch`
/// for "input was non-empty but nothing matched the query".
pub fn pick(lines: &[String], opts: &PickOptions<'_>) -> Result<PickResult> {
    let tiebreak = parse_tiebreak(opts.tiebreak)?;
    let with_nth = parse_field_spec(opts.with_nth);

    let mut builder = SkimOptionsBuilder::default();
    builder
        .prompt(opts.prompt.to_string())
        .multi(opts.multi)
        // Match the fzf side, which always passes `--delimiter=\t`. The
        // skim default (`[\t\n ]+`) would split on spaces inside display
        // columns, which we don't want.
        .delimiter(Regex::new(r"\t").expect("static tab regex"))
        .with_nth(with_nth)
        .tiebreak(vec![tiebreak])
        // fzf shows the prompt at the top; match that so the layout
        // matches what users see with external fzf.
        .reverse(true)
        // Take over the whole terminal so the picker has room.
        .height("100%".to_string());

    if let Some(preview) = opts.preview {
        // `setter(strip_option, into)` on the builder unwraps `Option<T>`
        // and converts via `Into`, so we just pass the bare value.
        builder.preview(crate::fuzzy::substitute_exe_placeholder(preview));
        builder.preview_window(opts.preview_window);
    }
    if let Some(header) = opts.header {
        builder.header(header.to_string());
    }

    let options = builder.build().context("build skim options")?;

    // Feed lines through the standard item reader so skim handles
    // matching/scoring just like its CLI does on stdin. The reader is
    // built *from* the configured options — `SkimItemReaderOption::default()`
    // would silently ignore `delimiter` and `with_nth` (those settings
    // live on the reader, not on `SkimOptions`), which is why an
    // earlier version of this code displayed every column including
    // the hidden lookup keys.
    let input = lines.join("\n");
    let reader = SkimItemReader::new(SkimItemReaderOption::from_options(&options));
    let items = reader.of_bufread(Cursor::new(input));

    // Skim returns eyre::Result rather than anyhow, so the chain isn't
    // 1:1; render the message and re-wrap.
    let output = Skim::run_with(options, Some(items))
        .map_err(|e| anyhow::anyhow!("skim picker failed: {e}"))?;

    if output.is_abort {
        return Ok(PickResult::Cancelled);
    }
    if output.selected_items.is_empty() {
        // Accepted without anything selected → treat as NoMatch so
        // callers that distinguish "user cancelled" from "query had no
        // matches" see the same shape they got from fzf.
        return Ok(PickResult::NoMatch);
    }
    let picked: Vec<String> = output
        .selected_items
        .iter()
        .map(|item| item.output().to_string())
        .collect();
    Ok(PickResult::Selected(picked))
}

/// Parse fzf's `tiebreak` flag value into skim's enum. We only use
/// `index` (preserve input order), but tolerate the other fzf names so
/// future call-sites don't surprise us.
fn parse_tiebreak(s: &str) -> Result<RankCriteria> {
    match s {
        "index" => Ok(RankCriteria::Index),
        "score" => Ok(RankCriteria::Score),
        "begin" => Ok(RankCriteria::Begin),
        "end" => Ok(RankCriteria::End),
        "length" => Ok(RankCriteria::Length),
        other => anyhow::bail!("unsupported tiebreak `{other}` for embedded picker"),
    }
}

/// Convert fzf's `--with-nth` spec (e.g. `"2.."` or `"1,3"`) into the
/// `Vec<String>` skim expects. fzf and skim share the same grammar, so
/// the conversion is just a comma-split with no remapping.
fn parse_field_spec(s: &str) -> Vec<String> {
    s.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tiebreak_known_values_map_through() {
        assert!(matches!(
            parse_tiebreak("index").unwrap(),
            RankCriteria::Index
        ));
        assert!(matches!(
            parse_tiebreak("score").unwrap(),
            RankCriteria::Score
        ));
    }

    #[test]
    fn parse_tiebreak_rejects_unknown_value() {
        assert!(parse_tiebreak("nope").is_err());
    }

    #[test]
    fn parse_field_spec_splits_on_comma_and_trims() {
        assert_eq!(parse_field_spec("2.."), vec!["2..".to_string()]);
        assert_eq!(
            parse_field_spec("1,3, 5"),
            vec!["1".to_string(), "3".to_string(), "5".to_string()]
        );
    }

    #[test]
    fn parse_field_spec_drops_empty_entries() {
        assert_eq!(parse_field_spec(",,2"), vec!["2".to_string()]);
        assert_eq!(parse_field_spec(""), Vec::<String>::new());
    }
}
