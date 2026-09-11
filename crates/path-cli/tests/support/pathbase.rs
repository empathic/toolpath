//! An in-process Pathbase with the sync API: enough of `/u/me`, repo
//! creation, graph create/meta/PUT/freeze/continuations/download, and
//! the anonymous endpoint for `share` and `sync` to run end to end
//! against it. Every request is logged; tests read and change the
//! graphs directly.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct GraphRecord {
    pub id: String,
    pub owner: String,
    pub repo: String,
    pub state: &'static str,
    pub generation: i64,
    pub doc: serde_json::Value,
    pub base_from: Option<String>,
    pub continuation: Option<String>,
}

impl GraphRecord {
    fn path(&self) -> &serde_json::Value {
        &self.doc["paths"][0]
    }
    pub fn path_id(&self) -> String {
        self.path()["path"]["id"].as_str().unwrap_or("").to_string()
    }
    pub fn head(&self) -> String {
        self.path()["path"]["head"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }
    pub fn step_ids(&self) -> Vec<String> {
        self.path()["steps"]
            .as_array()
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(|s| s["step"]["id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Default)]
struct State {
    base: String,
    username: String,
    sync_api: bool,
    graphs: BTreeMap<String, GraphRecord>,
    requests: Vec<String>,
}

pub struct MockPathbase {
    state: Arc<Mutex<State>>,
    base: String,
}

impl MockPathbase {
    /// A server that knows the sync API and logs the caller in as `alex`.
    pub fn start() -> Self {
        Self::start_as("alex", true)
    }

    /// `sync_api: false` is a Pathbase from before the sync API: graph
    /// meta answers a bare 404.
    pub fn start_as(username: &str, sync_api: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let state = Arc::new(Mutex::new(State {
            base: base.clone(),
            username: username.to_string(),
            sync_api,
            ..Default::default()
        }));
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let state = Arc::clone(&shared);
                std::thread::spawn(move || serve(stream, state));
            }
        });
        Self { state, base }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// `METHOD /path` for every request so far, in order.
    pub fn requests(&self) -> Vec<String> {
        self.state.lock().unwrap().requests.clone()
    }

    pub fn graphs(&self) -> Vec<GraphRecord> {
        self.state
            .lock()
            .unwrap()
            .graphs
            .values()
            .cloned()
            .collect()
    }

    pub fn graph(&self, id: &str) -> GraphRecord {
        self.state.lock().unwrap().graphs[id].clone()
    }

    /// What another client's freeze would do.
    pub fn freeze(&self, id: &str) {
        let mut state = self.state.lock().unwrap();
        let g = state.graphs.get_mut(id).unwrap();
        g.state = "frozen";
        g.generation += 1;
    }

    /// Credentials for this server, in the file `path` reads.
    pub fn write_credentials(&self, config_dir: &std::path::Path) {
        let username = self.state.lock().unwrap().username.clone();
        std::fs::create_dir_all(config_dir).unwrap();
        std::fs::write(
            config_dir.join("credentials.json"),
            format!(
                r#"{{"url":"{}","token":"tok","user":{{"id":"u-1","username":"{username}"}}}}"#,
                self.base
            ),
        )
        .unwrap();
    }
}

fn serve(stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut start = String::new();
        if reader.read_line(&mut start).unwrap_or(0) == 0 {
            return;
        }
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.trim_end().split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut body).unwrap();
        }
        let mut parts = start.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("").to_string();
        let path = target.split('?').next().unwrap_or("").to_string();
        let (status, content_type, response) = {
            let mut state = state.lock().unwrap();
            state.requests.push(format!("{method} {path}"));
            route(&mut state, &method, &path, &body)
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{response}",
            response.len()
        );
        if reader.get_mut().write_all(response.as_bytes()).is_err() {
            return;
        }
    }
}

const JSON: &str = "application/json";

fn error(status: &'static str, code: &str, message: &str) -> (&'static str, &'static str, String) {
    (
        status,
        JSON,
        serde_json::json!({ "code": code, "error": message }).to_string(),
    )
}

fn route(
    state: &mut State,
    method: &str,
    path: &str,
    body: &[u8],
) -> (&'static str, &'static str, String) {
    let Some(rest) = path.strip_prefix("/api/v1/u/") else {
        return error("404 Not Found", "not_found", "no such route");
    };
    let segments: Vec<&str> = rest.split('/').collect();
    match (method, segments.as_slice()) {
        ("GET", ["me"]) => (
            "200 OK",
            JSON,
            serde_json::json!({
                "id": "fe94b6f9-b0af-4cdd-b9ca-3c9a2a697537",
                "username": state.username,
                "email": null, "display_name": null, "bio": null,
                "created_at": "2024-01-01T00:00:00Z", "updated_at": "2024-01-01T00:00:00Z"
            })
            .to_string(),
        ),
        ("POST", [_owner, "repos"]) => error("409 Conflict", "conflict", "repo exists"),
        ("POST", [owner, "repos", repo, "graphs"]) => {
            let body: serde_json::Value = serde_json::from_slice(body).unwrap();
            let frozen = body["freeze_after"].as_bool().unwrap_or(false) || *owner == "anon";
            let record = insert(state, owner, repo, body["document"].clone(), frozen);
            (
                "201 Created",
                JSON,
                serde_json::json!({
                    "id": record.id,
                    "repo_id": "00000000-0000-0000-0000-000000000002",
                    "toolpath_id": "tp-1",
                    "document": {"graph": {"id": "g"}, "paths": []},
                    "path_count": 1,
                    "url": graph_url(&state.base, &record),
                    "visibility": body["visibility"].as_str().unwrap_or("unlisted"),
                    "state": record.state,
                    "generation": record.generation,
                    "created_at": "2024-01-01T00:00:00Z",
                    "updated_at": "2024-01-01T00:00:00Z"
                })
                .to_string(),
            )
        }
        ("GET", [_owner, "repos", _repo, "graphs", id, "meta"]) => {
            if !state.sync_api {
                return ("404 Not Found", "text/plain", "not found".to_string());
            }
            match state.graphs.get(*id) {
                Some(g) => ("200 OK", JSON, meta(&state.base, g).to_string()),
                None => error("404 Not Found", "not_found", "no such graph"),
            }
        }
        ("GET", [_owner, "repos", _repo, "graphs", id, "download"]) => {
            match state.graphs.get(*id) {
                Some(g) => ("200 OK", JSON, g.doc.to_string()),
                None => error("404 Not Found", "not_found", "no such graph"),
            }
        }
        ("PUT", [_owner, "repos", _repo, "graphs", id]) => {
            let body: serde_json::Value = serde_json::from_slice(body).unwrap();
            let base = state.base.clone();
            let Some(g) = state.graphs.get_mut(*id) else {
                return error("404 Not Found", "not_found", "no such graph");
            };
            if g.state == "frozen" {
                return error("409 Conflict", "frozen", "graph is frozen");
            }
            if body["expected_generation"].as_i64() != Some(g.generation) {
                return error("409 Conflict", "generation_conflict", "stale generation");
            }
            let (ids, head) = (g.step_ids(), g.head());
            g.doc = body["document"].clone();
            if g.step_ids() != ids || g.head() != head {
                g.generation += 1;
            }
            if body["freeze_after"].as_bool().unwrap_or(false) {
                g.state = "frozen";
                g.generation += 1;
            }
            ("200 OK", JSON, meta(&base, g).to_string())
        }
        ("POST", [_owner, "repos", _repo, "graphs", id, "freeze"]) => {
            let base = state.base.clone();
            let Some(g) = state.graphs.get_mut(*id) else {
                return error("404 Not Found", "not_found", "no such graph");
            };
            if g.state == "mutable" {
                g.state = "frozen";
                g.generation += 1;
            }
            ("200 OK", JSON, meta(&base, g).to_string())
        }
        ("POST", [owner, "repos", repo, "graphs", id, "continuations"]) => {
            let body: serde_json::Value = serde_json::from_slice(body).unwrap();
            let Some(source) = state.graphs.get(*id).cloned() else {
                return error("404 Not Found", "not_found", "no such graph");
            };
            if source.state != "frozen" {
                return error("409 Conflict", "source_not_frozen", "source is mutable");
            }
            if let Some(existing) = &source.continuation {
                let g = state.graphs[existing].clone();
                return ("200 OK", JSON, meta(&state.base, &g).to_string());
            }
            let frozen = body["freeze_after"].as_bool().unwrap_or(false);
            let record = insert(state, owner, repo, body["document"].clone(), frozen);
            state.graphs.get_mut(*id).unwrap().continuation = Some(record.id.clone());
            ("201 Created", JSON, meta(&state.base, &record).to_string())
        }
        _ => error("404 Not Found", "not_found", "no such route"),
    }
}

fn insert(
    state: &mut State,
    owner: &str,
    repo: &str,
    doc: serde_json::Value,
    frozen: bool,
) -> GraphRecord {
    let n = state.graphs.len() as u128 + 1;
    let record = GraphRecord {
        id: uuid::Uuid::from_u128(0x1000_0000_0000_4000_8000_0000_0000_0000 + n).to_string(),
        owner: owner.to_string(),
        repo: repo.to_string(),
        state: if frozen { "frozen" } else { "mutable" },
        generation: i64::from(frozen),
        base_from: doc["paths"][0]["path"]["base"]["from"]
            .as_str()
            .map(str::to_string),
        continuation: None,
        doc,
    };
    state.graphs.insert(record.id.clone(), record.clone());
    record
}

fn graph_url(base: &str, g: &GraphRecord) -> String {
    format!("{base}/u/{}/{}/graphs/{}", g.owner, g.repo, g.id)
}

fn meta(base: &str, g: &GraphRecord) -> serde_json::Value {
    let source_graph_id = g
        .base_from
        .as_deref()
        .and_then(|from| from.split('#').next())
        .and_then(|url| url.rsplit('/').next())
        .unwrap_or("00000000-0000-0000-0000-000000000000")
        .to_string();
    serde_json::json!({
        "id": g.id,
        "url": graph_url(base, g),
        "state": g.state,
        "generation": g.generation,
        "updated_at": "2024-01-01T00:00:00Z",
        "base": g.base_from.as_ref().map(|from| serde_json::json!({
            "from": from,
            "source_graph_id": source_graph_id,
            "source_path_id": from.rsplit_once('#').and_then(|(_, r)| r.split('/').next()).unwrap_or(""),
            "source_step_id": from.rsplit('/').next().unwrap_or(""),
        })),
        "lineage": {
            "continuation_graph_id": g.continuation,
            "source_graph_id": g.base_from.as_ref().map(|_| source_graph_id.clone()),
            "source_path": null
        },
        "paths": [{
            "id": g.path_id(),
            "server_id": "00000000-0000-0000-0000-000000000003",
            "head": g.head(),
            "step_count": g.step_ids().len()
        }]
    })
}
