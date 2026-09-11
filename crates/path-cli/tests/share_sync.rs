//! `path share` through the sync engine: one graph per session and
//! destination, updated in place, continued when frozen, and never
//! created on a server without the sync API.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

mod support;
use support::pathbase::MockPathbase;

fn cmd() -> Command {
    let mut c = Command::cargo_bin("path").unwrap();
    c.env_remove("CLAUDE_CONFIG_DIR");
    c
}

const TURN_2: &str = r#"{"type":"user","uuid":"u-2","parentUuid":"a-1","timestamp":"2024-01-01T00:05:00Z","cwd":"/x","message":{"role":"user","content":"more"}}"#;
const TURN_3: &str = r#"{"type":"user","uuid":"u-3","parentUuid":"u-2","timestamp":"2024-01-01T00:06:00Z","cwd":"/x","message":{"role":"user","content":"and more"}}"#;

/// A home with one Claude session (`session-abc`) for a project under it,
/// and an empty toolpath config dir. Returns (home, project, session file).
struct Fixture {
    home: tempfile::TempDir,
    cfg: tempfile::TempDir,
    project: PathBuf,
    session_file: PathBuf,
}

impl Fixture {
    fn new(server: &MockPathbase) -> Self {
        Self::with_claude_dir(server, None)
    }

    /// `claude_dir` places the Claude store outside `$HOME` (what
    /// `$CLAUDE_CONFIG_DIR` selects); `None` uses `$HOME/.claude`.
    fn with_claude_dir(server: &MockPathbase, claude_dir: Option<&Path>) -> Self {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let slug = project
            .to_string_lossy()
            .replace([std::path::MAIN_SEPARATOR, '_', '.'], "-");
        let claude_dir = claude_dir.map_or_else(|| home.path().join(".claude"), Path::to_path_buf);
        let project_dir = claude_dir.join("projects").join(&slug);
        std::fs::create_dir_all(&project_dir).unwrap();
        let session_file = project_dir.join("session-abc.jsonl");
        std::fs::write(
            &session_file,
            format!(
                r#"{{"type":"user","uuid":"u-1","timestamp":"2024-01-01T00:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":"hi"}}}}
{{"type":"assistant","uuid":"a-1","parentUuid":"u-1","timestamp":"2024-01-01T00:00:01Z","message":{{"role":"assistant","content":"hello"}}}}
"#,
                cwd = project.display()
            ),
        )
        .unwrap();
        let cfg = tempfile::tempdir().unwrap();
        server.write_credentials(cfg.path());
        Self {
            home,
            cfg,
            project,
            session_file,
        }
    }

    fn share(&self) -> Command {
        let mut c = cmd();
        c.env("HOME", self.home.path())
            .env("TOOLPATH_CONFIG_DIR", self.cfg.path())
            .args([
                "share",
                "--harness",
                "claude",
                "--session",
                "session-abc",
                "--project",
            ])
            .arg(&self.project);
        c
    }

    fn append(&self, line: &str) {
        let mut body = std::fs::read_to_string(&self.session_file).unwrap();
        body.push_str(line);
        body.push('\n');
        std::fs::write(&self.session_file, body).unwrap();
    }

    fn manifest(&self) -> serde_json::Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.cfg.path().join("manifest.json")).unwrap(),
        )
        .unwrap()
    }

    fn sync_state_dir(&self) -> PathBuf {
        self.cfg.path().join("sync-state")
    }
}

fn count(requests: &[String], prefix: &str) -> usize {
    requests.iter().filter(|r| r.starts_with(prefix)).count()
}

#[test]
fn share_creates_once_skips_unchanged_and_updates_in_place() {
    let server = MockPathbase::start();
    let f = Fixture::new(&server);
    let graphs_route = "POST /api/v1/u/alex/repos/pathstash/graphs".to_string();

    let out = f.share().assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let url = stdout.trim().to_string();
    out.stderr(predicate::str::contains("as a new graph"));
    assert_eq!(server.graphs().len(), 1);
    let g = &server.graphs()[0];
    assert_eq!(
        url,
        format!("{}/u/alex/pathstash/graphs/{}", server.base(), g.id)
    );
    assert_eq!(g.state, "mutable");
    assert_eq!(g.step_ids().len(), 2);
    let requests = server.requests();
    assert_eq!(requests[0], "GET /api/v1/u/me");
    assert!(
        requests
            .iter()
            .position(|r| r.ends_with("/graphs/00000000-0000-0000-0000-000000000000/meta"))
            < requests.iter().position(|r| *r == graphs_route),
        "the sync probe precedes the create: {requests:?}"
    );
    assert!(f.sync_state_dir().join("claude-session-abc.json").exists());
    assert_eq!(
        f.manifest()["claude"]["session-abc"]["uploads"][0]["graph_id"],
        serde_json::json!(g.id)
    );

    // Unchanged: nothing sent, the recorded URL printed again.
    f.share()
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "Already uploaded to alex/pathstash, unchanged since; pass --force to upload again",
        ))
        .stdout(predicate::str::contains(&url));
    let requests = server.requests();
    assert_eq!(count(&requests, &graphs_route), 1);
    assert_eq!(count(&requests, "PUT "), 0);

    // The session grows: the same graph is replaced, not duplicated.
    f.append(TURN_2);
    f.share()
        .assert()
        .success()
        .stderr(predicate::str::contains("updated the graph in place"))
        .stdout(predicate::str::contains(&url));
    let graphs = server.graphs();
    assert_eq!(graphs.len(), 1, "no second graph: {graphs:?}");
    assert_eq!(graphs[0].id, g.id);
    assert_eq!(graphs[0].step_ids().len(), 3);
    assert_eq!(graphs[0].generation, 1);
    let requests = server.requests();
    assert_eq!(count(&requests, &graphs_route), 1);
    assert_eq!(
        count(
            &requests,
            &format!("PUT /api/v1/u/alex/repos/pathstash/graphs/{}", g.id)
        ),
        1
    );
    assert!(
        !f.cfg.path().join("pending").exists()
            || std::fs::read_dir(f.cfg.path().join("pending"))
                .unwrap()
                .count()
                == 0
    );
}

#[test]
fn share_continues_a_graph_that_sync_froze() {
    let server = MockPathbase::start();
    let f = Fixture::new(&server);
    f.share().assert().success();
    let first = server.graphs()[0].clone();
    server.freeze(&first.id);

    f.append(TURN_2);
    f.share()
        .assert()
        .success()
        .stderr(predicate::str::contains("continuation of the frozen graph"));
    let graphs = server.graphs();
    assert_eq!(graphs.len(), 2);
    let cont = graphs.iter().find(|g| g.id != first.id).unwrap();
    assert_eq!(cont.state, "mutable");
    assert_eq!(cont.step_ids(), ["u-2"]);
    assert!(
        cont.base_from
            .as_deref()
            .is_some_and(|from| from.contains(&first.id) && from.ends_with("/a-1")),
        "{:?}",
        cont.base_from
    );
    assert_eq!(server.graph(&first.id).step_ids().len(), 2);
    assert_eq!(
        count(
            &server.requests(),
            &format!(
                "POST /api/v1/u/alex/repos/pathstash/graphs/{}/continuations",
                first.id
            )
        ),
        1
    );

    // Frozen again with nothing new: the frozen message, no new graph.
    server.freeze(&cont.id);
    f.share()
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "Already uploaded to alex/pathstash and frozen; nothing new to upload",
        ))
        .stdout(predicate::str::contains(&cont.id));
    assert_eq!(server.graphs().len(), 2);
}

/// Share stamps sessions through the same config-rooted providers the
/// derive uses, so a `$CLAUDE_CONFIG_DIR` store is found by both and an
/// unchanged session is recognised as such (and read from the cache).
#[test]
fn share_honours_claude_config_dir_for_stamps() {
    let server = MockPathbase::start();
    let store = tempfile::tempdir().unwrap();
    let f = Fixture::with_claude_dir(&server, Some(store.path()));
    let share = || {
        let mut c = f.share();
        c.env("CLAUDE_CONFIG_DIR", store.path());
        c
    };
    share()
        .assert()
        .success()
        .stderr(predicate::str::contains("as a new graph"));
    let upload = f.manifest()["claude"]["session-abc"]["uploads"][0].clone();
    assert!(upload["modified"].is_string(), "{upload}");
    assert!(upload["size"].is_number(), "{upload}");
    share()
        .assert()
        .success()
        .stderr(predicate::str::contains("uploading without re-deriving"))
        .stderr(predicate::str::contains("unchanged since"));
    assert_eq!(server.graphs().len(), 1);
    assert_eq!(count(&server.requests(), "PUT "), 0);
}

#[test]
fn share_refuses_a_server_without_the_sync_api() {
    let server = MockPathbase::start_as("alex", false);
    let f = Fixture::new(&server);
    f.share()
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not support sync"));
    let requests = server.requests();
    assert_eq!(count(&requests, "POST "), 0, "{requests:?}");
    assert!(server.graphs().is_empty());
}

#[test]
fn force_resends_unchanged_and_replaces_after_a_source_regression() {
    let server = MockPathbase::start();
    let f = Fixture::new(&server);
    f.share().assert().success();
    let g = server.graphs()[0].clone();
    let put = format!("PUT /api/v1/u/alex/repos/pathstash/graphs/{}", g.id);

    // Unchanged plus --force: a PUT the server treats as a no-op.
    f.share()
        .arg("--force")
        .assert()
        .success()
        .stderr(predicate::str::contains("updated the graph in place"));
    assert_eq!(count(&server.requests(), &put), 1);
    assert_eq!(server.graph(&g.id).generation, 0);
    assert_eq!(server.graphs().len(), 1);

    // The source loses the assistant turn: refused without --force.
    let first_line = std::fs::read_to_string(&f.session_file)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    std::fs::write(&f.session_file, format!("{first_line}\n")).unwrap();
    f.share()
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "source lost 1 acknowledged step(s) (a-1)",
        ));
    assert_eq!(count(&server.requests(), &put), 1);
    assert_eq!(server.graph(&g.id).step_ids().len(), 2);

    f.share()
        .arg("--force")
        .assert()
        .success()
        .stderr(predicate::str::contains("updated the graph in place"));
    assert_eq!(count(&server.requests(), &put), 2);
    assert_eq!(server.graph(&g.id).step_ids(), ["u-1"]);
    assert_eq!(server.graphs().len(), 1);
}

#[test]
fn share_all_updates_tracked_sessions_instead_of_duplicating() {
    let server = MockPathbase::start();
    let f = Fixture::new(&server);
    f.share().assert().success();
    let g = server.graphs()[0].clone();

    let all = |f: &Fixture| {
        let mut c = cmd();
        c.env("HOME", f.home.path())
            .env("TOOLPATH_CONFIG_DIR", f.cfg.path())
            .args(["share", "--all", "--yes", "--project-under"])
            .arg(f.home.path());
        c
    };
    all(&f)
        .assert()
        .success()
        .stderr(predicate::str::contains("1 already uploaded"))
        .stderr(predicate::str::contains("Nothing to upload"));

    f.append(TURN_2);
    all(&f)
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("(1 to update)"))
        .stderr(predicate::str::contains(format!(
            "would update graph {}",
            g.id
        )))
        .stderr(predicate::str::contains(
            "Would upload 1 sessions to alex/pathstash",
        ));
    assert_eq!(server.graph(&g.id).step_ids().len(), 2);

    all(&f)
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "(cached) → {}/u/alex/pathstash/graphs/{}",
            server.base(),
            g.id
        )))
        .stderr(predicate::str::contains("Uploaded 1 sessions"))
        .stdout(predicate::str::contains("alex/pathstash"));
    assert_eq!(server.graphs().len(), 1);
    assert_eq!(server.graph(&g.id).step_ids().len(), 3);

    f.append(TURN_3);
    server.freeze(&g.id);
    all(&f)
        .assert()
        .success()
        .stderr(predicate::str::contains("Uploaded 1 sessions"));
    assert_eq!(server.graphs().len(), 2);
}

#[test]
fn anon_share_stays_outside_the_engine() {
    let server = MockPathbase::start();
    let f = Fixture::new(&server);
    let anon = "POST /api/v1/u/anon/repos/pathstash/graphs".to_string();
    for _ in 0..2 {
        f.share()
            .args(["--anon", "--url", server.base()])
            .assert()
            .success()
            .stderr(predicate::str::contains("anon graph"));
    }
    let requests = server.requests();
    assert_eq!(count(&requests, &anon), 2, "{requests:?}");
    assert_eq!(
        count(&requests, "GET "),
        0,
        "no probe, no meta: {requests:?}"
    );
    assert!(f.manifest()["claude"]["session-abc"]["uploads"].is_null());
    assert!(!f.sync_state_dir().exists());
    assert!(server.graphs().iter().all(|g| g.state == "frozen"));
}

#[test]
fn export_pathbase_of_a_file_creates_directly_but_refuses_an_old_server() {
    let doc = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/path-02-local-session.path.json");
    let server = MockPathbase::start();
    let cfg = tempfile::tempdir().unwrap();
    server.write_credentials(cfg.path());
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "export", "pathbase", "--input"])
        .arg(&doc)
        .assert()
        .success()
        .stderr(predicate::str::contains("Uploaded"));
    assert_eq!(server.graphs().len(), 1);
    assert!(!cfg.path().join("sync-state").exists());
    assert!(!cfg.path().join("manifest.json").exists());

    let old = MockPathbase::start_as("alex", false);
    let cfg = tempfile::tempdir().unwrap();
    old.write_credentials(cfg.path());
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "export", "pathbase", "--input"])
        .arg(&doc)
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not support sync"));
    assert!(old.graphs().is_empty());
}
