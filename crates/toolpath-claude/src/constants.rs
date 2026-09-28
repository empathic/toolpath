//! JSON keys of the Claude Code session file format, for the code that
//! reads or writes a line as a `serde_json::Value`. Typed fields name
//! the same keys through `#[serde(rename)]`, which takes only a
//! literal.

/// The session a line belongs to.
pub(crate) const SESSION_ID: &str = "sessionId";

/// The working directory a line was recorded in.
pub(crate) const CWD: &str = "cwd";

/// Marks a line Claude Code writes for itself and hides from the
/// transcript it sends to the API: image sources, command caveats,
/// system reminders.
pub(crate) const IS_META: &str = "isMeta";

/// The start of the text of a user entry Claude Code writes without
/// `isMeta`: interrupt markers, slash-command echoes, command output,
/// and task notifications.
pub(crate) const HARNESS_TEXT_PREFIXES: [&str; 6] = [
    "[Request interrupted",
    "<command-name>",
    "<command-message>",
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<task-notification>",
];
