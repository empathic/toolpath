//! Shell tool calls: the script and working directory a call names, and
//! the outcome its result reports.

use serde_json::Value;
use toolpath_convo::ToolResult;
use toolpath_convo::shell_writes::{ParsedScript, normalize_path, parse_argv, parse_script};

/// What a shell call's input says: the parsed command it runs, and where
/// and how it runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellCall {
    pub parsed: ParsedScript,
    /// The directory the call names, joined onto relative targets.
    pub workdir: Option<String>,
    /// The input names a directory this reader does not read (an unknown
    /// directory-like key, or two keys that disagree): relative targets
    /// cannot be resolved.
    pub workdir_unknown: bool,
    /// Run in the background: the result is not the command's outcome.
    pub background: bool,
}

/// Keys naming the call's working directory: Codex's `workdir`, Gemini
/// CLI's `dir_path` (older releases: `directory`), and `cwd`.
const WORKDIR_KEYS: [&str; 4] = ["workdir", "cwd", "directory", "dir_path"];
/// Keys running the command in the background: Gemini CLI's
/// `is_background`, Claude Code's `run_in_background`.
const BACKGROUND_KEYS: [&str; 2] = ["is_background", "run_in_background"];

/// Reads `{command: script}`, `{cmd: script, workdir}` (Codex
/// `exec_command`), `{command: argv, workdir}` (Codex `shell`, read by
/// [`parse_argv`]) and `{command: script, dir_path, is_background}`
/// (Gemini CLI `run_shell_command`).
pub fn shell_call(input: &Value) -> Option<ShellCall> {
    let parsed = match input.get("cmd").or_else(|| input.get("command"))? {
        Value::String(s) => parse_script(s),
        Value::Array(argv) => {
            let argv: Vec<&str> = argv.iter().map(Value::as_str).collect::<Option<_>>()?;
            parse_argv(&argv)
        }
        _ => return None,
    };
    let mut workdir: Option<String> = None;
    let mut workdir_unknown = false;
    let mut background = false;
    for (key, value) in input.as_object().into_iter().flatten() {
        if value.is_null() || value.as_str() == Some("") {
            continue;
        }
        if BACKGROUND_KEYS.contains(&key.as_str()) {
            background |= value != &Value::Bool(false);
        } else if WORKDIR_KEYS.contains(&key.as_str()) {
            match (value.as_str(), &workdir) {
                (Some(d), None) => workdir = Some(d.to_string()),
                (Some(d), Some(w)) if normalize_path(d) == normalize_path(w) => {}
                _ => workdir_unknown = true,
            }
        } else if names_dir(key) {
            workdir_unknown = true;
        }
    }
    Some(ShellCall {
        parsed,
        workdir,
        workdir_unknown,
        background,
    })
}

fn names_dir(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["dir", "cwd", "path", "folder", "location"]
        .iter()
        .any(|w| key.contains(w))
}

/// How a shell call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
    Unknown,
}

impl Outcome {
    /// The serialized name.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::Failure => "failure",
            Outcome::Unknown => "unknown",
        }
    }
}

/// What an [`Outcome`] rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// An exit status the harness wrote into the result.
    ExitCode,
    /// The result is flagged as an error.
    IsError,
    /// A result with no exit status and no error flag.
    NoErrorReported,
    /// No result: the call was never answered in the capture.
    NoResult,
    /// Codex reports the process still running.
    StillRunning,
    /// The call ran the command in the background.
    Background,
}

impl Basis {
    /// The serialized name.
    pub fn as_str(self) -> &'static str {
        match self {
            Basis::ExitCode => "exit_code",
            Basis::IsError => "is_error",
            Basis::NoErrorReported => "no_error_reported",
            Basis::NoResult => "no_result",
            Basis::StillRunning => "still_running",
            Basis::Background => "background",
        }
    }
}

/// A shell call's outcome, its basis and any exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellOutcome {
    pub outcome: Outcome,
    pub basis: Basis,
    pub exit_code: Option<i64>,
}

/// The outcome `tool`'s result reports. The error flag wins over an exit
/// status (a sandbox denial can carry exit 0).
pub fn shell_outcome(tool: &str, result: Option<&ToolResult>) -> ShellOutcome {
    let Some(r) = result else {
        return ShellOutcome {
            outcome: Outcome::Unknown,
            basis: Basis::NoResult,
            exit_code: None,
        };
    };
    let status = exit_status(tool, &r.content);
    let exit_code = match status {
        Some(Status::Exited(c)) => Some(c),
        _ => None,
    };
    let (outcome, basis) = match (r.is_error, status) {
        (true, _) => (Outcome::Failure, Basis::IsError),
        (false, Some(Status::Exited(0))) => (Outcome::Success, Basis::ExitCode),
        (false, Some(Status::Exited(_))) => (Outcome::Failure, Basis::ExitCode),
        (false, Some(Status::Running)) => (Outcome::Unknown, Basis::StillRunning),
        (false, None) => (Outcome::Success, Basis::NoErrorReported),
    };
    ShellOutcome {
        outcome,
        basis,
        exit_code,
    }
}

/// The outcome of `call`, a shell call to `tool`.
pub fn call_outcome(tool: &str, call: &ShellCall, result: Option<&ToolResult>) -> ShellOutcome {
    if call.background {
        return ShellOutcome {
            outcome: Outcome::Unknown,
            basis: Basis::Background,
            exit_code: None,
        };
    }
    shell_outcome(tool, result)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Exited(i64),
    Running,
}

fn int(s: &str) -> Option<i64> {
    s.trim().parse().ok()
}

/// Only tools with a documented result format have an exit status, so
/// program output that happens to contain a marker is not mistaken for one.
fn exit_status(tool: &str, content: &str) -> Option<Status> {
    match tool {
        "exec_command" => unified_exec_status(content),
        "shell" => json_envelope_status(content),
        _ => None,
    }
}

/// Codex unified exec (`exec_command`): header lines before `Output:`
/// (docs/agents/formats/codex.md).
fn unified_exec_status(content: &str) -> Option<Status> {
    let lines: Vec<&str> = content.lines().collect();
    let end = lines.iter().position(|l| *l == "Output:")?;
    lines[..end].iter().find_map(|line| {
        if let Some(n) = line.strip_prefix("Process exited with code ").and_then(int) {
            Some(Status::Exited(n))
        } else if line.starts_with("Process running with session ID") {
            Some(Status::Running)
        } else {
            None
        }
    })
}

/// Codex's JSON function-output envelope `{"output": …, "metadata":
/// {"exit_code": N, …}}` (docs/agents/formats/codex.md), read only when
/// the whole result is that JSON.
fn json_envelope_status(content: &str) -> Option<Status> {
    if !content.trim_start().starts_with('{') {
        return None;
    }
    let v: Value = serde_json::from_str(content).ok()?;
    v.pointer("/metadata/exit_code")
        .and_then(Value::as_i64)
        .map(Status::Exited)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn result(content: &str, is_error: bool) -> ToolResult {
        ToolResult {
            content: content.into(),
            is_error,
        }
    }

    fn of(tool: &str, content: &str, is_error: bool) -> (Outcome, Basis, Option<i64>) {
        let o = shell_outcome(tool, Some(&result(content, is_error)));
        (o.outcome, o.basis, o.exit_code)
    }

    #[test]
    fn scripts_and_workdirs_per_harness() {
        let write = "cat <<'EOF' > a\nx\nEOF";
        for (input, workdir) in [
            (json!({"command": write, "description": "d"}), None),
            (
                json!({"cmd": write, "workdir": "/work/project"}),
                Some("/work/project"),
            ),
            (
                json!({"command": ["bash", "-lc", write], "workdir": "/w"}),
                Some("/w"),
            ),
            (json!({"command": ["/bin/zsh", "-c", write]}), None),
            (json!({"command": write, "workdir": ""}), None),
            // Gemini CLI `run_shell_command`, current and older releases.
            (
                json!({"command": write, "dir_path": "sub", "is_background": false}),
                Some("sub"),
            ),
            (
                json!({"command": write, "directory": "/work/project"}),
                Some("/work/project"),
            ),
        ] {
            let call = shell_call(&input).unwrap();
            assert_eq!(call.parsed, parse_script(write), "{input}");
            assert_eq!(call.workdir.as_deref(), workdir, "{input}");
            assert!(!call.workdir_unknown && !call.background, "{input}");
        }
        for input in [
            json!({"command": write, "working_dir": "sub"}),
            json!({"command": write, "targetPath": "/w"}),
            json!({"command": write, "workdir": "/a", "cwd": "/b"}),
            json!({"command": write, "workdir": ["/a"]}),
        ] {
            assert!(shell_call(&input).unwrap().workdir_unknown, "{input}");
        }
        for input in [
            json!({"command": write, "is_background": true}),
            json!({"command": write, "run_in_background": true}),
        ] {
            assert!(shell_call(&input).unwrap().background, "{input}");
        }
        assert_eq!(shell_call(&json!("{\"cmd\": \"ls")), None);
        assert_eq!(shell_call(&json!({"description": "no command"})), None);
        assert_eq!(shell_call(&json!({"command": ["ls", 1]})), None);
    }

    #[test]
    fn a_non_shell_argv_is_one_other() {
        let script = "cat > f <<EOF\nx\nEOF";
        for argv in [
            json!(["python3", "-c", script]),
            json!(["node", "-c", script]),
            json!(["ls", "-a"]),
        ] {
            let parsed = shell_call(&json!({"command": argv})).unwrap().parsed;
            assert_eq!(parsed.writes().count(), 0, "{argv}");
            assert_eq!(parsed.simple_commands, 0, "{argv}");
        }
    }

    #[test]
    fn codex_unified_exec_header() {
        let ok = "Chunk ID: ff886d\nWall time: 0.0000 seconds\nProcess exited with code 0\nOriginal token count: 17\nOutput:\n/work/project\n";
        assert_eq!(
            of("exec_command", ok, false),
            (Outcome::Success, Basis::ExitCode, Some(0))
        );
        let fail = "Command: /bin/bash -lc false\nChunk ID: 1\nWall time: 0.0 seconds\nProcess exited with code 128\nOutput:\n";
        assert_eq!(
            of("exec_command", fail, false),
            (Outcome::Failure, Basis::ExitCode, Some(128))
        );
        let running =
            "Chunk ID: 2\nWall time: 10.0 seconds\nProcess running with session ID 7\nOutput:\n";
        assert_eq!(
            of("exec_command", running, false),
            (Outcome::Unknown, Basis::StillRunning, None)
        );
        // The marker in the output body is the program's, not Codex's.
        let echoed = "Chunk ID: 3\nWall time: 0.0 seconds\nOutput:\nProcess exited with code 9\n";
        assert_eq!(
            of("exec_command", echoed, false),
            (Outcome::Success, Basis::NoErrorReported, None)
        );
    }

    #[test]
    fn codex_shell_json_envelope() {
        let ok = r#"{"output":"done\n","metadata":{"exit_code":0,"duration_seconds":0.1}}"#;
        assert_eq!(
            of("shell", ok, false),
            (Outcome::Success, Basis::ExitCode, Some(0))
        );
        let fail = r#"{"output":"","metadata":{"exit_code":2}}"#;
        assert_eq!(
            of("shell", fail, false),
            (Outcome::Failure, Basis::ExitCode, Some(2))
        );
        assert_eq!(
            of("shell", "{not json", false),
            (Outcome::Success, Basis::NoErrorReported, None)
        );
    }

    #[test]
    fn other_shell_tools_are_read_by_the_error_flag_alone() {
        let unified = "Chunk ID: 1\nProcess exited with code 1\nOutput:\n";
        let envelope = r#"{"output":"","metadata":{"exit_code":2}}"#;
        for tool in ["Bash", "bash", "shell_command", "run_shell_command"] {
            for content in [
                "Exit code 1\nboom",
                "Exit code: 127\nOutput:\nnope",
                "partial\nCommand exited with code 3\n",
                unified,
                envelope,
            ] {
                assert_eq!(
                    of(tool, content, false),
                    (Outcome::Success, Basis::NoErrorReported, None),
                    "{tool} {content:?}"
                );
                assert_eq!(
                    of(tool, content, true),
                    (Outcome::Failure, Basis::IsError, None),
                    "{tool} {content:?}"
                );
            }
        }
    }

    #[test]
    fn the_error_flag_wins_and_no_result_is_unknown() {
        assert_eq!(
            of(
                "exec_command",
                "Chunk ID: 1\nProcess exited with code 0\nOutput:\n",
                true
            ),
            (Outcome::Failure, Basis::IsError, Some(0))
        );
        assert_eq!(
            of("exec_command", "", false),
            (Outcome::Success, Basis::NoErrorReported, None)
        );
        assert_eq!(
            shell_outcome("exec_command", None),
            ShellOutcome {
                outcome: Outcome::Unknown,
                basis: Basis::NoResult,
                exit_code: None
            }
        );
    }
}
