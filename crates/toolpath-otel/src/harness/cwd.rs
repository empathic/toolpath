//! Working directory from a harness's environment block.

use crate::generation::Generation;
use crate::normalize::{content_text, is_system_like};

/// Line markers; the working directory is the rest of the marker's line.
const LINE_MARKERS: [&str; 3] = [
    // Claude Code.
    "Primary working directory:",
    // opencode `<env>` (sst/opencode session/system.ts).
    "Working directory:",
    // pi before its XML prompt sections (pi-mono coding-agent system-prompt.ts).
    "Current working directory:",
];

/// The working directory named in `text`, if a known marker is present.
/// Also reads `<cwd>…</cwd>` (Codex `<environment_context>`, newer pi).
pub fn cwd_in(text: &str) -> Option<String> {
    for marker in LINE_MARKERS {
        if let Some((_, rest)) = text.split_once(marker) {
            let line = rest.lines().next().unwrap_or("").trim();
            if !line.is_empty() {
                return Some(line.to_string());
            }
        }
    }
    let (_, rest) = text.split_once("<cwd>")?;
    let (cwd, _) = rest.split_once("</cwd>")?;
    Some(cwd.trim().to_string()).filter(|s| !s.is_empty())
}

/// First [`cwd_in`] match over system, developer and user messages.
/// Assistant and tool messages are skipped: a tool's output can quote any marker.
pub fn find_cwd(gens: &[Generation]) -> Option<String> {
    gens.iter()
        .flat_map(|g| &g.messages)
        .filter(|m| m.role == "user" || is_system_like(&m.role))
        .find_map(|m| cwd_in(&content_text(&m.content)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::Message;
    use serde_json::json;

    #[test]
    fn claude_code_marker() {
        let t = "Here is useful information about the environment:\n - Primary working directory: /home/user/proj\n - Is a git repository: true\n";
        assert_eq!(cwd_in(t).as_deref(), Some("/home/user/proj"));
    }

    #[test]
    fn opencode_env_block() {
        let t = "<env>\n  Working directory: /home/user/proj\n  Workspace root folder: /home/user\n  Is directory a git repo: yes\n</env>";
        assert_eq!(cwd_in(t).as_deref(), Some("/home/user/proj"));
    }

    #[test]
    fn pi_line_and_section_forms() {
        let old = "You are an expert coding assistant.\nCurrent date and time: Monday\nCurrent working directory: /home/user/proj";
        assert_eq!(cwd_in(old).as_deref(), Some("/home/user/proj"));
        let new = "<preamble>\n…\n</preamble>\n<cwd>\n/home/user/proj\n</cwd>";
        assert_eq!(cwd_in(new).as_deref(), Some("/home/user/proj"));
    }

    #[test]
    fn codex_environment_context() {
        let t = "<environment_context>\n  <cwd>/home/user/proj</cwd>\n  <shell>zsh</shell>\n</environment_context>";
        assert_eq!(cwd_in(t).as_deref(), Some("/home/user/proj"));
    }

    #[test]
    fn no_marker_is_none() {
        assert_eq!(cwd_in("nothing here"), None);
        assert_eq!(cwd_in("Working directory:   \n"), None);
        assert_eq!(cwd_in("<cwd> </cwd>"), None);
    }

    #[test]
    fn tool_and_assistant_messages_are_not_scanned() {
        let msg = |role: &str, text: &str| Message {
            role: role.into(),
            content: json!(text),
            ..Default::default()
        };
        let g = Generation {
            messages: vec![
                msg("tool", "Working directory: /tmp/from-tool"),
                msg("assistant", "Working directory: /tmp/from-model"),
                msg(
                    "user",
                    "<environment_context><cwd>/home/user/proj</cwd></environment_context>",
                ),
            ]
            .into(),
            ..Default::default()
        };
        assert_eq!(find_cwd(&[g]).as_deref(), Some("/home/user/proj"));
    }
}
