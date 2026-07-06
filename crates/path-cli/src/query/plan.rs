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
}
