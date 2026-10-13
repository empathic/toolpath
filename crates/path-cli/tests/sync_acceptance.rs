//! Acceptance test for the path sync client: `path share --all` against
//! a running Pathbase, as the design's "Acceptance test" section lays
//! out (docs/superpowers/specs/2026-10-13-path-sync-client-design.md).
//!
//! Gated on `TEST_PATHBASE_URL` and `TEST_PATHBASE_TOKEN` (a CLI token
//! for a user on that server, as `path auth login` stores it); without
//! both, each test prints why and passes. The server's public URL must
//! be `TEST_PATHBASE_URL`, since continuations name their parent by it.
//!
//! Each test copies a real session from `test-fixtures/` into a temp
//! harness home, truncated, runs a pass of the real `path` binary, and
//! grows the file between passes. It asserts on the server's step ids
//! (the step-ids route), on the request bodies the CLI sent (through a
//! logging forward proxy set as `HTTP_PROXY`, so the CLI still addresses
//! the server by its own URL), and on `remotes.json`.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use assert_cmd::Command;
use serde_json::{Value, json};

const CLAUDE_ID: &str = "89f4d66a-510b-48b3-94b8-d6a8f981b143";
const CODEX_ID: &str = "019e03b2-11a2-7320-9098-5d831813e66c";

// ── The server ──────────────────────────────────────────────────────

struct Server {
    url: String,
    token: String,
    owner: String,
    http: reqwest::blocking::Client,
}

impl Server {
    fn from_env() -> Option<Self> {
        let (Ok(url), Ok(token)) = (
            std::env::var("TEST_PATHBASE_URL"),
            std::env::var("TEST_PATHBASE_TOKEN"),
        ) else {
            eprintln!("skipping: set TEST_PATHBASE_URL and TEST_PATHBASE_TOKEN to run");
            return None;
        };
        let url = url.trim_end_matches('/').to_string();
        let http = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let me: Value = http
            .get(format!("{url}/api/v1/u/me"))
            .bearer_auth(&token)
            .send()
            .unwrap()
            .error_for_status()
            .expect("TEST_PATHBASE_TOKEN is accepted")
            .json()
            .unwrap();
        let owner = me["username"].as_str().unwrap().to_string();
        Some(Self {
            url,
            token,
            owner,
            http,
        })
    }

    /// A fresh repo for one test, so runs do not see each other.
    fn new_repo(&self, prefix: &str) -> String {
        let name = format!(
            "{prefix}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let resp = self
            .http
            .post(format!("{}/api/v1/u/{}/repos", self.url, self.owner))
            .bearer_auth(&self.token)
            .json(&json!({"name": name}))
            .send()
            .unwrap();
        assert!(resp.status().is_success(), "create repo: {}", resp.status());
        name
    }

    fn step_ids(&self, path_url: &str) -> Vec<String> {
        let v: Value = self
            .http
            .get(format!("{path_url}/step-ids"))
            .bearer_auth(&self.token)
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap();
        v["step_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect()
    }

    fn freeze(&self, path_url: &str) {
        let resp = self
            .http
            .post(format!("{}/freeze", graph_url(path_url)))
            .bearer_auth(&self.token)
            .send()
            .unwrap();
        assert!(resp.status().is_success(), "freeze: {}", resp.status());
    }

    fn delete(&self, path_url: &str) {
        let resp = self
            .http
            .delete(graph_url(path_url))
            .bearer_auth(&self.token)
            .send()
            .unwrap();
        assert!(resp.status().is_success(), "delete: {}", resp.status());
    }

    /// The stored document of the graph holding `path_url`.
    fn document(&self, path_url: &str) -> Value {
        self.http
            .get(format!("{}/download", graph_url(path_url)))
            .bearer_auth(&self.token)
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap()
    }
}

/// `…/graphs/<g>` of a path API URL `…/graphs/<g>/paths/<p>`.
fn graph_url(path_url: &str) -> &str {
    let cut = path_url.rfind("/paths/").expect("a path API URL");
    &path_url[..cut]
}

/// The graph page URL Pathbase resolves `path.parent.uri` against.
fn graph_page_url(path_url: &str) -> String {
    graph_url(path_url)
        .replacen("/api/v1/u/", "/u/", 1)
        .replacen("/repos/", "/", 1)
}

// ── A logging forward proxy ─────────────────────────────────────────

#[derive(Debug, Clone)]
struct Logged {
    method: String,
    url: String,
    body: String,
}

impl Logged {
    fn step_ids(&self) -> Vec<String> {
        if let Ok(upload) = serde_json::from_str::<Value>(&self.body)
            && let Some(paths) = upload["document"]["paths"].as_array()
        {
            return paths
                .iter()
                .flat_map(|p| p["steps"].as_array().cloned().unwrap_or_default())
                .filter_map(|s| s["step"]["id"].as_str().map(String::from))
                .collect();
        }
        self.body
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v["Step"]["step"]["id"].as_str().map(String::from))
            .collect()
    }

    fn is_write(&self) -> bool {
        self.method == "POST" && self.url.contains("/graphs")
    }
}

/// A plain-HTTP forward proxy that logs each request and relays it
/// upstream on a fresh connection, one request per connection.
struct Proxy {
    port: u16,
    log: Arc<Mutex<Vec<Logged>>>,
}

impl Proxy {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let shared = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = shared.clone();
                std::thread::spawn(move || relay(stream, &log));
            }
        });
        Self { port, log }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn take(&self) -> Vec<Logged> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }
}

fn relay(client: TcpStream, log: &Mutex<Vec<Logged>>) {
    let mut reader = BufReader::new(client.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut headers = Vec::new();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        let (name, value) = line
            .trim_end()
            .split_once(':')
            .unwrap_or((line.trim_end(), ""));
        if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().unwrap_or(0);
        }
        if name.eq_ignore_ascii_case("connection") || name.eq_ignore_ascii_case("proxy-connection")
        {
            continue;
        }
        headers.push(line.trim_end().to_string());
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).unwrap();

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let url = parts.next().unwrap_or_default().to_string();
    log.lock().unwrap().push(Logged {
        method: method.clone(),
        url: url.clone(),
        body: String::from_utf8_lossy(&body).into_owned(),
    });

    let rest = url
        .strip_prefix("http://")
        .expect("the proxy relays plain HTTP");
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let mut upstream = TcpStream::connect(host).unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\n");
    for h in &headers {
        head.push_str(h);
        head.push_str("\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    upstream.write_all(head.as_bytes()).unwrap();
    upstream.write_all(&body).unwrap();
    let mut response = Vec::new();
    upstream.read_to_end(&mut response).unwrap();
    // Close the client's connection after the response too.
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(response.len(), |i| i + 2);
    let (status_and_headers, rest) = response.split_at(split);
    let kept: Vec<&str> = std::str::from_utf8(status_and_headers)
        .unwrap()
        .split("\r\n")
        .filter(|l| !l.to_ascii_lowercase().starts_with("connection:"))
        .collect();
    let mut client = client;
    let _ = client.write_all(kept.join("\r\n").as_bytes());
    let _ = client.write_all(b"Connection: close\r\n");
    let _ = client.write_all(rest);
}

// ── A sandboxed CLI ─────────────────────────────────────────────────

struct Cli {
    home: tempfile::TempDir,
    config: tempfile::TempDir,
    proxy: Proxy,
    server_url: String,
    repo: String,
}

impl Cli {
    fn new(server: &Server, repo: &str) -> Self {
        let cli = Self {
            home: tempfile::tempdir().unwrap(),
            config: tempfile::tempdir().unwrap(),
            proxy: Proxy::start(),
            server_url: server.url.clone(),
            repo: format!("{}/{repo}", server.owner),
        };
        let credentials = json!({
            "url": server.url,
            "token": server.token,
            "user": {"id": "test", "username": server.owner},
        });
        std::fs::write(
            cli.config.path().join("credentials.json"),
            credentials.to_string(),
        )
        .unwrap();
        cli
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("path").unwrap();
        cmd.env("HOME", self.home.path())
            .env("TOOLPATH_CONFIG_DIR", self.config.path())
            .env("XDG_DATA_HOME", self.home.path().join(".local/share"))
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("PATHBASE_URL")
            .env("HTTP_PROXY", self.proxy.url())
            .env("http_proxy", self.proxy.url())
            .env_remove("NO_PROXY")
            .env_remove("no_proxy");
        cmd
    }

    /// One `path share --all` pass for `harness`; its stderr. Panics
    /// when the pass fails.
    fn pass(&self, harness: &str) -> String {
        let out = self
            .cmd()
            .args(["share", "--all", "--yes", "--harness", harness])
            .args(["--url", &self.server_url, "--repo", &self.repo])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "pass failed:\n{stderr}");
        stderr
    }

    /// The session's one record in `remotes.json`: key and value.
    fn record(&self, harness: &str, id: &str) -> (String, Value) {
        let json: Value = serde_json::from_str(
            &std::fs::read_to_string(self.config.path().join("remotes.json")).unwrap(),
        )
        .unwrap();
        let records = json["sessions"][harness][id].as_object().unwrap();
        assert_eq!(records.len(), 1, "one record per destination: {records:?}");
        let (k, v) = records.iter().next().unwrap();
        (k.clone(), v.clone())
    }

    /// The local derive's steps, through `path p derive`.
    fn derive(&self, args: &[&str]) -> Vec<Value> {
        let out = self
            .cmd()
            .args(["p", "derive"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "derive: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
        doc["paths"][0]["steps"].as_array().unwrap().clone()
    }
}

// ── The settle rule, stated independently ───────────────────────────

fn conversation(step: &Value) -> Option<&Value> {
    step["change"]
        .as_object()?
        .values()
        .find(|c| c["structural"]["type"] == "conversation.append")
        .map(|c| &c["structural"])
}

/// Ids of the settled steps: everything before the trailing run of the
/// last turn's message group, less trailing turns with an unanswered
/// tool call.
fn settled(steps: &[Value]) -> Vec<String> {
    let Some(last) = steps.iter().rposition(|s| conversation(s).is_some()) else {
        return Vec::new();
    };
    let group = conversation(&steps[last]).unwrap()["group_id"].as_str();
    let mut k = last;
    if group.is_some() {
        for i in (0..last).rev() {
            match conversation(&steps[i]) {
                None => continue,
                Some(c) if c["group_id"].as_str() == group => k = i,
                Some(_) => break,
            }
        }
    }
    let open = |s: &Value| {
        conversation(s)
            .and_then(|c| c["tool_uses"].as_array())
            .is_some_and(|calls| {
                calls
                    .iter()
                    .any(|c| c.get("result").is_none_or(Value::is_null))
            })
    };
    while k > 0 && open(&steps[k - 1]) {
        k -= 1;
    }
    steps[..k]
        .iter()
        .map(|s| s["step"]["id"].as_str().unwrap().to_string())
        .collect()
}

// ── Sessions on disk ────────────────────────────────────────────────

fn fixture(name: &str) -> Vec<String> {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-fixtures")
            .join(name),
    )
    .unwrap()
    .lines()
    .map(String::from)
    .collect()
}

fn write_lines(file: &Path, lines: &[String]) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, lines.join("\n") + "\n").unwrap();
}

/// A harness's session file, its lines so far, and how to extend it.
trait Session {
    fn file(&self) -> PathBuf;
    fn derive_args(&self) -> Vec<String>;
    /// A user line (and, with `reply`, the assistant's answer) after
    /// `lines`, so the turn before settles.
    fn exchange(&self, lines: &[String], n: usize, reply: bool) -> Vec<String>;
}

struct Claude {
    home: PathBuf,
}

impl Session for Claude {
    fn file(&self) -> PathBuf {
        self.home
            .join(".claude/projects/-sync-test")
            .join(format!("{CLAUDE_ID}.jsonl"))
    }

    fn derive_args(&self) -> Vec<String> {
        ["claude", "--project", "/sync/test", "--session", CLAUDE_ID]
            .map(String::from)
            .to_vec()
    }

    fn exchange(&self, lines: &[String], n: usize, reply: bool) -> Vec<String> {
        let last = lines
            .iter()
            .rev()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find_map(|v| v["uuid"].as_str().map(String::from))
            .unwrap();
        let user = format!("ffffffff-0000-0000-0000-{n:012}");
        let mut out = vec![
            json!({
                "type": "user", "uuid": user, "parentUuid": last, "sessionId": CLAUDE_ID,
                "timestamp": format!("2026-05-08T00:{:02}:00.000Z", n % 60),
                "cwd": "/sync/test",
                "message": {"role": "user", "content": format!("follow-up {n}")},
            })
            .to_string(),
        ];
        if reply {
            out.push(
                json!({
                    "type": "assistant", "uuid": format!("eeeeeeee-0000-0000-0000-{n:012}"),
                    "parentUuid": user, "sessionId": CLAUDE_ID,
                    "timestamp": format!("2026-05-08T00:{:02}:30.000Z", n % 60),
                    "message": {"id": format!("msg_followup_{n}"), "role": "assistant",
                                "content": [{"type": "text", "text": format!("answer {n}")}]},
                })
                .to_string(),
            );
        }
        out
    }
}

struct Codex {
    home: PathBuf,
}

impl Session for Codex {
    fn file(&self) -> PathBuf {
        self.home
            .join(".codex/sessions/2026/05/07")
            .join(format!("rollout-2026-05-07T18-27-42-{CODEX_ID}.jsonl"))
    }

    fn derive_args(&self) -> Vec<String> {
        ["codex", "--session", CODEX_ID].map(String::from).to_vec()
    }

    fn exchange(&self, _lines: &[String], n: usize, reply: bool) -> Vec<String> {
        let at = format!("2026-05-08T00:{:02}:00.000Z", n % 60);
        let text = format!("follow-up {n}");
        let mut out = vec![
            json!({"timestamp": at, "type": "event_msg", "payload": {"type": "task_started"}}).to_string(),
            json!({"timestamp": at, "type": "response_item", "payload": {
                "type": "message", "role": "user",
                "content": [{"type": "input_text", "text": text}]}})
            .to_string(),
            json!({"timestamp": at, "type": "event_msg", "payload": {
                "type": "user_message", "message": text, "images": [], "local_images": [], "text_elements": []}})
            .to_string(),
        ];
        if reply {
            out.push(
                json!({"timestamp": at, "type": "response_item", "payload": {
                    "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": format!("answer {n}")}]}})
                .to_string(),
            );
            out.push(
                json!({"timestamp": at, "type": "event_msg", "payload": {"type": "task_complete"}})
                    .to_string(),
            );
        }
        out
    }
}

// ── The loop ────────────────────────────────────────────────────────

/// Spec steps 1 to 5 for one harness's session.
fn run_loop(
    server: &Server,
    harness: &str,
    id: &str,
    fixture_name: &str,
    first: usize,
    grow: usize,
) {
    let repo = server.new_repo(&format!("sync-{harness}"));
    let cli = Cli::new(server, &repo);
    let session: Box<dyn Session> = match harness {
        "claude" => Box::new(Claude {
            home: cli.home.path().to_path_buf(),
        }),
        _ => Box::new(Codex {
            home: cli.home.path().to_path_buf(),
        }),
    };
    let derive_args = session.derive_args();
    let derive_args: Vec<&str> = derive_args.iter().map(String::as_str).collect();
    let all = fixture(fixture_name);

    // 1. A truncated copy, one pass: the server holds the settled derive.
    let mut lines = all[..first].to_vec();
    write_lines(&session.file(), &lines);
    let stderr = cli.pass(harness);
    let mut want = settled(&cli.derive(&derive_args));
    assert!(!want.is_empty(), "the first {first} lines settle something");
    let (mut key, record) = cli.record(harness, id);
    assert_eq!(server.step_ids(&key), want, "step 1:\n{stderr}");
    assert_eq!(record["steps"], want.len());
    assert_eq!(record["last_step"], want[want.len() - 1].as_str());
    cli.proxy.take();

    // 2 and 3. Grow, pass, until the file is complete; then a closing
    // user line settles the last group.
    let mut at = first;
    let mut closing = 0;
    let mut grew = 0;
    loop {
        let before = want.clone();
        if at < all.len() {
            at = (at + grow).min(all.len());
            lines = all[..at].to_vec();
        } else {
            closing += 1;
            lines.extend(session.exchange(&lines, closing, false));
        }
        write_lines(&session.file(), &lines);
        let stderr = cli.pass(harness);
        want = settled(&cli.derive(&derive_args));
        assert!(
            want.starts_with(&before),
            "the {harness} derive is not prefix-stable at {at} lines \
             (expected to hold once toolpath #144 is merged):\n  before: {before:?}\n  now:    {want:?}"
        );
        let sent: Vec<String> = cli
            .proxy
            .take()
            .iter()
            .filter(|r| r.is_write())
            .flat_map(|r| r.step_ids())
            .collect();
        assert_eq!(
            sent,
            want[before.len()..],
            "only the new settled steps are sent:\n{stderr}"
        );
        eprintln!(
            "{harness}: {} lines, {} settled, {} sent",
            lines.len(),
            want.len(),
            sent.len()
        );
        grew += usize::from(!sent.is_empty());
        let (k, record) = cli.record(harness, id);
        assert_eq!(k, key, "the pass appended to the same path:\n{stderr}");
        assert_eq!(server.step_ids(&key), want);
        assert_eq!(record["steps"], want.len());
        assert_eq!(record["last_step"], want[want.len() - 1].as_str());
        if closing > 0 {
            break;
        }
    }
    assert!(
        grew >= 2,
        "growth reached the server on at least two passes"
    );

    // 4. Freeze, grow, pass: a continuation of the frozen path holding
    // only the new steps, its parent the frozen path's head.
    let (_, frozen_record) = cli.record(harness, id);
    server.freeze(&key);
    let before = want.clone();
    lines.extend(session.exchange(&lines, 100, true));
    lines.extend(session.exchange(&lines, 101, false));
    write_lines(&session.file(), &lines);
    let stderr = cli.pass(harness);
    assert!(stderr.contains("continued"), "{stderr}");
    want = settled(&cli.derive(&derive_args));
    let (new_key, record) = cli.record(harness, id);
    assert_ne!(new_key, key, "the record is re-keyed to the continuation");
    assert_eq!(server.step_ids(&new_key), want[before.len()..]);
    assert_eq!(record["steps"], want.len());
    let doc = server.document(&new_key);
    let path_id = cli.derive_doc_path_id(&derive_args);
    let parent = &doc["paths"][0]["path"]["parent"];
    assert_eq!(parent["uri"], graph_page_url(&key).as_str(), "{doc}");
    assert_eq!(parent["path"], path_id.as_str());
    assert_eq!(parent["step"], frozen_record["head"]);
    key = new_key;

    // A pass with more growth appends to the continuation.
    let before = want.clone();
    lines.extend(session.exchange(&lines, 102, true));
    lines.extend(session.exchange(&lines, 103, false));
    write_lines(&session.file(), &lines);
    cli.pass(harness);
    want = settled(&cli.derive(&derive_args));
    assert_eq!(cli.record(harness, id).0, key);
    let stored: HashSet<String> = server.step_ids(&key).into_iter().collect();
    assert!(want[before.len()..].iter().all(|s| stored.contains(s)));

    // 5. Delete the graph the record points at, grow, pass: recreated.
    server.delete(&key);
    lines.extend(session.exchange(&lines, 104, true));
    lines.extend(session.exchange(&lines, 105, false));
    write_lines(&session.file(), &lines);
    let stderr = cli.pass(harness);
    assert!(stderr.contains("recreated"), "{stderr}");
    want = settled(&cli.derive(&derive_args));
    let (recreated, record) = cli.record(harness, id);
    assert_ne!(recreated, key);
    assert_eq!(server.step_ids(&recreated), want);
    assert_eq!(record["steps"], want.len());

    // A repeat pass with nothing new sends nothing.
    cli.proxy.take();
    let stderr = cli.pass(harness);
    assert!(
        cli.proxy.take().iter().all(|r| !r.is_write()),
        "a pass with nothing new writes nothing:\n{stderr}"
    );
}

impl Cli {
    fn derive_doc_path_id(&self, args: &[&str]) -> String {
        let out = self
            .cmd()
            .args(["p", "derive"])
            .args(args)
            .output()
            .unwrap();
        let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
        doc["paths"][0]["path"]["id"].as_str().unwrap().to_string()
    }
}

#[test]
fn claude_session_syncs_incrementally() {
    let Some(server) = Server::from_env() else {
        return;
    };
    run_loop(&server, "claude", CLAUDE_ID, "claude/convo.jsonl", 20, 15);
}

/// The Codex deriver became prefix-stable with toolpath #144; before
/// that merge this test failed at the prefix assertion in the loop.
#[test]
fn codex_session_syncs_incrementally() {
    let Some(server) = Server::from_env() else {
        return;
    };
    run_loop(&server, "codex", CODEX_ID, "codex/convo.jsonl", 12, 8);
}
