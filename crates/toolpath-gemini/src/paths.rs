//! Filesystem layout for Gemini CLI conversation logs.
//!
//! Gemini CLI stores per-project chat logs under `~/.gemini/tmp/<slot>/`,
//! where `<slot>` is either the friendly project name from
//! `~/.gemini/projects.json` or the SHA-256 hex of the absolute project
//! path. Both are supported: the resolver prefers the friendly name when
//! it exists on disk, and falls back to the hash otherwise.

use crate::error::{ConvoError, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const PROJECTS_FILE: &str = "projects.json";
const TMP_DIR: &str = "tmp";
const CHATS_SUBDIR: &str = "chats";
const LOGS_FILE: &str = "logs.json";

/// One session surfaced by [`PathResolver::list_session_entries`].
#[derive(Debug, Clone)]
pub struct SessionEntry {
    /// Listing key, exactly as [`PathResolver::list_sessions`] returns
    /// it: main-file stem or orphan sub-agent directory name.
    pub id: String,
    /// Inner `sessionId` UUID peeked from a main file (the directory
    /// name itself for orphan dirs); `None` when the peek failed.
    pub session_uuid: Option<String>,
    /// The main chat file, or the orphan sub-agent directory — stat
    /// this for change detection.
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PathResolver {
    home_dir: Option<PathBuf>,
    gemini_dir: Option<PathBuf>,
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PathResolver {
    pub fn new() -> Self {
        Self {
            home_dir: dirs::home_dir(),
            gemini_dir: None,
        }
    }

    pub fn with_home<P: Into<PathBuf>>(mut self, home: P) -> Self {
        self.home_dir = Some(home.into());
        self
    }

    pub fn with_gemini_dir<P: Into<PathBuf>>(mut self, gemini_dir: P) -> Self {
        self.gemini_dir = Some(gemini_dir.into());
        self
    }

    pub fn home_dir(&self) -> Result<&Path> {
        self.home_dir.as_deref().ok_or(ConvoError::NoHomeDirectory)
    }

    pub fn gemini_dir(&self) -> Result<PathBuf> {
        if let Some(d) = &self.gemini_dir {
            return Ok(d.clone());
        }
        Ok(self.home_dir()?.join(".gemini"))
    }

    pub fn projects_file(&self) -> Result<PathBuf> {
        Ok(self.gemini_dir()?.join(PROJECTS_FILE))
    }

    pub fn tmp_dir(&self) -> Result<PathBuf> {
        Ok(self.gemini_dir()?.join(TMP_DIR))
    }

    /// Absolute path to the project slot directory under `tmp/`.
    ///
    /// Looks up `project_path` in `projects.json` for its friendly name
    /// first; if that directory doesn't exist, falls back to
    /// `tmp/<sha256(project_path)>/`. The returned path may not exist
    /// yet — callers decide how to handle that.
    pub fn project_dir(&self, project_path: &str) -> Result<PathBuf> {
        let tmp = self.tmp_dir()?;

        if let Some(friendly) = self.friendly_name_for(project_path)? {
            let candidate = tmp.join(&friendly);
            if candidate.exists() {
                return Ok(candidate);
            }
        }

        // Fall back to the SHA-256 slot.
        let hashed = project_hash(project_path);
        let candidate = tmp.join(&hashed);
        if candidate.exists() {
            return Ok(candidate);
        }

        // If neither exists, try the friendly name anyway (the caller
        // may intend to create the directory) — otherwise return the
        // hash path as a stable default.
        if let Some(friendly) = self.friendly_name_for(project_path)? {
            return Ok(tmp.join(friendly));
        }
        Ok(candidate)
    }

    pub fn chats_dir(&self, project_path: &str) -> Result<PathBuf> {
        Ok(self.project_dir(project_path)?.join(CHATS_SUBDIR))
    }

    pub fn session_dir(&self, project_path: &str, session_uuid: &str) -> Result<PathBuf> {
        Ok(self.chats_dir(project_path)?.join(session_uuid))
    }

    pub fn chat_file(
        &self,
        project_path: &str,
        session_uuid: &str,
        chat_name: &str,
    ) -> Result<PathBuf> {
        let stem = if chat_name.ends_with(".json") {
            chat_name.to_string()
        } else {
            format!("{}.json", chat_name)
        };
        Ok(self.session_dir(project_path, session_uuid)?.join(stem))
    }

    pub fn logs_file(&self, project_path: &str) -> Result<PathBuf> {
        Ok(self.project_dir(project_path)?.join(LOGS_FILE))
    }

    /// Read `projects.json` and reverse-lookup a friendly name for the
    /// given absolute project path.
    pub fn friendly_name_for(&self, project_path: &str) -> Result<Option<String>> {
        let file = match self.projects_file() {
            Ok(p) if p.exists() => p,
            _ => return Ok(None),
        };
        let bytes = fs::read(&file)?;
        let projects: ProjectsFile = match serde_json::from_slice(&bytes) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        Ok(projects.projects.get(project_path).cloned())
    }

    /// Return every project path known to Gemini: the union of
    /// `projects.json` keys and any project slots present under `tmp/`
    /// that have a `.project_root` marker.
    pub fn list_project_dirs(&self) -> Result<Vec<String>> {
        let mut paths: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // projects.json entries.
        if let Ok(file) = self.projects_file()
            && file.exists()
            && let Ok(bytes) = fs::read(&file)
            && let Ok(projects) = serde_json::from_slice::<ProjectsFile>(&bytes)
        {
            for key in projects.projects.keys() {
                if seen.insert(key.clone()) {
                    paths.push(key.clone());
                }
            }
        }

        // `.project_root` markers under tmp/.
        if let Ok(tmp) = self.tmp_dir()
            && tmp.exists()
        {
            for entry in fs::read_dir(&tmp)?.flatten() {
                if entry.file_type().ok().is_some_and(|ft| ft.is_dir()) {
                    let marker = entry.path().join(".project_root");
                    if marker.exists()
                        && let Ok(text) = fs::read_to_string(&marker)
                    {
                        let p = text.trim().to_string();
                        if !p.is_empty() && seen.insert(p.clone()) {
                            paths.push(p);
                        }
                    }
                }
            }
        }

        paths.sort();
        Ok(paths)
    }

    /// List sessions under a project's `chats/` directory.
    ///
    /// A session is either a top-level `session-*.json` main-chat file
    /// (listed by its file stem) or an orphan `<uuid>/` directory that
    /// has no corresponding main file (listed by the dir name).
    ///
    /// When both a `session-*.json` *and* a `<uuid>/` dir point at the
    /// same `sessionId`, the UUID dir is considered the main file's
    /// sub-agent bucket and is **not** surfaced as a separate session —
    /// it gets merged into the main session by `read_session`.
    pub fn list_sessions(&self, project_path: &str) -> Result<Vec<String>> {
        Ok(self
            .list_session_entries(project_path)?
            .into_iter()
            .map(|e| e.id)
            .collect())
    }

    /// Like [`Self::list_sessions`], but each session comes with the
    /// backing main file (or orphan sub-agent directory) and the inner
    /// `sessionId` when one could be peeked — enough for stat-level
    /// change detection without parsing chat bodies. The peek is
    /// bounded; see [`peek_session_id`].
    pub fn list_session_entries(&self, project_path: &str) -> Result<Vec<SessionEntry>> {
        let chats = match self.chats_dir(project_path) {
            Ok(p) => p,
            Err(_) => return Ok(Vec::new()),
        };
        if !chats.exists() {
            return Ok(Vec::new());
        }

        let mut mains: Vec<SessionEntry> = Vec::new();
        let mut main_session_uuids: std::collections::HashSet<String> = Default::default();
        let mut dirs: Vec<SessionEntry> = Vec::new();

        for entry in fs::read_dir(&chats)?.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            let path = entry.path();
            if ft.is_file() {
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let stem = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let session_uuid = peek_session_id(&path);
                if let Some(uuid) = &session_uuid {
                    main_session_uuids.insert(uuid.clone());
                }
                mains.push(SessionEntry {
                    id: stem,
                    session_uuid,
                    path,
                });
            } else if ft.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                dirs.push(SessionEntry {
                    id: name.to_string(),
                    session_uuid: Some(name.to_string()),
                    path,
                });
            }
        }

        let mut out = mains;
        for dir in dirs {
            if !main_session_uuids.contains(&dir.id) {
                out.push(dir);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// List just the top-level main session file stems (no UUID dirs).
    pub fn list_main_session_stems(&self, project_path: &str) -> Result<Vec<String>> {
        let chats = match self.chats_dir(project_path) {
            Ok(p) => p,
            Err(_) => return Ok(Vec::new()),
        };
        if !chats.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&chats)?.flatten() {
            let path = entry.path();
            if path.is_file()
                && path.extension().and_then(|s| s.to_str()) == Some("json")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                out.push(stem.to_string());
            }
        }
        out.sort();
        Ok(out)
    }

    /// Path to a main session JSON at the top of `chats/`.
    pub fn main_session_file(&self, project_path: &str, stem: &str) -> Result<PathBuf> {
        let name = if stem.ends_with(".json") {
            stem.to_string()
        } else {
            format!("{}.json", stem)
        };
        Ok(self.chats_dir(project_path)?.join(name))
    }

    /// Locate a main chat file whose *identity* (either the filename stem
    /// or the inner `sessionId` field) matches `session_id`.
    ///
    /// This mirrors how Gemini CLI itself resolves `--resume <id>`: it
    /// accepts both the on-disk stem (e.g. `session-2026-04-17T18-09-b26d7f99`)
    /// and the full session UUID (which lives inside the file as
    /// `"sessionId"`). Returns `Ok(None)` if nothing matches.
    ///
    /// Does *not* consider UUID subdirectories — those are handled
    /// separately in [`crate::ConvoIO::read_session`] as an orphan
    /// sub-agent bucket.
    pub fn resolve_main_file(
        &self,
        project_path: &str,
        session_id: &str,
    ) -> Result<Option<PathBuf>> {
        // Fast path: direct stem match at chats/<session_id>.json.
        let direct = self.main_session_file(project_path, session_id)?;
        if direct.exists() {
            return Ok(Some(direct));
        }

        // Fallback: scan chats/*.json and match on inner sessionId.
        let chats = match self.chats_dir(project_path) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        if !chats.exists() {
            return Ok(None);
        }
        for entry in fs::read_dir(&chats)?.flatten() {
            let p = entry.path();
            if !p.is_file() || p.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Some(inner) = peek_session_id(&p)
                && inner == session_id
            {
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    /// List chat file stems in a session directory (without `.json`).
    pub fn list_chat_files(&self, project_path: &str, session_uuid: &str) -> Result<Vec<String>> {
        let dir = match self.session_dir(project_path, session_uuid) {
            Ok(p) => p,
            Err(_) => return Ok(Vec::new()),
        };
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut stems: Vec<String> = Vec::new();
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                stems.push(stem.to_string());
            }
        }
        stems.sort();
        Ok(stems)
    }

    pub fn exists(&self) -> bool {
        self.gemini_dir().map(|p| p.exists()).unwrap_or(false)
    }
}

#[derive(Debug, Deserialize)]
struct ProjectsFile {
    #[serde(default)]
    projects: HashMap<String, String>,
}

/// Byte budget for [`peek_session_id`]'s prefix read. Chat files put
/// their identity fields first, so this is plenty in practice.
const PEEK_BYTES: usize = 4096;

/// Read just the top-level `sessionId` field from a chat JSON file.
/// Bounded: scans the first [`PEEK_BYTES`] of the file and falls back
/// to a full parse only when the field isn't in the prefix. Used by
/// `list_session_entries` to correlate main files with sibling
/// sub-agent UUID directories.
fn peek_session_id(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let file = fs::File::open(path).ok()?;
    let mut prefix = Vec::with_capacity(PEEK_BYTES);
    file.take(PEEK_BYTES as u64).read_to_end(&mut prefix).ok()?;
    let whole_file = prefix.len() < PEEK_BYTES;
    if let Some(id) = prefix_session_id(&prefix) {
        return Some(id);
    }
    if whole_file {
        return None;
    }
    #[derive(Deserialize)]
    struct Peek {
        #[serde(rename = "sessionId")]
        session_id: Option<String>,
    }
    let bytes = fs::read(path).ok()?;
    let peek: Peek = serde_json::from_slice(&bytes).ok()?;
    peek.session_id.filter(|s| !s.is_empty())
}

/// Extract `"sessionId": "…"` from a JSON prefix, trusting it only when
/// it appears before any `"messages"` key — message bodies are the one
/// place user-controlled text could fake the key.
fn prefix_session_id(prefix: &[u8]) -> Option<String> {
    let text = match std::str::from_utf8(prefix) {
        Ok(t) => t,
        // The cut can land mid-codepoint; scan the valid part.
        Err(e) => std::str::from_utf8(&prefix[..e.valid_up_to()]).ok()?,
    };
    let key_at = text.find("\"sessionId\"")?;
    if let Some(messages_at) = text.find("\"messages\"")
        && messages_at < key_at
    {
        return None;
    }
    let rest = text[key_at + "\"sessionId\"".len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let value = &rest[..rest.find('"')?];
    if value.is_empty() || value.contains('\\') {
        return None;
    }
    Some(value.to_string())
}

/// Canonical `projectHash`: SHA-256 hex of the absolute project path.
pub fn project_hash(project_path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project_path.as_bytes());
    let digest = hasher.finalize();
    let mut s = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", byte);
    }
    s
}

mod dirs {
    use std::env;
    use std::path::PathBuf;

    pub fn home_dir() -> Option<PathBuf> {
        env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathResolver) {
        let temp = TempDir::new().unwrap();
        let gemini = temp.path().join(".gemini");
        fs::create_dir_all(&gemini).unwrap();
        let resolver = PathResolver::new()
            .with_home(temp.path())
            .with_gemini_dir(&gemini);
        (temp, resolver)
    }

    #[test]
    fn test_project_hash_stable() {
        let h1 = project_hash("/Users/ben/empathic/oss/toolpath");
        let h2 = project_hash("/Users/ben/empathic/oss/toolpath");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_project_hash_matches_known_value() {
        // Value observed in real local chat file for this project
        let h = project_hash("/Users/ben/empathic/oss/toolpath");
        assert_eq!(
            h,
            "384e9530e99733805bc2c98a596ab23e67d4c29a6ef263cdc1c89b3bcd022c69"
        );
    }

    #[test]
    fn test_gemini_dir_default() {
        let (temp, resolver) = setup();
        let dir = resolver.gemini_dir().unwrap();
        assert_eq!(dir, temp.path().join(".gemini"));
    }

    #[test]
    fn test_gemini_dir_from_home() {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new().with_home(temp.path());
        assert_eq!(resolver.gemini_dir().unwrap(), temp.path().join(".gemini"));
    }

    #[test]
    fn test_project_dir_friendly_name() {
        let (_temp, resolver) = setup();
        let gemini = resolver.gemini_dir().unwrap();
        fs::write(
            gemini.join("projects.json"),
            r#"{"projects":{"/abs/myrepo":"myrepo"}}"#,
        )
        .unwrap();
        fs::create_dir_all(gemini.join("tmp/myrepo")).unwrap();

        let dir = resolver.project_dir("/abs/myrepo").unwrap();
        assert_eq!(dir, gemini.join("tmp/myrepo"));
    }

    #[test]
    fn test_project_dir_hash_fallback() {
        let (_temp, resolver) = setup();
        let gemini = resolver.gemini_dir().unwrap();
        let hashed = project_hash("/abs/other");
        fs::create_dir_all(gemini.join("tmp").join(&hashed)).unwrap();

        let dir = resolver.project_dir("/abs/other").unwrap();
        assert_eq!(dir, gemini.join("tmp").join(hashed));
    }

    #[test]
    fn test_project_dir_no_dir_returns_hash_path() {
        let (_temp, resolver) = setup();
        let gemini = resolver.gemini_dir().unwrap();
        let dir = resolver.project_dir("/never/exists").unwrap();
        assert_eq!(dir, gemini.join("tmp").join(project_hash("/never/exists")));
    }

    #[test]
    fn test_project_dir_prefers_friendly_name_even_without_tmp() {
        let (_temp, resolver) = setup();
        let gemini = resolver.gemini_dir().unwrap();
        // Friendly name is present in projects.json, but tmp/<friendly>/
        // doesn't exist. When no slot exists, we still prefer the friendly
        // path so callers targeting the known name work.
        fs::write(
            gemini.join("projects.json"),
            r#"{"projects":{"/abs/myrepo":"myrepo"}}"#,
        )
        .unwrap();
        let dir = resolver.project_dir("/abs/myrepo").unwrap();
        assert_eq!(dir, gemini.join("tmp/myrepo"));
    }

    #[test]
    fn test_session_dir_chat_file() {
        let (_temp, resolver) = setup();
        let gemini = resolver.gemini_dir().unwrap();
        fs::create_dir_all(gemini.join("tmp/myrepo/chats/session-uuid")).unwrap();
        fs::write(
            gemini.join("projects.json"),
            r#"{"projects":{"/abs/myrepo":"myrepo"}}"#,
        )
        .unwrap();

        let session = resolver.session_dir("/abs/myrepo", "session-uuid").unwrap();
        assert_eq!(session, gemini.join("tmp/myrepo/chats/session-uuid"));

        let file = resolver
            .chat_file("/abs/myrepo", "session-uuid", "main")
            .unwrap();
        assert_eq!(file, gemini.join("tmp/myrepo/chats/session-uuid/main.json"));

        let file_with_ext = resolver
            .chat_file("/abs/myrepo", "session-uuid", "main.json")
            .unwrap();
        assert_eq!(file, file_with_ext);
    }

    #[test]
    fn test_logs_file() {
        let (_temp, resolver) = setup();
        let gemini = resolver.gemini_dir().unwrap();
        let logs = resolver.logs_file("/abs/myrepo").unwrap();
        assert!(logs.ends_with("logs.json"));
        // Should live inside the project slot
        assert!(logs.starts_with(gemini.join("tmp")));
    }

    #[test]
    fn test_friendly_name_lookup_missing_file() {
        let (_temp, resolver) = setup();
        assert_eq!(resolver.friendly_name_for("/nope").unwrap(), None);
    }

    #[test]
    fn test_friendly_name_lookup_malformed_file() {
        let (_temp, resolver) = setup();
        let gemini = resolver.gemini_dir().unwrap();
        fs::write(gemini.join("projects.json"), "not json").unwrap();
        assert_eq!(resolver.friendly_name_for("/nope").unwrap(), None);
    }

    #[test]
    fn test_list_project_dirs_union() {
        let (_temp, resolver) = setup();
        let gemini = resolver.gemini_dir().unwrap();

        fs::write(
            gemini.join("projects.json"),
            r#"{"projects":{"/a":"a","/b":"b"}}"#,
        )
        .unwrap();

        // Add a C slot that only has a .project_root marker
        fs::create_dir_all(gemini.join("tmp/c")).unwrap();
        fs::write(gemini.join("tmp/c/.project_root"), "/c\n").unwrap();

        let projects = resolver.list_project_dirs().unwrap();
        assert!(projects.contains(&"/a".to_string()));
        assert!(projects.contains(&"/b".to_string()));
        assert!(projects.contains(&"/c".to_string()));
        assert_eq!(projects.len(), 3);
    }

    #[test]
    fn test_list_project_dirs_empty() {
        let (_temp, resolver) = setup();
        let projects = resolver.list_project_dirs().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn test_list_sessions() {
        let (_temp, resolver) = setup();
        let gemini = resolver.gemini_dir().unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        fs::create_dir_all(gemini.join("tmp/p/chats/session-a")).unwrap();
        fs::create_dir_all(gemini.join("tmp/p/chats/session-b")).unwrap();
        // A stray file should be ignored
        fs::write(gemini.join("tmp/p/chats/stray.txt"), "x").unwrap();

        let sessions = resolver.list_sessions("/p").unwrap();
        assert_eq!(
            sessions,
            vec!["session-a".to_string(), "session-b".to_string()]
        );
    }

    #[test]
    fn test_list_sessions_no_project() {
        let (_temp, resolver) = setup();
        let sessions = resolver.list_sessions("/never").unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_list_chat_files() {
        let (_temp, resolver) = setup();
        let gemini = resolver.gemini_dir().unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        fs::create_dir_all(gemini.join("tmp/p/chats/session-x")).unwrap();
        fs::write(gemini.join("tmp/p/chats/session-x/main.json"), "{}").unwrap();
        fs::write(gemini.join("tmp/p/chats/session-x/qclszz.json"), "{}").unwrap();
        fs::write(gemini.join("tmp/p/chats/session-x/ignore.txt"), "x").unwrap();

        let stems = resolver.list_chat_files("/p", "session-x").unwrap();
        assert_eq!(stems, vec!["main".to_string(), "qclszz".to_string()]);
    }

    #[test]
    fn test_exists() {
        let (_temp, resolver) = setup();
        assert!(resolver.exists());

        let missing = PathResolver::new().with_gemini_dir("/never/exists");
        assert!(!missing.exists());
    }

    #[test]
    fn test_home_dir_from_env() {
        let home = dirs::home_dir();
        // Most test environments have one of HOME/USERPROFILE set
        assert!(home.is_some());
    }

    #[test]
    fn test_tmp_dir() {
        let (_t, r) = setup();
        let tmp = r.tmp_dir().unwrap();
        assert!(tmp.ends_with(".gemini/tmp"));
    }

    #[test]
    fn test_chats_dir() {
        let (_t, r) = setup();
        let gemini = r.gemini_dir().unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        let chats = r.chats_dir("/p").unwrap();
        assert_eq!(chats, gemini.join("tmp/p/chats"));
    }

    #[test]
    fn test_list_main_session_stems() {
        // Flat main files at the top of `chats/` are enumerated; UUID
        // subdirectories are not.
        let (_t, r) = setup();
        let gemini = r.gemini_dir().unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        let chats = gemini.join("tmp/p/chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(
            chats.join("session-2026-04-17-abc.json"),
            r#"{"sessionId":"abc","projectHash":"","messages":[]}"#,
        )
        .unwrap();
        fs::write(
            chats.join("session-2026-04-18-def.json"),
            r#"{"sessionId":"def","projectHash":"","messages":[]}"#,
        )
        .unwrap();
        // UUID dir next to the main files — ignored by this listing
        fs::create_dir_all(chats.join("abc-1234-5678-9abc")).unwrap();

        let stems = r.list_main_session_stems("/p").unwrap();
        assert_eq!(
            stems,
            vec![
                "session-2026-04-17-abc".to_string(),
                "session-2026-04-18-def".to_string(),
            ]
        );
    }

    #[test]
    fn test_main_session_file_path() {
        let (_t, r) = setup();
        let gemini = r.gemini_dir().unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        let p = r.main_session_file("/p", "session-2026-04-17-abc").unwrap();
        assert_eq!(p, gemini.join("tmp/p/chats/session-2026-04-17-abc.json"));
        // .json suffix is optional
        let p2 = r
            .main_session_file("/p", "session-2026-04-17-abc.json")
            .unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn test_resolve_main_file_by_stem() {
        let (_t, r) = setup();
        let gemini = r.gemini_dir().unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        let chats = gemini.join("tmp/p/chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(
            chats.join("session-2026-04-17-abc.json"),
            r#"{"sessionId":"abc-uuid","projectHash":"","messages":[]}"#,
        )
        .unwrap();

        let found = r.resolve_main_file("/p", "session-2026-04-17-abc").unwrap();
        assert_eq!(found, Some(chats.join("session-2026-04-17-abc.json")));
    }

    #[test]
    fn test_resolve_main_file_by_inner_session_id() {
        // Matches the way Gemini CLI's `--resume <uuid>` resolves: scans
        // all main files and matches on inner `sessionId`.
        let (_t, r) = setup();
        let gemini = r.gemini_dir().unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        let chats = gemini.join("tmp/p/chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(
            chats.join("session-2026-04-17-abc.json"),
            r#"{"sessionId":"f7cc36c0-980c-4914-ae79-439567272478","projectHash":"","messages":[]}"#,
        )
        .unwrap();

        // `--resume f7cc36c0-...` should resolve to the file above even
        // though its on-disk stem is different.
        let found = r
            .resolve_main_file("/p", "f7cc36c0-980c-4914-ae79-439567272478")
            .unwrap();
        assert_eq!(found, Some(chats.join("session-2026-04-17-abc.json")));
    }

    #[test]
    fn test_resolve_main_file_prefers_stem_over_inner_id() {
        // If a file's stem *and* another file's inner sessionId both
        // match, the direct stem lookup wins — it's the fast path and
        // mirrors CLI lookup order.
        let (_t, r) = setup();
        let gemini = r.gemini_dir().unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        let chats = gemini.join("tmp/p/chats");
        fs::create_dir_all(&chats).unwrap();
        // File whose stem matches the query
        fs::write(
            chats.join("my-id.json"),
            r#"{"sessionId":"other-uuid","projectHash":"","messages":[]}"#,
        )
        .unwrap();
        // File whose inner sessionId matches the query
        fs::write(
            chats.join("session-other.json"),
            r#"{"sessionId":"my-id","projectHash":"","messages":[]}"#,
        )
        .unwrap();

        let found = r.resolve_main_file("/p", "my-id").unwrap();
        assert_eq!(found, Some(chats.join("my-id.json")));
    }

    #[test]
    fn test_resolve_main_file_returns_none_when_unmatched() {
        let (_t, r) = setup();
        let gemini = r.gemini_dir().unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        let chats = gemini.join("tmp/p/chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(
            chats.join("session-other.json"),
            r#"{"sessionId":"uuid-a","projectHash":"","messages":[]}"#,
        )
        .unwrap();

        let found = r.resolve_main_file("/p", "uuid-that-doesnt-exist").unwrap();
        assert_eq!(found, None);
    }

    #[test]
    fn test_list_sessions_dedupes_main_and_sibling_uuid() {
        // A main file whose inner sessionId matches a sibling UUID dir
        // should surface once as the main stem, not twice.
        let (_t, r) = setup();
        let gemini = r.gemini_dir().unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        let chats = gemini.join("tmp/p/chats");
        fs::create_dir_all(&chats).unwrap();
        // Main file carrying sessionId "sess-uuid-full"
        fs::write(
            chats.join("session-2026-abc.json"),
            r#"{"sessionId":"sess-uuid-full","projectHash":"","messages":[]}"#,
        )
        .unwrap();
        // Sibling sub-agent dir matching that UUID — should NOT be listed
        // as its own session.
        fs::create_dir_all(chats.join("sess-uuid-full")).unwrap();
        // An orphan UUID dir that does NOT correspond to any main — should
        // be listed.
        fs::create_dir_all(chats.join("orphan-uuid-zzz")).unwrap();

        let sessions = r.list_sessions("/p").unwrap();
        assert!(sessions.contains(&"session-2026-abc".to_string()));
        assert!(sessions.contains(&"orphan-uuid-zzz".to_string()));
        assert!(!sessions.contains(&"sess-uuid-full".to_string()));
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn peek_session_id_reads_id_from_prefix_of_large_file() {
        let (_temp, resolver) = setup();
        let chats = resolver.chats_dir("/proj").unwrap();
        fs::create_dir_all(&chats).unwrap();
        let pad = "x".repeat(16 * 1024);
        let body = format!(
            r#"{{"sessionId":"aaaa-bbbb","projectHash":"h","messages":[{{"content":"{pad}"}}]}}"#
        );
        let path = chats.join("session-2026-01-01T00-00-aaaa.json");
        fs::write(&path, body).unwrap();
        assert_eq!(peek_session_id(&path).as_deref(), Some("aaaa-bbbb"));
    }

    #[test]
    fn peek_session_id_falls_back_when_identity_comes_late() {
        let (_temp, resolver) = setup();
        let chats = resolver.chats_dir("/proj").unwrap();
        fs::create_dir_all(&chats).unwrap();
        let pad = "x".repeat(16 * 1024);
        let body = format!(r#"{{"messages":[{{"content":"{pad}"}}],"sessionId":"late-id"}}"#);
        let path = chats.join("session-2026-01-01T00-00-late.json");
        fs::write(&path, body).unwrap();
        assert_eq!(peek_session_id(&path).as_deref(), Some("late-id"));
    }

    #[test]
    fn prefix_session_id_rejects_keys_after_messages() {
        assert_eq!(
            prefix_session_id(br#"{"sessionId":"abc","messages":[]}"#).as_deref(),
            Some("abc")
        );
        assert_eq!(
            prefix_session_id(br#"{"messages":[],"sessionId":"abc"}"#),
            None,
            "a sessionId after the messages key must not be trusted from the prefix"
        );
        assert_eq!(prefix_session_id(br#"{"sessionId":""}"#), None);
    }

    #[test]
    fn list_session_entries_pairs_ids_with_backing_paths() {
        let (_temp, resolver) = setup();
        let chats = resolver.chats_dir("/proj").unwrap();
        fs::create_dir_all(&chats).unwrap();
        let main = chats.join("session-2026-01-01T00-00-aaaa.json");
        fs::write(&main, r#"{"sessionId":"uuid-a","messages":[]}"#).unwrap();
        // uuid-a's sub-agent bucket is claimed by the main file; uuid-b
        // is an orphan and must surface as its own session.
        fs::create_dir_all(chats.join("uuid-a")).unwrap();
        fs::create_dir_all(chats.join("uuid-b")).unwrap();

        let entries = resolver.list_session_entries("/proj").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "session-2026-01-01T00-00-aaaa");
        assert_eq!(entries[0].session_uuid.as_deref(), Some("uuid-a"));
        assert_eq!(entries[0].path, main);
        assert_eq!(entries[1].id, "uuid-b");
        assert_eq!(entries[1].session_uuid.as_deref(), Some("uuid-b"));
        assert_eq!(entries[1].path, chats.join("uuid-b"));

        // The plain listing keeps returning the same ids.
        let ids = resolver.list_sessions("/proj").unwrap();
        assert_eq!(ids, vec!["session-2026-01-01T00-00-aaaa", "uuid-b"]);
    }
}
