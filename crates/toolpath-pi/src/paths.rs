//! Path resolution for Pi's on-disk layout.
//!
//! Pi stores session logs at:
//!
//! ```text
//! <home>/.pi/agent/sessions/--<encoded-cwd>--/<timestamp>_<uuid>.jsonl
//! ```
//!
//! The project directory name encodes the cwd: the leading slash is dropped,
//! each remaining `/` becomes `-`, and the whole thing is wrapped in
//! `--...--`. This is lossy for paths that contain internal dashes (Pi has
//! the same limitation), but we accept the ambiguity for now.

use std::path::{Path, PathBuf};

/// Resolves the Pi sessions directory and its project subdirectories.
#[derive(Debug, Clone)]
pub struct PathResolver {
    sessions_dir: PathBuf,
}

impl PathResolver {
    /// Resolver for `<home>/.pi/agent/sessions/`. The caller supplies the
    /// home directory.
    pub fn new(home: impl AsRef<Path>) -> Self {
        Self {
            sessions_dir: home.as_ref().join(".pi").join("agent").join("sessions"),
        }
    }

    /// Override the sessions base directory directly.
    pub fn with_sessions_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.sessions_dir = dir.as_ref().to_path_buf();
        self
    }

    /// The resolved sessions directory (`.../sessions/`).
    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// Project directory for a given cwd.
    pub fn project_dir(&self, cwd: &str) -> PathBuf {
        self.sessions_dir.join(encode_project(cwd))
    }

    /// Whether the sessions directory exists.
    pub fn exists(&self) -> bool {
        self.sessions_dir.exists()
    }

    /// Return the project cwd → directory-name encoding for `cwd`.
    pub fn encode_cwd(&self, cwd: &str) -> String {
        encode_project(cwd)
    }

    /// Return the cwd decoded from a project directory name.
    pub fn decode_project_dir(&self, dir_name: &str) -> String {
        decode_project(dir_name)
    }

    /// Enumerate project directories that exist on disk. Returns decoded cwd
    /// strings, sorted ascending. Missing sessions_dir returns an empty vec
    /// rather than an error.
    pub fn list_projects(&self) -> std::io::Result<Vec<String>> {
        if !self.sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                out.push(decode_project(name));
            }
        }
        out.sort();
        Ok(out)
    }
}

/// Encode a cwd path into a Pi project directory name.
///
/// ```
/// # use toolpath_pi::paths::encode_project;
/// assert_eq!(encode_project("/Users/alex/project"), "--Users-alex-project--");
/// assert_eq!(encode_project("/"), "----");
/// assert_eq!(encode_project(""), "----");
/// ```
pub fn encode_project(cwd: &str) -> String {
    let trimmed = cwd.trim_start_matches('/');
    format!("--{}--", trimmed.replace('/', "-"))
}

/// Decode a Pi project directory name back into a cwd.
///
/// The empty encoding `----` decodes to `/`.
///
/// ```
/// # use toolpath_pi::paths::decode_project;
/// assert_eq!(decode_project("--Users-alex-project--"), "/Users/alex/project");
/// assert_eq!(decode_project("----"), "/");
/// ```
pub fn decode_project(dir_name: &str) -> String {
    // Strip exactly one leading and trailing "--" if present.
    let after_prefix = dir_name.strip_prefix("--").unwrap_or(dir_name);
    let inner = after_prefix.strip_suffix("--").unwrap_or(after_prefix);
    if inner.is_empty() {
        return "/".to_string();
    }
    format!("/{}", inner.replace('-', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_sessions_dir_sits_under_the_home_argument() {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new(temp.path());
        assert_eq!(
            resolver.sessions_dir(),
            temp.path().join(".pi/agent/sessions")
        );
    }

    #[test]
    fn test_with_sessions_dir_override() {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new("/tmp/fake-home").with_sessions_dir(temp.path());
        assert_eq!(resolver.sessions_dir(), temp.path());
    }

    #[test]
    fn test_with_sessions_dir_override_beats_the_home_argument() {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new("/some/other/home").with_sessions_dir(temp.path());
        assert_eq!(resolver.sessions_dir(), temp.path());
    }

    #[test]
    fn test_encode_roundtrip() {
        for cwd in ["/Users/alex/proj", "/", "/a", "/a/b/c", "/home/user/repo"] {
            let encoded = encode_project(cwd);
            let decoded = decode_project(&encoded);
            assert_eq!(decoded, cwd, "roundtrip failed for {cwd}");
        }
    }

    #[test]
    fn test_encode_strips_leading_slash() {
        assert_eq!(
            encode_project("/Users/alex/project"),
            "--Users-alex-project--"
        );
        assert_eq!(
            encode_project("Users/alex/project"),
            "--Users-alex-project--"
        );
    }

    #[test]
    fn test_encode_wraps_double_dashes() {
        let s = encode_project("/a/b");
        assert!(s.starts_with("--"));
        assert!(s.ends_with("--"));
    }

    #[test]
    fn test_decode_root() {
        assert_eq!(decode_project("----"), "/");
    }

    #[test]
    fn test_encode_empty() {
        assert_eq!(encode_project(""), "----");
        assert_eq!(encode_project("/"), "----");
    }

    #[test]
    fn test_project_dir_combines_sessions_and_encoded_cwd() {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new("/tmp/fake-home").with_sessions_dir(temp.path());
        let pd = resolver.project_dir("/Users/alex/proj");
        assert_eq!(pd, temp.path().join("--Users-alex-proj--"));
    }

    #[test]
    fn test_list_projects_empty_dir() {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new("/tmp/fake-home").with_sessions_dir(temp.path());
        let projects = resolver.list_projects().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn test_list_projects_nonexistent_dir() {
        let temp = TempDir::new().unwrap();
        let missing = temp.path().join("does-not-exist");
        let resolver = PathResolver::new("/tmp/fake-home").with_sessions_dir(&missing);
        let projects = resolver.list_projects().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn test_list_projects_skips_non_dirs() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join("--Users-alex-proj--")).unwrap();
        fs::write(temp.path().join("stray-file.txt"), "hi").unwrap();

        let resolver = PathResolver::new("/tmp/fake-home").with_sessions_dir(temp.path());
        let projects = resolver.list_projects().unwrap();
        assert_eq!(projects, vec!["/Users/alex/proj".to_string()]);
    }

    #[test]
    fn test_list_projects_returns_decoded_cwds() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join("--Users-alex-proj--")).unwrap();
        fs::create_dir(temp.path().join("--home-bob-repo--")).unwrap();

        let resolver = PathResolver::new("/tmp/fake-home").with_sessions_dir(temp.path());
        let projects = resolver.list_projects().unwrap();
        assert_eq!(
            projects,
            vec!["/Users/alex/proj".to_string(), "/home/bob/repo".to_string(),]
        );
    }

    #[test]
    fn test_exists_returns_false_for_missing_dir() {
        let temp = TempDir::new().unwrap();
        let resolver =
            PathResolver::new("/tmp/fake-home").with_sessions_dir(temp.path().join("nope"));
        assert!(!resolver.exists());
    }

    #[test]
    fn test_exists_returns_true_for_created_dir() {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::new("/tmp/fake-home").with_sessions_dir(temp.path());
        assert!(resolver.exists());
    }

    #[test]
    fn test_debug_impl_doesnt_panic() {
        let resolver = PathResolver::new("/tmp/fake-home");
        let s = format!("{resolver:?}");
        assert!(!s.is_empty());
    }

    #[test]
    fn test_clone_produces_equal_resolver() {
        let resolver = PathResolver::new("/tmp/fake-home");
        let cloned = resolver.clone();
        assert_eq!(resolver.sessions_dir(), cloned.sessions_dir());
    }

    #[test]
    fn test_encode_cwd_and_decode_project_dir_methods() {
        let resolver = PathResolver::new("/tmp/fake-home");
        assert_eq!(resolver.encode_cwd("/a/b"), "--a-b--");
        assert_eq!(resolver.decode_project_dir("--a-b--"), "/a/b");
    }
}
