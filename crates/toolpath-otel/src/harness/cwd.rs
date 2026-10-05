//! Working directory from a harness's environment block.

use crate::generation::{Generation, Message};
use crate::harness::SourceHarness;
use crate::normalize::{content_text, is_system_like};

const CLAUDE_CODE: &str = "Primary working directory:";
/// opencode `<env>` (sst/opencode session/system.ts).
const OPENCODE: &str = "Working directory:";
/// pi before its XML prompt sections (pi-mono coding-agent system-prompt.ts).
const PI: &str = "Current working directory:";

/// The rest of `marker`'s line, if non-empty.
fn after_line_marker(text: &str, marker: &str) -> Option<String> {
    let (_, rest) = text.split_once(marker)?;
    let line = rest.lines().next().unwrap_or("").trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// `<cwd>…</cwd>` (Codex `<environment_context>`, newer pi).
fn cwd_tag(text: &str) -> Option<String> {
    let (_, rest) = text.split_once("<cwd>")?;
    let (cwd, _) = rest.split_once("</cwd>")?;
    Some(cwd.trim().to_string()).filter(|s| !s.is_empty())
}

fn claude_code(text: &str) -> Option<String> {
    after_line_marker(text, CLAUDE_CODE)
}

fn opencode(text: &str) -> Option<String> {
    let (_, rest) = text.split_once("<env>")?;
    let env = rest.split_once("</env>").map_or(rest, |(env, _)| env);
    after_line_marker(env, OPENCODE)
}

fn pi(text: &str) -> Option<String> {
    after_line_marker(text, PI).or_else(|| cwd_tag(text))
}

/// The working directory named in `text` by any harness's marker.
pub fn cwd_in(text: &str) -> Option<String> {
    [CLAUDE_CODE, OPENCODE, PI]
        .into_iter()
        .find_map(|marker| after_line_marker(text, marker))
        .or_else(|| cwd_tag(text))
}

fn is_user(m: &Message) -> bool {
    m.role == "user"
}

fn is_system(m: &Message) -> bool {
    is_system_like(&m.role)
}

fn is_user_or_system(m: &Message) -> bool {
    is_user(m) || is_system(m)
}

type Scanned = fn(&Message) -> bool;
type Reader = fn(&str) -> Option<String>;

fn first(gens: &[Generation], scanned: Scanned, read: Reader) -> Option<String> {
    gens.iter()
        .flat_map(|g| &g.messages)
        .filter(|m| scanned(m))
        .find_map(|m| read(&content_text(&m.content)))
}

/// A known harness: the first match of its own marker in the messages it
/// puts its environment in, else the first [`cwd_in`] match over system and
/// developer messages. An unknown harness: the first [`cwd_in`] match over
/// system, developer and user messages. Assistant and tool messages are
/// never scanned: a tool's output can quote any marker.
pub fn find_cwd(gens: &[Generation], harness: SourceHarness) -> Option<String> {
    let (scanned, read): (Scanned, Reader) = match harness {
        SourceHarness::ClaudeCode => (is_system, claude_code),
        SourceHarness::Codex => (is_user, cwd_tag),
        SourceHarness::Opencode => (is_system, opencode),
        SourceHarness::Pi => (is_system, pi),
        SourceHarness::Unknown => return first(gens, is_user_or_system, cwd_in),
    };
    first(gens, scanned, read).or_else(|| first(gens, is_system, cwd_in))
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

    fn msg(role: &str, text: &str) -> Message {
        Message {
            role: role.into(),
            content: json!(text),
            ..Default::default()
        }
    }

    fn of(messages: Vec<Message>) -> [Generation; 1] {
        [Generation {
            messages,
            ..Default::default()
        }]
    }

    #[test]
    fn tool_and_assistant_messages_are_not_scanned() {
        let g = of(vec![
            msg("tool", "Working directory: /tmp/from-tool"),
            msg("assistant", "Working directory: /tmp/from-model"),
            msg(
                "user",
                "<environment_context><cwd>/home/user/proj</cwd></environment_context>",
            ),
        ]);
        assert_eq!(
            find_cwd(&g, SourceHarness::Unknown).as_deref(),
            Some("/home/user/proj")
        );
    }

    #[test]
    fn codex_reads_only_its_cwd_tag() {
        let g = of(vec![
            msg("developer", "<permissions instructions>…"),
            msg(
                "user",
                "# AGENTS.md instructions\nWorking directory: /wrong\nPrimary working directory: /wrong",
            ),
            msg(
                "user",
                "<environment_context>\n  <cwd>/home/user/proj</cwd>\n</environment_context>",
            ),
        ]);
        assert_eq!(
            find_cwd(&g, SourceHarness::Codex).as_deref(),
            Some("/home/user/proj")
        );
        assert_eq!(
            find_cwd(&g, SourceHarness::Unknown).as_deref(),
            Some("/wrong"),
            "an unknown harness takes the first marker of any harness"
        );
    }

    #[test]
    fn each_harness_reads_its_own_marker_in_its_own_role() {
        let g = of(vec![
            msg("user", "Primary working directory: /typed"),
            msg(
                "system",
                "Working directory: /outside-env\nPrimary working directory: /cc\n<env>\n  Working directory: /oc\n</env>\nCurrent working directory: /pi",
            ),
        ]);
        for (harness, want) in [
            (SourceHarness::ClaudeCode, "/cc"),
            (SourceHarness::Opencode, "/oc"),
            (SourceHarness::Pi, "/pi"),
        ] {
            assert_eq!(find_cwd(&g, harness).as_deref(), Some(want), "{harness:?}");
        }
        assert_eq!(
            find_cwd(&g, SourceHarness::Codex).as_deref(),
            Some("/cc"),
            "no <cwd>: any marker, but only in a system message"
        );
        let typed_only = of(vec![msg("user", "Primary working directory: /typed")]);
        for harness in [
            SourceHarness::ClaudeCode,
            SourceHarness::Codex,
            SourceHarness::Opencode,
            SourceHarness::Pi,
        ] {
            assert_eq!(find_cwd(&typed_only, harness), None, "{harness:?}");
        }
        let pi_tag = of(vec![msg("system", "<cwd>\n/home/user/proj\n</cwd>")]);
        assert_eq!(
            find_cwd(&pi_tag, SourceHarness::Pi).as_deref(),
            Some("/home/user/proj")
        );
    }
}
