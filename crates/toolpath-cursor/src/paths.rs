//! Filesystem layout for Cursor state.
//!
//! Cursor splits its data across two roots: an Anysphere-specific tree
//! at `~/.cursor/` (used for project slugs and the per-composer JSONL
//! agent transcript), and a VS Code-flavored Electron user-data tree
//! at `~/Library/Application Support/Cursor/` on macOS,
//! `~/.config/Cursor/` on Linux, and
//! `%APPDATA%\Cursor\` on Windows.
//!
//! The source of truth for conversations is the global SQLite database
//! at `<user-data>/User/globalStorage/state.vscdb`. The JSONL
//! transcripts are useful for fast project-keyed listing but lossy.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Subset of `workspace.json` that Cursor (and VS Code) writes for
/// every workspace folder it's opened. We only care about the
/// `folder` URI — that's what we match on when looking up the
/// workspace id for a given on-disk folder.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WorkspaceManifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
}

/// Outcome of [`PathResolver::ensure_workspace_storage_entry`].
#[derive(Debug, Clone)]
pub struct EnsuredWorkspaceId {
    /// The 32-hex-char id Cursor will see / does see for this
    /// workspace.
    pub id: String,
    /// `true` if we created a new `workspaceStorage/<id>/workspace.json`
    /// just now (folder hadn't been opened in Cursor before).
    pub created: bool,
}

const ANYSPHERE_SUBDIR: &str = ".cursor";
const PROJECTS_SUBDIR: &str = "projects";
const AGENT_TRANSCRIPTS_SUBDIR: &str = "agent-transcripts";
const USER_SUBDIR: &str = "User";
const GLOBAL_STORAGE_SUBDIR: &str = "globalStorage";
const WORKSPACE_STORAGE_SUBDIR: &str = "workspaceStorage";
const WORKSPACE_JSON: &str = "workspace.json";
const DB_FILE: &str = "state.vscdb";

/// Builder-style resolver over Cursor's data directories.
#[derive(Debug, Clone)]
pub struct PathResolver {
    home_dir: PathBuf,
    /// Override for `~/.cursor/`.
    anysphere_dir: Option<PathBuf>,
    /// Override for the Electron user-data root
    /// (`~/Library/Application Support/Cursor/` on macOS).
    user_data_dir: Option<PathBuf>,
    /// The Windows roaming-application-data root (`%APPDATA%`). Only
    /// the Windows default user-data directory consults it.
    appdata: Option<PathBuf>,
}

impl PathResolver {
    /// A resolver rooted at `home`, so the Anysphere directory is
    /// `<home>/.cursor`.
    pub fn new<P: Into<PathBuf>>(home: P) -> Self {
        Self {
            home_dir: home.into(),
            anysphere_dir: None,
            user_data_dir: None,
            appdata: None,
        }
    }

    /// Override `~/.cursor/` directly.
    pub fn with_anysphere_dir<P: Into<PathBuf>>(mut self, dir: P) -> Self {
        self.anysphere_dir = Some(dir.into());
        self
    }

    /// Override the Electron user-data root. On macOS this defaults
    /// to `<home>/Library/Application Support/Cursor`.
    pub fn with_user_data_dir<P: Into<PathBuf>>(mut self, dir: P) -> Self {
        self.user_data_dir = Some(dir.into());
        self
    }

    /// Set the Windows roaming-application-data root (`%APPDATA%`).
    /// The Windows default user-data directory is `<appdata>/Cursor`.
    /// Every other platform ignores the value.
    pub fn with_appdata<P: Into<PathBuf>>(mut self, appdata: P) -> Self {
        self.appdata = Some(appdata.into());
        self
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    /// Path to `~/.cursor/`.
    pub fn anysphere_dir(&self) -> PathBuf {
        match &self.anysphere_dir {
            Some(d) => d.clone(),
            None => self.home_dir.join(ANYSPHERE_SUBDIR),
        }
    }

    /// Path to `~/.cursor/projects/`.
    pub fn projects_dir(&self) -> PathBuf {
        self.anysphere_dir().join(PROJECTS_SUBDIR)
    }

    /// Path to the agent-transcripts folder for a project slug.
    pub fn project_transcripts_dir(&self, slug: &str) -> PathBuf {
        self.projects_dir()
            .join(slug)
            .join(AGENT_TRANSCRIPTS_SUBDIR)
    }

    /// Path to the JSONL transcript file for a composer in a project.
    pub fn transcript_path(&self, slug: &str, composer_id: &str) -> PathBuf {
        self.project_transcripts_dir(slug)
            .join(composer_id)
            .join(format!("{composer_id}.jsonl"))
    }

    /// Path to the Electron user-data root.
    pub fn user_data_dir(&self) -> PathBuf {
        match &self.user_data_dir {
            Some(d) => d.clone(),
            None => default_user_data_dir(&self.home_dir, self.appdata.as_deref()),
        }
    }

    /// Path to `<user-data>/User/`.
    pub fn user_dir(&self) -> PathBuf {
        self.user_data_dir().join(USER_SUBDIR)
    }

    /// Path to `<user-data>/User/globalStorage/`.
    pub fn global_storage_dir(&self) -> PathBuf {
        self.user_dir().join(GLOBAL_STORAGE_SUBDIR)
    }

    /// Path to the primary cross-workspace SQLite database
    /// (`<user-data>/User/globalStorage/state.vscdb`).
    pub fn db_path(&self) -> PathBuf {
        self.global_storage_dir().join(DB_FILE)
    }

    /// Path to `<user-data>/User/workspaceStorage/`. Cursor stores
    /// one subdirectory per workspace folder it's been opened
    /// against, named by the workspace id (an opaque 32-hex-char
    /// hash Cursor computes from the folder URI). Each subdir
    /// contains a `workspace.json` with the canonical `folder` URI
    /// that subdir is bound to.
    pub fn workspace_storage_dir(&self) -> PathBuf {
        self.user_dir().join(WORKSPACE_STORAGE_SUBDIR)
    }

    /// Look up Cursor's workspace id for a given folder, if it has
    /// been opened in Cursor.app before. Scans every
    /// `workspaceStorage/<id>/workspace.json`, returning the `<id>`
    /// whose recorded `folder` URI canonicalizes to `folder`.
    ///
    /// Returns `Ok(None)` when the folder hasn't been opened yet —
    /// the caller can decide whether to synthesize one via
    /// [`Self::ensure_workspace_storage_entry`].
    pub fn find_workspace_id(&self, folder: &Path) -> Result<Option<String>> {
        let storage_root = self.workspace_storage_dir();
        if !storage_root.exists() {
            return Ok(None);
        }
        let target = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());

        for entry in std::fs::read_dir(&storage_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let manifest = entry.path().join(WORKSPACE_JSON);
            let Ok(raw) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let Ok(parsed) = serde_json::from_str::<WorkspaceManifest>(&raw) else {
                continue;
            };
            let Some(folder_uri) = parsed.folder.as_deref() else {
                continue;
            };
            let Some(path_part) = folder_uri.strip_prefix("file://") else {
                continue;
            };
            let recorded =
                std::fs::canonicalize(path_part).unwrap_or_else(|_| PathBuf::from(path_part));
            if recorded == target {
                let id = entry.file_name().to_string_lossy().into_owned();
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Look up the workspace id for `folder`; if none exists, create
    /// `workspaceStorage/<synthesized-id>/workspace.json` recording
    /// the folder URI and return the synthesized id. The next time
    /// Cursor.app opens that folder it'll scan workspaceStorage,
    /// match by URI, and adopt our id — so any composer we projected
    /// with `workspaceIdentifier.id = <our-id>` lights up in the
    /// sidebar.
    ///
    /// `synthesize_id` decides what id to assign for new folders.
    /// Production callers should pass a stable hash (e.g. the
    /// first 32 hex chars of SHA-256 of the folder path) so re-runs
    /// don't accumulate orphaned workspaceStorage entries.
    pub fn ensure_workspace_storage_entry(
        &self,
        folder: &Path,
        synthesize_id: impl FnOnce(&Path) -> String,
    ) -> Result<EnsuredWorkspaceId> {
        if let Some(id) = self.find_workspace_id(folder)? {
            return Ok(EnsuredWorkspaceId { id, created: false });
        }
        let id = synthesize_id(folder);
        let dir = self.workspace_storage_dir().join(&id);
        std::fs::create_dir_all(&dir)?;
        let canonical = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
        let folder_uri = format!("file://{}", canonical.to_string_lossy());
        let manifest = WorkspaceManifest {
            folder: Some(folder_uri),
        };
        let json = serde_json::to_string_pretty(&manifest)?;
        std::fs::write(dir.join(WORKSPACE_JSON), json)?;
        Ok(EnsuredWorkspaceId { id, created: true })
    }

    /// Whether Cursor's user-data tree exists.
    pub fn exists(&self) -> bool {
        self.user_data_dir().exists()
    }

    /// Whether the primary global SQLite database exists.
    pub fn db_exists(&self) -> bool {
        self.db_path().exists()
    }
}

/// Slugify an absolute filesystem path into Cursor's project-slug form
/// (the directory name under `~/.cursor/projects/`).
///
/// Cursor encodes `/Users/ben/projects/temp/cursortest` →
/// `Users-ben-projects-temp-cursortest` (strip leading `/`, replace `/`
/// with `-`). Tmp-dir workspaces and remote/untitled workspaces use
/// different slugs we don't try to reconstruct here.
pub fn slug_from_abs_path(abs: &str) -> String {
    abs.trim_start_matches('/').replace('/', "-")
}

#[cfg(target_os = "macos")]
fn default_user_data_dir(home: &Path, _appdata: Option<&Path>) -> PathBuf {
    home.join("Library/Application Support/Cursor")
}

#[cfg(target_os = "linux")]
fn default_user_data_dir(home: &Path, _appdata: Option<&Path>) -> PathBuf {
    home.join(".config/Cursor")
}

#[cfg(target_os = "windows")]
fn default_user_data_dir(home: &Path, appdata: Option<&Path>) -> PathBuf {
    match appdata {
        Some(appdata) => appdata.join("Cursor"),
        None => home.join("AppData/Roaming/Cursor"),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn default_user_data_dir(home: &Path, _appdata: Option<&Path>) -> PathBuf {
    home.join(".config/Cursor")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathResolver) {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new(temp.path())
            .with_anysphere_dir(temp.path().join(".cursor"))
            .with_user_data_dir(temp.path().join("UserData"));
        (temp, resolver)
    }

    #[test]
    fn anysphere_dir_defaults_to_home_dotcursor() {
        let temp = TempDir::new().unwrap();
        let r = PathResolver::new(temp.path());
        assert_eq!(r.anysphere_dir(), temp.path().join(".cursor"));
        assert_eq!(r.home_dir(), temp.path());
    }

    #[test]
    fn db_path_under_global_storage() {
        let (_t, r) = setup();
        assert!(
            r.db_path()
                .ends_with("UserData/User/globalStorage/state.vscdb")
        );
    }

    /// The user-data override wins against the home default.
    #[test]
    fn user_data_dir_override_wins_against_home() {
        let r = PathResolver::new("/custom/home").with_user_data_dir("/custom/user-data");
        assert_eq!(r.user_data_dir(), PathBuf::from("/custom/user-data"));
    }

    /// `%APPDATA%` reaches the Windows default user-data directory.
    /// Every other platform keeps its own default.
    #[cfg(target_os = "windows")]
    #[test]
    fn appdata_feeds_the_windows_default_user_data_dir() {
        let r = PathResolver::new("/custom/home").with_appdata("/appdata/roaming");
        assert_eq!(r.user_data_dir(), PathBuf::from("/appdata/roaming/Cursor"));
        assert_eq!(
            PathResolver::new("/custom/home").user_data_dir(),
            PathBuf::from("/custom/home/AppData/Roaming/Cursor")
        );
    }

    #[test]
    fn transcript_path_uses_double_uuid() {
        let (_t, r) = setup();
        let uuid = "724686cd-875e-47da-a90b-dbc3e523efb8";
        let p = r.transcript_path("my-project", uuid);
        assert!(p.ends_with(format!("agent-transcripts/{uuid}/{uuid}.jsonl")));
    }

    #[test]
    fn slug_strips_leading_slash_and_replaces() {
        assert_eq!(
            slug_from_abs_path("/Users/ben/projects/temp/cursortest"),
            "Users-ben-projects-temp-cursortest"
        );
        assert_eq!(slug_from_abs_path("/a"), "a");
    }

    #[test]
    fn exists_reflects_user_data_dir() {
        let (_t, r) = setup();
        std::fs::create_dir_all(r.user_data_dir()).unwrap();
        assert!(r.exists());
        let missing = PathResolver::new("/never/exists");
        assert!(!missing.exists());
    }

    #[test]
    fn find_workspace_id_matches_by_folder_uri() {
        let (t, r) = setup();
        let folder = t.path().join("project");
        std::fs::create_dir_all(&folder).unwrap();
        let canonical = std::fs::canonicalize(&folder).unwrap();
        let folder_uri = format!("file://{}", canonical.to_string_lossy());

        let storage = r.workspace_storage_dir();
        let ws_dir = storage.join("deadbeefdeadbeefdeadbeefdeadbeef");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("workspace.json"),
            format!(r#"{{"folder": "{folder_uri}"}}"#),
        )
        .unwrap();

        let found = r.find_workspace_id(&folder).unwrap();
        assert_eq!(found.as_deref(), Some("deadbeefdeadbeefdeadbeefdeadbeef"));

        let other = t.path().join("nope");
        std::fs::create_dir_all(&other).unwrap();
        assert!(r.find_workspace_id(&other).unwrap().is_none());
    }

    #[test]
    fn ensure_workspace_storage_creates_entry_when_missing() {
        let (t, r) = setup();
        let folder = t.path().join("brand-new");
        std::fs::create_dir_all(&folder).unwrap();

        let ensured = r
            .ensure_workspace_storage_entry(&folder, |_| "11feedbeef00000000000000feedbeef".into())
            .unwrap();
        assert!(ensured.created);
        assert_eq!(ensured.id, "11feedbeef00000000000000feedbeef");

        // Manifest is written and points back at our folder.
        let manifest_path = r
            .workspace_storage_dir()
            .join(&ensured.id)
            .join("workspace.json");
        let raw = std::fs::read_to_string(&manifest_path).unwrap();
        let canonical = std::fs::canonicalize(&folder).unwrap();
        assert!(raw.contains(&format!("file://{}", canonical.to_string_lossy())));

        // Second call finds the existing one — doesn't re-create.
        let again = r
            .ensure_workspace_storage_entry(&folder, |_| "should-not-be-used".into())
            .unwrap();
        assert!(!again.created);
        assert_eq!(again.id, ensured.id);
    }
}
