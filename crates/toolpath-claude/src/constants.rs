//! JSON keys of the Claude Code session file format, for the code that
//! reads or writes a line as a `serde_json::Value`. Typed fields name
//! the same keys through `#[serde(rename)]`, which takes only a
//! literal.

/// The session a line belongs to.
pub(crate) const SESSION_ID: &str = "sessionId";

/// The working directory a line was recorded in.
pub(crate) const CWD: &str = "cwd";
