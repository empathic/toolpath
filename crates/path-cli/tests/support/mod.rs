//! Shared helpers for `path resume` integration tests.
//!
//! These are NOT integration-test entry points — they're a support
//! module imported by `tests/resume.rs`. Lives under `tests/` so it
//! doesn't leak into the production library API.

#![allow(dead_code)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use path_cli::cmd_resume::{HarnessArg, ResumeArgs};

/// Process-wide lock for tests that mutate `$HOME`, `$PATH`, or
/// `$TOOLPATH_CONFIG_DIR`. Integration tests under `tests/resume.rs`
/// can't reach the library's internal `crate::config::TEST_ENV_LOCK`,
/// so we use a separate lock here. Crucially, no library test holds
/// this lock — but library tests now properly save+restore env vars
/// (see commit 23deeb2), so the integration suite can be self-isolating.
pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// RAII guard that pins `$HOME` and `$TOOLPATH_CONFIG_DIR` to a tempdir.
pub struct ScopedHome {
    _td: tempfile::TempDir,
    prev_home: Option<OsString>,
    prev_config: Option<OsString>,
}

impl ScopedHome {
    pub fn new() -> Self {
        let td = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        let prev_config = std::env::var_os("TOOLPATH_CONFIG_DIR");
        unsafe {
            std::env::set_var("HOME", td.path());
            std::env::set_var("TOOLPATH_CONFIG_DIR", td.path().join(".toolpath"));
        }
        Self {
            _td: td,
            prev_home,
            prev_config,
        }
    }

    pub fn home_dir(&self) -> PathBuf {
        PathBuf::from(self._td.path())
    }
}

impl Drop for ScopedHome {
    fn drop(&mut self) {
        unsafe {
            match &self.prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match &self.prev_config {
                Some(v) => std::env::set_var("TOOLPATH_CONFIG_DIR", v),
                None => std::env::remove_var("TOOLPATH_CONFIG_DIR"),
            }
        }
    }
}

/// RAII guard that prepends a tempdir of fake binaries to `$PATH`.
pub struct ScopedPath {
    _td: tempfile::TempDir,
    prev: Option<OsString>,
}

impl ScopedPath {
    pub fn with_binary(name: &str) -> Self {
        Self::with_binaries(&[name])
    }

    pub fn with_binaries(names: &[&str]) -> Self {
        let td = tempfile::tempdir().unwrap();
        for n in names {
            let p = td.path().join(n);
            std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = std::fs::metadata(&p).unwrap().permissions();
                perm.set_mode(0o755);
                std::fs::set_permissions(&p, perm).unwrap();
            }
        }
        let prev = std::env::var_os("PATH");
        let new_path = std::env::join_paths(
            std::iter::once(td.path().to_path_buf())
                .chain(std::env::split_paths(&prev.clone().unwrap_or_default())),
        )
        .unwrap();
        unsafe {
            std::env::set_var("PATH", new_path);
        }
        Self { _td: td, prev }
    }

    pub fn empty() -> Self {
        let td = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", td.path());
        }
        Self { _td: td, prev }
    }
}

impl Drop for ScopedPath {
    fn drop(&mut self) {
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

/// Build a minimal `Path` whose single step has the given `actor`
/// and a `conversation.append` artifact keyed `<artifact_prefix>://<session>`.
/// The artifact key drives the harness projector's session-id extraction;
/// the actor satisfies `ensure_path_with_agent`.
pub fn make_convo_path(actor: &str, artifact_key: &str) -> toolpath::v1::Path {
    let mut extra = HashMap::new();
    extra.insert("role".to_string(), serde_json::json!("user"));
    extra.insert("text".to_string(), serde_json::json!("hello"));
    let step = toolpath::v1::Step {
        step: toolpath::v1::StepIdentity {
            id: "s1".to_string(),
            parents: vec![],
            actor: actor.to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        },
        change: {
            let mut m = HashMap::new();
            m.insert(
                artifact_key.to_string(),
                toolpath::v1::ArtifactChange {
                    raw: None,
                    structural: Some(toolpath::v1::StructuralChange {
                        change_type: "conversation.append".to_string(),
                        extra,
                    }),
                },
            );
            m
        },
        meta: None,
    };
    toolpath::v1::Path {
        path: toolpath::v1::PathIdentity {
            id: "p1".to_string(),
            base: None,
            head: "s1".to_string(),
            graph_ref: None,
        },
        steps: vec![step],
        meta: None,
    }
}

/// Convenience: write a single-path graph as JSON to `dir/doc.json`.
pub fn write_path_to_temp(dir: &Path, path: toolpath::v1::Path) -> PathBuf {
    let graph = toolpath::v1::Graph::from_path(path);
    let p = dir.join("doc.json");
    std::fs::write(&p, graph.to_json().unwrap()).unwrap();
    p
}

/// Construct `ResumeArgs` for a file-input + explicit-harness test.
pub fn args_explicit(input: PathBuf, cwd: &Path, harness: HarnessArg) -> ResumeArgs {
    ResumeArgs {
        input: input.to_string_lossy().to_string(),
        cwd: Some(cwd.to_path_buf()),
        harness: Some(harness),
        no_cache: false,
        force: false,
        url: None,
    }
}

/// Recursively walk `root` looking for a file with the given extension.
pub fn dir_contains_file_with_ext(root: &Path, ext: &str) -> bool {
    fn walk(p: &Path, ext: &str) -> bool {
        if !p.exists() {
            return false;
        }
        if p.is_dir() {
            for e in std::fs::read_dir(p).unwrap() {
                if walk(&e.unwrap().path(), ext) {
                    return true;
                }
            }
            false
        } else {
            p.extension().and_then(|s| s.to_str()) == Some(ext)
        }
    }
    walk(root, ext)
}
