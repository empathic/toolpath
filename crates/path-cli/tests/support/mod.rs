//! Shared helpers for `path resume` integration tests.
//!
//! These are NOT integration-test entry points — they're a support
//! module imported by `tests/resume.rs`. Lives under `tests/` so it
//! doesn't leak into the production library API.
//!
//! [`TestHome`] is the sandbox: a tempdir plus the `Config` that points
//! at it. Tests pass that `Config` to the code under test, so no test
//! reads or mutates the process environment.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use path_cli::cmd_resume::ResumeArgs;
use path_cli::config::Config;
use path_cli::harness::Harness;

/// A tempdir that stands in for the user's home directory.
pub struct TestHome {
    td: tempfile::TempDir,
}

impl TestHome {
    pub fn new() -> Self {
        Self {
            td: tempfile::tempdir().unwrap(),
        }
    }

    pub fn home_dir(&self) -> PathBuf {
        self.td.path().to_path_buf()
    }

    /// The toolpath config directory inside the sandbox.
    pub fn config_dir(&self) -> PathBuf {
        self.td.path().join(".toolpath")
    }

    /// The `Config` the code under test receives. Every other field
    /// stays `None`, so all seven harness resolvers root under the
    /// sandbox home whatever the developer's environment holds.
    pub fn config(&self) -> Config {
        Config {
            home: Some(self.home_dir()),
            toolpath_config_dir: Some(self.config_dir()),
            ..Config::default()
        }
    }
}

impl Default for TestHome {
    fn default() -> Self {
        Self::new()
    }
}

/// A tempdir holding an executable stub per name in `names`. Pass its
/// path as the search path the code under test probes.
pub fn fake_bin_dir(names: &[&str]) -> tempfile::TempDir {
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
    td
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
pub fn args_explicit(input: PathBuf, cwd: &Path, harness: Harness) -> ResumeArgs {
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
