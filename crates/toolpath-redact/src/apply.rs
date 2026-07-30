//! Rewriting a document from an approved plan.

/// Rewrite `path` in place according to `plan`.
///
/// Nothing the plan does not name is touched: with no findings, the
/// serialised output is byte-identical to the input.
pub fn apply(
    _path: &mut toolpath::v1::Path,
    _plan: &crate::plan::Plan,
    _cfg: &crate::RedactConfig,
) -> crate::Result<crate::RedactReport> {
    todo!("T7")
}
