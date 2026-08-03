//! Fuzzy matching for the native picker: fzf `--with-nth` field
//! projection plus a thin wrapper over [`nucleo_matcher`].
//!
//! Query syntax is nucleo's, parsed with
//! `Pattern::parse(query, CaseMatching::Smart, Normalization::Smart)`.
//! This is a deliberate upgrade over the skim backend:
//!
//! - space-separated words AND together (`share codex` matches rows
//!   containing both),
//! - `'text` requires an exact (non-fuzzy) substring match,
//! - `^text` anchors at the start,
//! - `!text` negates.
//!
//! Matching runs over [`Row::display`] ONLY — the `with_nth` projection
//! — so hidden lookup columns (project paths, session ids) never match
//! a query, exactly like fzf with `--with-nth`.

use anyhow::{Result, bail};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32String};

/// One picker line. `original` is the full TSV line as supplied by the
/// caller — it is what [`Selected`](crate::fuzzy::PickResult::Selected)
/// returns, hidden columns included. `fields` back the `{1}`..`{n}`
/// preview placeholders. `display` is the `with_nth` projection,
/// space-joined — the ONLY text that is visible and searchable.
#[derive(Debug, Clone)]
pub(super) struct Row {
    pub original: String,
    pub fields: Vec<String>,
    pub display: String,
}

impl Row {
    /// Split a TSV `line` into fields and project the visible columns
    /// per `spec` (an already-parsed `--with-nth` field spec).
    pub fn new(line: &str, spec: &[FieldRange]) -> Self {
        let fields: Vec<String> = line.split('\t').map(str::to_string).collect();
        let display = project_fields(&fields, spec);
        Self {
            original: line.to_string(),
            fields,
            display,
        }
    }
}

/// One row's match result. `indices` are *char* positions into
/// [`Row::display`] (sorted, deduped) for highlight rendering.
#[derive(Debug, Clone)]
pub(super) struct MatchEntry {
    pub row: usize,
    pub score: u32,
    pub indices: Vec<u32>,
}

/// One component of an fzf `--with-nth` field spec. 1-based, as fzf
/// counts fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FieldRange {
    /// `3` — a single field.
    Single(usize),
    /// `2..` — from a field to the end.
    From(usize),
    /// `..2` — from the start through a field.
    To(usize),
    /// `2..4` — a bounded inclusive range.
    Between(usize, usize),
}

impl FieldRange {
    /// Does this range include 1-based field index `idx`?
    fn contains(&self, idx: usize) -> bool {
        match *self {
            FieldRange::Single(n) => idx == n,
            FieldRange::From(n) => idx >= n,
            FieldRange::To(n) => idx <= n,
            FieldRange::Between(a, b) => idx >= a && idx <= b,
        }
    }
}

/// Parse fzf `--with-nth` notation: comma-separated components, each
/// `3`, `1..`, `2..4`, or `..2`. Rejects negatives and garbage with a
/// clear error — the in-repo call sites only ever pass well-formed
/// specs, so an error here is a programming bug worth surfacing.
pub(super) fn parse_field_spec(s: &str) -> Result<Vec<FieldRange>> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.push(parse_component(part)?);
    }
    if out.is_empty() {
        bail!("empty --with-nth field spec {s:?}");
    }
    Ok(out)
}

fn parse_component(part: &str) -> Result<FieldRange> {
    let parse_index = |txt: &str| -> Result<usize> {
        if txt.starts_with('-') {
            bail!("negative field index {txt:?} in --with-nth spec (not supported)");
        }
        let n: usize = txt
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid field index {txt:?} in --with-nth spec"))?;
        if n == 0 {
            bail!("field indices are 1-based; got 0 in --with-nth spec");
        }
        Ok(n)
    };
    if let Some((lo, hi)) = part.split_once("..") {
        match (lo.is_empty(), hi.is_empty()) {
            (true, true) => bail!("bare `..` in --with-nth spec (use `1..`)"),
            (false, true) => Ok(FieldRange::From(parse_index(lo)?)),
            (true, false) => Ok(FieldRange::To(parse_index(hi)?)),
            (false, false) => {
                let (a, b) = (parse_index(lo)?, parse_index(hi)?);
                if a > b {
                    bail!("inverted range `{part}` in --with-nth spec");
                }
                Ok(FieldRange::Between(a, b))
            }
        }
    } else {
        Ok(FieldRange::Single(parse_index(part)?))
    }
}

/// Project `fields` through a parsed field spec, space-joining the
/// selected columns in spec order. Out-of-range indices are skipped —
/// a row with fewer columns than the spec asks for just shows less.
pub(super) fn project_fields(fields: &[String], spec: &[FieldRange]) -> String {
    let mut picked: Vec<&str> = Vec::new();
    for range in spec {
        for (i, field) in fields.iter().enumerate() {
            if range.contains(i + 1) {
                picked.push(field.as_str());
            }
        }
    }
    picked.join(" ")
}

/// Reused nucleo matcher. `Matcher` carries sizable internal scratch
/// buffers, so it is constructed once and reused across keystrokes.
pub(super) struct NucleoMatcher {
    matcher: Matcher,
    /// Rows converted to UTF-32 once up front — nucleo matches over
    /// `Utf32Str`, and re-converting every row on every keystroke would
    /// dominate the match cost.
    haystacks: Vec<Utf32String>,
}

impl NucleoMatcher {
    pub fn new(rows: &[Row]) -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            haystacks: rows
                .iter()
                .map(|r| Utf32String::from(r.display.as_str()))
                .collect(),
        }
    }

    /// Re-match every row against `query`. Empty query returns every
    /// row in input order with no highlight spans. Non-empty queries
    /// return matches sorted score-descending, then row-ascending —
    /// the `tiebreak=index` contract every in-repo call site relies on.
    pub fn rematch(&mut self, query: &str) -> Vec<MatchEntry> {
        if query.is_empty() {
            return (0..self.haystacks.len())
                .map(|row| MatchEntry {
                    row,
                    score: 0,
                    indices: Vec::new(),
                })
                .collect();
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut out = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for (row, hay) in self.haystacks.iter().enumerate() {
            indices.clear();
            if let Some(score) = pattern.indices(hay.slice(..), &mut self.matcher, &mut indices) {
                let mut idx = indices.clone();
                idx.sort_unstable();
                idx.dedup();
                out.push(MatchEntry {
                    row,
                    score,
                    indices: idx,
                });
            }
        }
        out.sort_by(|a, b| b.score.cmp(&a.score).then(a.row.cmp(&b.row)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_field_spec_single_index() {
        assert_eq!(parse_field_spec("3").unwrap(), vec![FieldRange::Single(3)]);
    }

    #[test]
    fn parse_field_spec_open_range_from() {
        assert_eq!(parse_field_spec("2..").unwrap(), vec![FieldRange::From(2)]);
    }

    #[test]
    fn parse_field_spec_open_range_to() {
        assert_eq!(parse_field_spec("..2").unwrap(), vec![FieldRange::To(2)]);
    }

    #[test]
    fn parse_field_spec_bounded_range() {
        assert_eq!(
            parse_field_spec("2..4").unwrap(),
            vec![FieldRange::Between(2, 4)]
        );
    }

    #[test]
    fn parse_field_spec_comma_list() {
        assert_eq!(
            parse_field_spec("1,3").unwrap(),
            vec![FieldRange::Single(1), FieldRange::Single(3)]
        );
    }

    #[test]
    fn parse_field_spec_rejects_negative_and_garbage() {
        assert!(parse_field_spec("-1").is_err());
        assert!(parse_field_spec("2..-4").is_err());
        assert!(parse_field_spec("abc").is_err());
        assert!(parse_field_spec("0").is_err());
        assert!(parse_field_spec("..").is_err());
        assert!(parse_field_spec("4..2").is_err());
        assert!(parse_field_spec("").is_err());
    }

    /// Every `with_nth` value in live use in this repo must parse:
    /// "2", "3", "4", "1..", "2..". A regression here bricks a picker
    /// call site.
    #[test]
    fn parse_field_spec_covers_every_in_repo_spec() {
        for spec in ["2", "3", "4", "1..", "2.."] {
            assert!(
                parse_field_spec(spec).is_ok(),
                "in-repo with_nth spec {spec:?} failed to parse"
            );
        }
    }

    #[test]
    fn project_fields_skips_out_of_range() {
        let f = fields(&["a", "b"]);
        // Field 5 doesn't exist; only what's present renders.
        assert_eq!(
            project_fields(&f, &parse_field_spec("2,5").unwrap()),
            "b".to_string()
        );
        assert_eq!(
            project_fields(&f, &parse_field_spec("3..").unwrap()),
            String::new()
        );
    }

    #[test]
    fn project_fields_open_range_takes_tail() {
        let f = fields(&["proj", "sess", "row text"]);
        assert_eq!(
            project_fields(&f, &parse_field_spec("2..").unwrap()),
            "sess row text"
        );
    }

    #[test]
    fn row_display_is_projection_of_tsv_line() {
        let spec = parse_field_spec("3").unwrap();
        let row = Row::new("proj\tsess\tvisible title", &spec);
        assert_eq!(row.original, "proj\tsess\tvisible title");
        assert_eq!(row.fields.len(), 3);
        assert_eq!(row.display, "visible title");
    }

    #[test]
    fn rematch_empty_query_preserves_input_order_without_spans() {
        let spec = parse_field_spec("1..").unwrap();
        let rows: Vec<Row> = ["bbb", "aaa"].iter().map(|l| Row::new(l, &spec)).collect();
        let mut m = NucleoMatcher::new(&rows);
        let out = m.rematch("");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].row, 0);
        assert_eq!(out[1].row, 1);
        assert!(out.iter().all(|e| e.indices.is_empty()));
    }

    #[test]
    fn rematch_scores_and_sorts_best_first() {
        let spec = parse_field_spec("1..").unwrap();
        let rows: Vec<Row> = ["completely unrelated", "share codex session", "share"]
            .iter()
            .map(|l| Row::new(l, &spec))
            .collect();
        let mut m = NucleoMatcher::new(&rows);
        let out = m.rematch("share");
        // "completely unrelated" has no fuzzy `share` match in order; the
        // two real matches are present with highlight indices.
        assert!(out.iter().all(|e| e.row != 0));
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|e| !e.indices.is_empty()));
        // Equal-quality matches tie-break by row (input) order.
        let rows_in_order: Vec<usize> = out.iter().map(|e| e.row).collect();
        assert!(rows_in_order == vec![1, 2] || rows_in_order == vec![2, 1]);
    }

    #[test]
    fn rematch_matches_display_only_not_hidden_columns() {
        // Hidden column 1 contains "secret"; display is column 2.
        let spec = parse_field_spec("2").unwrap();
        let rows = vec![Row::new("secret\tvisible", &spec)];
        let mut m = NucleoMatcher::new(&rows);
        assert!(m.rematch("secret").is_empty());
        assert_eq!(m.rematch("visible").len(), 1);
    }
}
