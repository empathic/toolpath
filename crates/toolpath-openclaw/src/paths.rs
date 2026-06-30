//! On-disk layout resolution for OpenClaw's state directory.
//!
//! OpenClaw persists sessions under a single state directory (default
//! `~/.openclaw`) as `agents/<agentId>/sessions/<sessionId>.jsonl`, with a
//! `sessions.json` index mapping routing keys to files. See
//! `docs/agents/formats/openclaw/directory-layout.md`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::error::{OpenClawError, Result};

/// The default agent bucket when none is given.
pub const DEFAULT_AGENT_ID: &str = "main";

/// Normalize an agent id the way OpenClaw does: lowercase and path-sanitize.
pub fn normalize_agent_id(agent_id: &str) -> String {
    let trimmed = agent_id.trim();
    if trimmed.is_empty() {
        return DEFAULT_AGENT_ID.to_string();
    }
    trimmed
        .to_lowercase()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | ' ' => '-',
            other => other,
        })
        .collect()
}

/// Parsed components of an OpenClaw routing/session key
/// (`agent:<agentId>:<channel>:<peerKind>:<peerId>` and its variants).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedKey {
    /// The agent persona id.
    pub agent_id: String,
    /// Messaging channel (whatsapp/telegram/slack/…), if any.
    pub channel: Option<String>,
    /// Peer kind (direct/dm/group/channel), if any.
    pub peer_kind: Option<String>,
    /// Channel-native peer id (a person for DMs, a room for groups).
    pub peer_id: Option<String>,
    /// Thread id, when the key is thread-scoped.
    pub thread_id: Option<String>,
}

/// Parse an OpenClaw session key into its components. Lenient: unknown shapes
/// yield `None` parts rather than an error.
pub fn parse_session_key(key: &str) -> ParsedKey {
    let segs: Vec<&str> = key.split(':').collect();
    let mut out = ParsedKey::default();
    if segs.first() != Some(&"agent") || segs.len() < 2 {
        return out;
    }
    out.agent_id = segs[1].to_string();
    let mut rest: Vec<&str> = segs[2..].to_vec();

    if let Some(i) = rest.iter().position(|s| *s == "thread") {
        if i + 1 < rest.len() {
            out.thread_id = Some(rest[i + 1].to_string());
        }
        rest.truncate(i);
    }

    if rest.is_empty() || rest == ["main"] {
        return out;
    }
    if rest[0] == "direct" && rest.len() >= 2 {
        out.peer_kind = Some("direct".to_string());
        out.peer_id = Some(rest[1].to_string());
        return out;
    }
    out.channel = Some(rest[0].to_string());
    if let Some(i) = rest.iter().position(|s| *s == "direct") {
        out.peer_kind = Some("direct".to_string());
        if i + 1 < rest.len() {
            out.peer_id = Some(rest[i + 1].to_string());
        }
    } else if rest.len() >= 3 {
        out.peer_kind = Some(rest[1].to_string());
        out.peer_id = Some(rest[2].to_string());
    } else if rest.len() == 2 {
        out.peer_id = Some(rest[1].to_string());
    }
    out
}

/// One entry in `sessions.json` (keyed by routing key).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexEntry {
    /// The transcript session id (filename stem).
    #[serde(default)]
    pub session_id: Option<String>,
    /// The transcript filename (relative to the sessions dir, or absolute).
    #[serde(default)]
    pub session_file: Option<String>,
    /// Anything else OpenClaw stores per session.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Parsed `sessions.json` index: routing key -> entry.
#[derive(Debug, Clone, Default)]
pub struct SessionsIndex(pub BTreeMap<String, IndexEntry>);

impl SessionsIndex {
    /// Load `sessions.json` from a sessions directory, if present and parseable.
    pub fn load(sessions_dir: &Path) -> Option<SessionsIndex> {
        let path = sessions_dir.join("sessions.json");
        let bytes = std::fs::read(&path).ok()?;
        let map: BTreeMap<String, IndexEntry> = serde_json::from_slice(&bytes).ok()?;
        Some(SessionsIndex(map))
    }

    /// Find the routing key (and parse) for a given session id.
    pub fn routing_key_for(&self, session_id: &str) -> Option<(String, ParsedKey)> {
        self.0.iter().find_map(|(key, entry)| {
            if entry.session_id.as_deref() == Some(session_id) {
                Some((key.clone(), parse_session_key(key)))
            } else {
                None
            }
        })
    }

    /// The recorded session file for a session id, if the index names one.
    pub fn session_file_for(&self, session_id: &str) -> Option<String> {
        self.0.values().find_map(|e| {
            if e.session_id.as_deref() == Some(session_id) {
                e.session_file.clone()
            } else {
                None
            }
        })
    }
}

/// Resolves OpenClaw's on-disk layout. Construct with [`PathResolver::new`]
/// (reads the environment) or [`PathResolver::with_state_dir`] (explicit, used
/// in tests).
#[derive(Debug, Clone)]
pub struct PathResolver {
    state_dir: PathBuf,
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PathResolver {
    /// Resolve the state directory from the process environment.
    pub fn new() -> Self {
        let state_dir = resolve_state_dir(&|k| std::env::var(k).ok(), os_home().as_deref());
        Self { state_dir }
    }

    /// Use an explicit state directory (test seam / `--base` override).
    pub fn with_state_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: dir.into(),
        }
    }

    /// The resolved state directory (e.g. `~/.openclaw`).
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// `<state>/agents`.
    pub fn agents_dir(&self) -> PathBuf {
        self.state_dir.join("agents")
    }

    /// `<state>/agents/<agentId>/sessions`.
    pub fn agent_sessions_dir(&self, agent_id: &str) -> PathBuf {
        self.agents_dir()
            .join(normalize_agent_id(agent_id))
            .join("sessions")
    }

    /// List agent ids that have a `sessions/` directory on disk.
    pub fn list_agent_ids(&self) -> std::io::Result<Vec<String>> {
        let mut ids = Vec::new();
        let dir = self.agents_dir();
        if !dir.exists() {
            return Ok(ids);
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                ids.push(name.to_string());
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Resolve a session id to a concrete transcript file under an agent.
    ///
    /// Tries, in order: the `sessions.json` index (`sessionFile`/`sessionId`),
    /// then `<id>.jsonl`, then any `*.jsonl` whose stem is `<id>`, ends with
    /// `_<id>` (forked/rotated), or starts with `<id>-topic-`.
    pub fn resolve_session_file(&self, agent_id: &str, session_id: &str) -> Result<PathBuf> {
        let dir = self.agent_sessions_dir(agent_id);
        if !dir.exists() {
            return Err(OpenClawError::agent_not_found(agent_id));
        }

        if let Some(index) = SessionsIndex::load(&dir)
            && let Some(file) = index.session_file_for(session_id)
        {
            let p = PathBuf::from(&file);
            let candidate = if p.is_absolute() { p } else { dir.join(p) };
            if candidate.exists() {
                return Ok(candidate);
            }
        }

        let direct = dir.join(format!("{session_id}.jsonl"));
        if direct.exists() {
            return Ok(direct);
        }

        let forked_suffix = format!("_{session_id}");
        let topic_prefix = format!("{session_id}-topic-");
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if stem == session_id || stem.ends_with(&forked_suffix) || stem.starts_with(&topic_prefix)
            {
                return Ok(path);
            }
        }

        Err(OpenClawError::session_not_found(session_id))
    }
}

/// The OS home directory, honoring `OPENCLAW_HOME` then `HOME`/`USERPROFILE`.
fn os_home() -> Option<PathBuf> {
    resolve_home(&|k| std::env::var(k).ok())
}

fn resolve_home(get: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let pick = |v: Option<String>| v.filter(|s| !s.trim().is_empty()).map(PathBuf::from);
    pick(get("OPENCLAW_HOME"))
        .or_else(|| pick(get("HOME")))
        .or_else(|| pick(get("USERPROFILE")))
}

/// Resolve the state directory: `OPENCLAW_STATE_DIR` -> existing `~/.openclaw`
/// -> existing legacy `~/.clawdbot` -> default `~/.openclaw`.
fn resolve_state_dir(get: &dyn Fn(&str) -> Option<String>, os_home: Option<&Path>) -> PathBuf {
    if let Some(override_dir) = get("OPENCLAW_STATE_DIR").filter(|s| !s.trim().is_empty()) {
        return PathBuf::from(override_dir);
    }
    let home = resolve_home(get)
        .or_else(|| os_home.map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    let new_dir = home.join(".openclaw");
    if new_dir.exists() {
        return new_dir;
    }
    let legacy = home.join(".clawdbot");
    if legacy.exists() {
        return legacy;
    }
    new_dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_sessions_dir_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let r = PathResolver::with_state_dir(tmp.path());
        assert_eq!(r.state_dir(), tmp.path());
        assert_eq!(
            r.agent_sessions_dir("main"),
            tmp.path().join("agents/main/sessions")
        );
        assert_eq!(
            r.agent_sessions_dir("Work Bot"),
            tmp.path().join("agents/work-bot/sessions")
        );
    }

    #[test]
    fn state_dir_env_override_wins() {
        let get = |k: &str| match k {
            "OPENCLAW_STATE_DIR" => Some("/custom/state".to_string()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(
            resolve_state_dir(&get, None),
            PathBuf::from("/custom/state")
        );
    }

    #[test]
    fn state_dir_defaults_to_openclaw_under_home() {
        let get = |k: &str| match k {
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        // Neither ~/.openclaw nor ~/.clawdbot exists under this fake home.
        assert_eq!(
            resolve_state_dir(&get, None),
            PathBuf::from("/home/u/.openclaw")
        );
    }

    #[test]
    fn home_prefers_openclaw_home() {
        let get = |k: &str| match k {
            "OPENCLAW_HOME" => Some("/oc/home".to_string()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(resolve_home(&get), Some(PathBuf::from("/oc/home")));
    }

    #[test]
    fn parses_dm_key() {
        let dm = parse_session_key("agent:main:whatsapp:direct:15555550123");
        assert_eq!(dm.agent_id, "main");
        assert_eq!(dm.channel.as_deref(), Some("whatsapp"));
        assert_eq!(dm.peer_kind.as_deref(), Some("direct"));
        assert_eq!(dm.peer_id.as_deref(), Some("15555550123"));
    }

    #[test]
    fn parses_group_key() {
        let grp = parse_session_key("agent:main:slack:group:T42");
        assert_eq!(grp.channel.as_deref(), Some("slack"));
        assert_eq!(grp.peer_kind.as_deref(), Some("group"));
        assert_eq!(grp.peer_id.as_deref(), Some("T42"));
    }

    #[test]
    fn parses_account_scoped_dm_and_thread() {
        let k = parse_session_key("agent:main:telegram:acct1:direct:99:thread:7");
        assert_eq!(k.channel.as_deref(), Some("telegram"));
        assert_eq!(k.peer_kind.as_deref(), Some("direct"));
        assert_eq!(k.peer_id.as_deref(), Some("99"));
        assert_eq!(k.thread_id.as_deref(), Some("7"));
    }

    #[test]
    fn parses_main_key() {
        let main = parse_session_key("agent:main:main");
        assert_eq!(main.agent_id, "main");
        assert!(main.channel.is_none());
        assert!(main.peer_id.is_none());
    }

    #[test]
    fn resolves_session_file_by_stem_and_index() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents/main/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("abc-123.jsonl"), "{}").unwrap();
        std::fs::write(
            dir.join("sessions.json"),
            r#"{"agent:main:whatsapp:direct:55":{"sessionId":"abc-123","sessionFile":"abc-123.jsonl"}}"#,
        )
        .unwrap();
        let r = PathResolver::with_state_dir(tmp.path());

        // by index
        let by_index = r.resolve_session_file("main", "abc-123").unwrap();
        assert_eq!(by_index, dir.join("abc-123.jsonl"));

        // routing key recovered
        let idx = SessionsIndex::load(&dir).unwrap();
        let (key, parsed) = idx.routing_key_for("abc-123").unwrap();
        assert!(key.contains("whatsapp"));
        assert_eq!(parsed.channel.as_deref(), Some("whatsapp"));

        // by stem (different file, no index entry)
        std::fs::write(dir.join("loose-9.jsonl"), "{}").unwrap();
        assert_eq!(
            r.resolve_session_file("main", "loose-9").unwrap(),
            dir.join("loose-9.jsonl")
        );

        // missing → error
        assert!(r.resolve_session_file("main", "nope").is_err());
    }
}
