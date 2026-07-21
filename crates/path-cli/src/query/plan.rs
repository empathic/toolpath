//! Streaming execution planner for `path query`.
//!
//! The default contract is "the filter sees the whole array," which for a big
//! cache means holding every step in memory. But we can often avoid that by
//! *reading the filter*: a filter that's element-wise, or an algebraic
//! aggregation (top-N, sum, count, min/max), can run per file with a bounded
//! merge and produce the identical result.
//!
//! This module parses the jaq filter into jaq's own AST and classifies it into
//! a [`Plan`]. Recognition is deliberately conservative — anything we can't
//! prove decomposable falls back to [`Plan::Slurp`] (the whole-array path),
//! which is always correct. So the planner never changes an answer; it only
//! lowers memory for the shapes it recognizes.
//!
//! Correctness rests on two facts:
//! - **Distributive** filters (`.[] | g`, `map(g)`) satisfy
//!   `F(a ++ b) == F(a) ++ F(b)`, so per-file execution + concatenation is exact.
//! - **Algebraic** aggregations split into a per-file partial and a combine:
//!   a global top-N is a subset of the per-file top-Ns, `length` is the sum of
//!   per-file lengths, etc. We run the whole filter per file (so any
//!   distributive prefix is applied exactly once) and then run a *combine*
//!   filter over the concatenated partials.

use jaq_core::load::{self, parse::BinaryOp, parse::Term};
use jaq_core::path::Part;
#[cfg(not(target_os = "emscripten"))]
use jaq_core::{load::lex::StrPart, ops::Cmp, path::Opt};

/// How to execute a filter over the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Load the whole array and run the filter once. Always correct; the
    /// fallback for anything we can't prove decomposable.
    Slurp,
    /// The filter streams per element (`.[] | g`). Run it on each file's step
    /// array and print outputs immediately — nothing accumulates.
    PerFileStream,
    /// Run the filter per file, concatenate the per-file outputs into one
    /// array, then run `reduce` over that array. Covers `map(g)` (reduce =
    /// `add`, i.e. array concatenation), top-N (`add | sort_by(k) | .[:N]`),
    /// and `length` (reduce = `add` over exact integer counts). Scalar `add`,
    /// `min`, and `max` deliberately do NOT decompose — see [`scalar_reduce`].
    Decompose { reduce: String },
}

impl Plan {
    /// One-line description for `TOOLPATH_QUERY_EXPLAIN`.
    pub fn describe(&self) -> String {
        match self {
            Plan::Slurp => "slurp (whole array; not decomposable)".to_string(),
            Plan::PerFileStream => {
                "stream per file (element-wise; bounded to one file)".to_string()
            }
            Plan::Decompose { reduce } => {
                format!("decompose per file, then reduce with `{reduce}` (bounded)")
            }
        }
    }
}

/// A per-row predicate recognized from a filter's leading `select`, mappable
/// to SQL over the step index's generated columns. Recognition is exact, not
/// merely implied: on wrapper-produced rows (`dead_end` always a boolean,
/// `.step.actor` always a string) each atom means precisely its jq
/// counterpart, so one recognizer serves both index prefiltering and the
/// fully absorbed `count(*)` path. Anything not on this whitelist simply
/// isn't recognized — the query still runs, unprefiltered.
#[cfg(not(target_os = "emscripten"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowPredicate {
    /// `select(.dead_end)` / `select(.dead_end | not)`
    DeadEnd(bool),
    /// `select(.step.actor == "…")`
    ActorEq(String),
    /// `select(.step.actor | startswith("…"))`
    ActorStartsWith(String),
    /// `select(.path.meta.source == "…")`
    SourceEq(String),
    /// `select(a and b)`
    And(Box<RowPredicate>, Box<RowPredicate>),
}

#[cfg(not(target_os = "emscripten"))]
impl RowPredicate {
    /// The in-code mirror of the SQL clause, applied to rows that were just
    /// reparsed from a stale file (the index's WHERE couldn't run). Must
    /// agree with `index::predicate_clause` on every wrapper-produced row.
    pub fn matches(&self, row: &serde_json::Value) -> bool {
        match self {
            RowPredicate::DeadEnd(b) => row["dead_end"].as_bool() == Some(*b),
            RowPredicate::ActorEq(s) => row["step"]["actor"].as_str() == Some(s),
            RowPredicate::ActorStartsWith(s) => row["step"]["actor"]
                .as_str()
                .is_some_and(|a| a.starts_with(s)),
            RowPredicate::SourceEq(s) => row["path"]["meta"]["source"].as_str() == Some(s),
            RowPredicate::And(l, r) => l.matches(row) && r.matches(row),
        }
    }

    /// Short form for `TOOLPATH_QUERY_EXPLAIN`.
    pub fn describe(&self) -> String {
        match self {
            RowPredicate::DeadEnd(b) => format!("dead_end={b}"),
            RowPredicate::ActorEq(s) => format!("actor={s:?}"),
            RowPredicate::ActorStartsWith(s) => format!("actor^={s:?}"),
            RowPredicate::SourceEq(s) => format!("source={s:?}"),
            RowPredicate::And(l, r) => format!("{} and {}", l.describe(), r.describe()),
        }
    }
}

/// The predicate established by the filter's first pipeline stage, if fully
/// recognized: `map(select(P)) | …`, `[.[] | select(P)] | …`, or
/// `.[] | select(P) | …`. Rows failing `P` are dropped by that first stage
/// before anything downstream can observe them, so prefiltering the input
/// with `P` leaves every later stage's input — and the final answer —
/// unchanged, whatever the plan.
#[cfg(not(target_os = "emscripten"))]
pub fn row_predicate(code: &str) -> Option<RowPredicate> {
    let term = load::parse(code, |p| p.term())?;
    let segs = pipe_segments(&term)?;
    if let Some(body) = map_select_body(segs[0]) {
        return predicate(body);
    }
    if is_iterate(segs[0])
        && let Some(second) = segs.get(1)
        && let Some(body) = select_body(second)
    {
        return predicate(body);
    }
    None
}

/// Whether the whole filter is a count the index can answer with
/// `SELECT count(*)`: bare `length`, or `map(select(P)) | length` (and the
/// collected-iteration spelling) with `P` fully recognized. Returns the
/// predicate to count under (`None` = count everything).
#[cfg(not(target_os = "emscripten"))]
pub fn absorbable_count(code: &str) -> Option<Option<RowPredicate>> {
    let term = load::parse(code, |p| p.term())?;
    let segs = pipe_segments(&term)?;
    let is_length = |t: &Term<&str>| matches!(t, Term::Call(name, args) if *name == "length" && args.is_empty());
    match segs.as_slice() {
        [only] if is_length(only) => Some(None),
        [head, tail] if is_length(tail) => {
            let body = map_select_body(head)?;
            Some(Some(predicate(body)?))
        }
        _ => None,
    }
}

/// `select(P)` → `P`.
#[cfg(not(target_os = "emscripten"))]
fn select_body<'a, 's>(t: &'a Term<&'s str>) -> Option<&'a Term<&'s str>> {
    match t {
        Term::Call(name, args) if *name == "select" && args.len() == 1 => Some(&args[0]),
        _ => None,
    }
}

/// `map(select(P))` or `[.[] | select(P)]` → `P`.
#[cfg(not(target_os = "emscripten"))]
fn map_select_body<'a, 's>(t: &'a Term<&'s str>) -> Option<&'a Term<&'s str>> {
    if let Term::Call(name, args) = t
        && *name == "map"
        && args.len() == 1
    {
        return select_body(&args[0]);
    }
    if let Term::Arr(Some(inner)) = t {
        let segs = pipe_segments(inner)?;
        if let [head, sel] = segs.as_slice()
            && is_iterate(head)
        {
            return select_body(sel);
        }
    }
    None
}

#[cfg(not(target_os = "emscripten"))]
fn predicate(t: &Term<&str>) -> Option<RowPredicate> {
    match t {
        Term::BinOp(l, BinaryOp::And, r) => Some(RowPredicate::And(
            Box::new(predicate(l)?),
            Box::new(predicate(r)?),
        )),
        // `.step.actor == "…"` / `.path.meta.source == "…"` (either order).
        Term::BinOp(l, BinaryOp::Cmp(Cmp::Eq), r) => {
            let (path, lit) = match (field_path(l), str_lit(r)) {
                (Some(p), Some(s)) => (p, s),
                _ => (field_path(r)?, str_lit(l)?),
            };
            match path.as_slice() {
                ["step", "actor"] => Some(RowPredicate::ActorEq(lit)),
                ["path", "meta", "source"] => Some(RowPredicate::SourceEq(lit)),
                _ => None,
            }
        }
        // `.step.actor | startswith("…")`
        Term::BinOp(l, BinaryOp::Pipe(None), r) => match r.as_ref() {
            Term::Call(name, args)
                if *name == "startswith"
                    && args.len() == 1
                    && field_path(l).as_deref() == Some(&["step", "actor"]) =>
            {
                Some(RowPredicate::ActorStartsWith(str_lit(&args[0])?))
            }
            Term::Call(name, args)
                if *name == "not"
                    && args.is_empty()
                    && field_path(l).as_deref() == Some(&["dead_end"]) =>
            {
                Some(RowPredicate::DeadEnd(false))
            }
            _ => None,
        },
        // `.dead_end` — truthiness; exact because the wrapper always writes
        // a boolean.
        _ => match field_path(t)?.as_slice() {
            ["dead_end"] => Some(RowPredicate::DeadEnd(true)),
            _ => None,
        },
    }
}

/// `.a.b.c` as `["a","b","c"]` — literal, non-optional field accesses only.
#[cfg(not(target_os = "emscripten"))]
fn field_path<'a>(t: &Term<&'a str>) -> Option<Vec<&'a str>> {
    let Term::Path(inner, path) = t else {
        return None;
    };
    if !matches!(inner.as_ref(), Term::Id) {
        return None;
    }
    let mut fields = Vec::with_capacity(path.0.len());
    for (part, opt) in &path.0 {
        if !matches!(opt, Opt::Essential) {
            return None;
        }
        let Part::Index(idx) = part else {
            return None;
        };
        fields.push(term_str(idx)?);
    }
    (!fields.is_empty()).then_some(fields)
}

/// A plain string literal (no interpolation, no escapes, no format).
#[cfg(not(target_os = "emscripten"))]
fn term_str<'a>(t: &Term<&'a str>) -> Option<&'a str> {
    match t {
        Term::Str(None, parts) => match parts.as_slice() {
            [StrPart::Str(s)] => Some(s),
            [] => Some(""),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(not(target_os = "emscripten"))]
fn str_lit(t: &Term<&str>) -> Option<String> {
    term_str(t).map(str::to_string)
}

/// Classify `code` into a [`Plan`]. Unparseable or unrecognized filters map to
/// [`Plan::Slurp`] (the main executor re-parses and surfaces any real error).
pub fn analyze(code: &str) -> Plan {
    let Some(term) = load::parse(code, |p| p.term()) else {
        return Plan::Slurp;
    };
    let Some(segs) = pipe_segments(&term) else {
        return Plan::Slurp;
    };
    let plan = classify(code, &segs);
    // Safety net: a derived `reduce` is built by recovering the tail's source
    // span, which can be unbalanced when the tail is wrapped (e.g. a
    // parenthesized `sort_by`). If the combine doesn't itself parse, fall back
    // to the always-correct slurp rather than emit a broken filter.
    if let Plan::Decompose { reduce } = &plan
        && !reparses(reduce)
    {
        return Plan::Slurp;
    }
    plan
}

/// Whether `code` parses as a jaq term.
fn reparses(code: &str) -> bool {
    load::parse(code, |p| p.term()).is_some()
}

/// Flatten a top-level `a | b | c` chain into `[a, b, c]`. jaq parses pipe
/// right-associatively as `BinOp(a, Pipe(None), BinOp(b, Pipe(None), c))`.
/// Returns `None` if a top-level `as`-binding (`… as $x | …`) is present — we
/// don't reason about variable scope, so those slurp.
fn pipe_segments<'a, 's>(t: &'a Term<&'s str>) -> Option<Vec<&'a Term<&'s str>>> {
    let mut segs = Vec::new();
    let mut cur = t;
    loop {
        match cur {
            Term::BinOp(l, BinaryOp::Pipe(None), r) => {
                segs.push(l.as_ref());
                cur = r.as_ref();
            }
            Term::BinOp(_, BinaryOp::Pipe(Some(_)), _) => return None,
            other => {
                segs.push(other);
                break;
            }
        }
    }
    Some(segs)
}

fn classify(code: &str, segs: &[&Term<&str>]) -> Plan {
    let last = *segs.last().expect("at least one segment");

    // `.[] | …` (or a bare `.[]`): element-wise stream, distributive for any
    // downstream `g` since `g` only ever sees one element.
    if is_iterate(segs[0]) {
        return Plan::PerFileStream;
    }

    // Single segment with no pipe.
    if segs.len() == 1 {
        if is_map(last) || is_collected_iteration(last) {
            return Plan::Decompose {
                reduce: "add".to_string(),
            };
        }
        if let Some(reduce) = scalar_reduce(last) {
            return Plan::Decompose {
                reduce: reduce.to_string(),
            };
        }
        return Plan::Slurp;
    }

    // Top-N: `<prefix> | sort_by(k) | .[:N]`. The prefix runs per file inside
    // the filter; the combine re-applies `sort_by(k) | .[:N]` to the merged
    // per-file top-Ns. A global top-N element is necessarily in its file's
    // top-N, so this is exact.
    if is_slice_upto(last) {
        let before = &segs[..segs.len() - 1];
        if let Some(sort_seg) = before.last().copied().filter(|s| is_sort_by(s))
            && prefix_ok(&before[..before.len() - 1])
        {
            // Tail spans the last two segments; source runs from the start of
            // `sort_by` to the end of `code`.
            let tail = tail_source(code, sort_seg);
            return Plan::Decompose {
                reduce: format!("add | {tail}"),
            };
        }
        return Plan::Slurp;
    }

    // Scalar reduction with a distributive prefix: `<prefix> | length` etc.
    if let Some(reduce) = scalar_reduce(last)
        && prefix_ok(&segs[..segs.len() - 1])
    {
        return Plan::Decompose {
            reduce: reduce.to_string(),
        };
    }

    Plan::Slurp
}

/// Source text of a call segment: from the call's name to the end of `code`.
/// Relies on jaq's AST holding `&str` slices *into* `code`, so the name's start
/// offset is recoverable by pointer arithmetic ([`load::span`]).
fn tail_source<'a>(code: &'a str, call: &Term<&str>) -> &'a str {
    match call {
        Term::Call(name, _) => &code[load::span(code, name).start..],
        _ => code,
    }
}

/// Is the prefix (segments before the aggregation tail) safely distributive?
/// Conservative: empty, or a single `map(_)` / `[.[] | _]` stage. A prefix
/// containing its own aggregation (`unique`, `sort`, `group_by`, …) is not
/// distributive and must slurp.
fn prefix_ok(prefix: &[&Term<&str>]) -> bool {
    match prefix {
        [] => true,
        [seg] => is_map(seg) || is_collected_iteration(seg),
        _ => false,
    }
}

fn is_iterate(t: &Term<&str>) -> bool {
    matches!(t, Term::Path(inner, path)
        if matches!(inner.as_ref(), Term::Id)
        && path.0.len() == 1
        && matches!(path.0[0].0, Part::Range(None, None)))
}

/// `.[:N]` where `N` is a **literal non-negative integer**. This is the only
/// slice that decomposes: per-file `.[:N]` then merge-and-`.[:N]` is exact for
/// a fixed nonnegative cutoff. `.[:-1]` (drop-last) and `.[:length-1]`
/// (dynamic) do **not** — each file would truncate its own tail before the
/// merge — so those must slurp.
fn is_slice_upto(t: &Term<&str>) -> bool {
    if let Term::Path(inner, path) = t
        && matches!(inner.as_ref(), Term::Id)
        && path.0.len() == 1
        && let Part::Range(None, Some(bound)) = &path.0[0].0
    {
        return matches!(bound, Term::Num(s) if s.parse::<u64>().is_ok());
    }
    false
}

fn is_map(t: &Term<&str>) -> bool {
    matches!(t, Term::Call(name, args) if *name == "map" && args.len() == 1)
}

fn is_sort_by(t: &Term<&str>) -> bool {
    matches!(t, Term::Call(name, args) if *name == "sort_by" && args.len() == 1)
}

/// `[.[] | g]` — collect an element-wise iteration into an array. Distributive.
fn is_collected_iteration(t: &Term<&str>) -> bool {
    match t {
        Term::Arr(Some(inner)) => match pipe_segments(inner) {
            Some(segs) => is_iterate(segs[0]),
            None => false,
        },
        _ => false,
    }
}

/// If `t` is a scalar reduction we know how to combine, return the combine
/// filter. Only `length` qualifies: per-file counts are exact integers, so
/// summing them is associative. The others deliberately do NOT decompose:
/// - scalar `add`: float addition is not associative, so re-associating the
///   sum across per-file partials can change the answer (1e100 + (-1e100 + 1)
///   is 0.0 grouped one way and 1.0 the other);
/// - `min`/`max`: `[] | min == null` from an empty (or fully scoped-out)
///   document, and `null` sorts below everything, poisoning the merge.
///
/// All three slurp instead, which is always correct.
fn scalar_reduce(t: &Term<&str>) -> Option<&'static str> {
    match t {
        Term::Call(name, args) if args.is_empty() && *name == "length" => Some("add"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decompose(reduce: &str) -> Plan {
        Plan::Decompose {
            reduce: reduce.to_string(),
        }
    }

    #[test]
    fn stream_for_iteration() {
        assert_eq!(analyze(".[]"), Plan::PerFileStream);
        assert_eq!(analyze(".[] | select(.dead_end)"), Plan::PerFileStream);
        assert_eq!(
            analyze(r#".[] | select(.step.actor | startswith("agent:")) | .step.id"#),
            Plan::PerFileStream
        );
    }

    #[test]
    fn map_decomposes_to_add() {
        assert_eq!(analyze("map(select(.dead_end))"), decompose("add"));
        assert_eq!(analyze("[.[] | select(.dead_end)]"), decompose("add"));
    }

    #[test]
    fn top_n_decomposes_with_sort_tail() {
        assert_eq!(
            analyze("sort_by(-.tokens) | .[:10]"),
            decompose("add | sort_by(-.tokens) | .[:10]")
        );
        assert_eq!(
            analyze("map({t: .step.id}) | sort_by(-.t) | .[:5]"),
            decompose("add | sort_by(-.t) | .[:5]")
        );
    }

    #[test]
    fn scalar_reductions() {
        assert_eq!(analyze("length"), decompose("add"));
        assert_eq!(analyze("map(select(.dead_end)) | length"), decompose("add"));
    }

    #[test]
    fn scalar_add_slurps_because_float_sums_reassociate() {
        // 1e100 + (-1e100 + 1) == 1.0 but (1e100 + -1e100) + 1 grouped by file
        // is 0.0 — decomposing a scalar sum changes float answers.
        assert_eq!(analyze("add"), Plan::Slurp);
        assert_eq!(analyze("map(.tokens) | add"), Plan::Slurp);
        assert_eq!(analyze("[.[].tokens] | add"), Plan::Slurp);
    }

    #[test]
    fn non_distributive_prefix_slurps() {
        // `unique` before the top-N isn't distributive — cross-file dupes would
        // survive a per-file pass.
        assert_eq!(analyze("unique | sort_by(.x) | .[:10]"), Plan::Slurp);
        // group_by is holistic.
        assert_eq!(
            analyze("group_by(.path.meta.source) | map({n: length})"),
            Plan::Slurp
        );
    }

    #[test]
    fn bare_slice_without_sort_slurps() {
        // First-N without a sort is decomposable too, but we keep the planner
        // small: only sort-backed top-N streams; a bare slice slurps.
        assert_eq!(analyze("map(.step) | .[:10]"), Plan::Slurp);
    }

    #[test]
    fn as_binding_and_reduce_slurp() {
        assert_eq!(analyze(".tokens as $t | $t"), Plan::Slurp);
        assert_eq!(analyze("reduce .[] as $x (0; . + $x.n)"), Plan::Slurp);
    }

    #[test]
    fn bare_identity_slurps() {
        // `.` (whole array) has no decomposition; slurp (it's cheap anyway).
        assert_eq!(analyze("."), Plan::Slurp);
    }

    // ── Row-predicate recognition (index pushdown) ────────────────────

    #[test]
    fn recognizes_leading_select_predicates() {
        assert_eq!(
            row_predicate("map(select(.dead_end))"),
            Some(RowPredicate::DeadEnd(true))
        );
        assert_eq!(
            row_predicate(".[] | select(.dead_end | not) | .step.id"),
            Some(RowPredicate::DeadEnd(false))
        );
        assert_eq!(
            row_predicate("[.[] | select(.step.actor == \"human:ben\")] | length"),
            Some(RowPredicate::ActorEq("human:ben".into()))
        );
        assert_eq!(
            row_predicate(r#"map(select(.step.actor | startswith("agent:")))"#),
            Some(RowPredicate::ActorStartsWith("agent:".into()))
        );
        assert_eq!(
            row_predicate(r#"map(select(.path.meta.source == "claude-code")) | length"#),
            Some(RowPredicate::SourceEq("claude-code".into()))
        );
        assert_eq!(
            row_predicate(r#"map(select(.dead_end and .step.actor == "agent:x"))"#),
            Some(RowPredicate::And(
                Box::new(RowPredicate::DeadEnd(true)),
                Box::new(RowPredicate::ActorEq("agent:x".into()))
            ))
        );
        // Literal-first comparison order.
        assert_eq!(
            row_predicate(r#"map(select("agent:x" == .step.actor))"#),
            Some(RowPredicate::ActorEq("agent:x".into()))
        );
    }

    #[test]
    fn unrecognized_predicates_stay_none() {
        // A partially recognized conjunction is not used (whole-P only).
        assert_eq!(
            row_predicate("map(select(.dead_end and (.change | length > 2)))"),
            None
        );
        // Fields off the whitelist.
        assert_eq!(row_predicate("map(select(.step.intent == \"x\"))"), None);
        // Optional access, dynamic values, non-select heads.
        assert_eq!(row_predicate("map(select(.dead_end?))"), None);
        assert_eq!(row_predicate("map(select(.step.actor == .other))"), None);
        assert_eq!(row_predicate("map(.step)"), None);
        assert_eq!(row_predicate("group_by(.step.actor)"), None);
        // select not in the first stage: prefiltering could change what the
        // first stage sees, so it is not recognized.
        assert_eq!(row_predicate("map(.step) | map(select(.dead_end))"), None);
        // Interpolated strings are not literals.
        assert_eq!(
            row_predicate(r#"map(select(.step.actor == "a\(.x)"))"#),
            None
        );
    }

    #[test]
    fn matches_mirrors_the_predicates() {
        use serde_json::json;
        let row = json!({
            "dead_end": true,
            "step": {"actor": "agent:claude"},
            "path": {"meta": {"source": "claude-code"}}
        });
        assert!(RowPredicate::DeadEnd(true).matches(&row));
        assert!(!RowPredicate::DeadEnd(false).matches(&row));
        assert!(RowPredicate::ActorEq("agent:claude".into()).matches(&row));
        assert!(RowPredicate::ActorStartsWith("agent:".into()).matches(&row));
        assert!(!RowPredicate::ActorStartsWith("human:".into()).matches(&row));
        assert!(RowPredicate::SourceEq("claude-code".into()).matches(&row));
        assert!(
            RowPredicate::And(
                Box::new(RowPredicate::DeadEnd(true)),
                Box::new(RowPredicate::ActorStartsWith("agent:".into()))
            )
            .matches(&row)
        );
        // Missing source: `null == "x"` is false, like the SQL NULL.
        let no_meta = json!({"dead_end": false, "step": {"actor": "a"}, "path": {"id": "p"}});
        assert!(!RowPredicate::SourceEq("claude-code".into()).matches(&no_meta));
    }

    #[test]
    fn absorbable_counts() {
        assert_eq!(absorbable_count("length"), Some(None));
        assert_eq!(
            absorbable_count("map(select(.dead_end)) | length"),
            Some(Some(RowPredicate::DeadEnd(true)))
        );
        assert_eq!(
            absorbable_count(r#"[.[] | select(.step.actor | startswith("agent:"))] | length"#),
            Some(Some(RowPredicate::ActorStartsWith("agent:".into())))
        );
        // Not a bare count: extra stages, unrecognized predicates, other tails.
        assert_eq!(
            absorbable_count("map(select(.dead_end)) | length + 1"),
            None
        );
        assert_eq!(absorbable_count("map(select(.tokens > 3)) | length"), None);
        assert_eq!(absorbable_count("map(.step) | length"), None);
        assert_eq!(absorbable_count(".[] | length"), None);
        assert_eq!(absorbable_count("length | tostring"), None);
    }
}
