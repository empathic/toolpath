//! An in-memory Pathbase for tests of the sync client: the routes
//! `path share` writes through, keeping graphs, paths and steps between
//! requests so a test can run several passes against one server, freeze
//! or delete a graph between them, and read back what was stored and
//! every request that stored it.
//!
//! It models the contract the client relies on, not the real server:
//! a step is stored once (an identical resend is skipped, a different
//! payload under a stored id is `400 invalid_document`), every parent
//! must be stored or earlier in the request (`400 invalid_document`), a
//! frozen graph refuses writes (`409 write_to_frozen`), a missing graph
//! is `404 not_found`, and a continuation may not own a step stored
//! along its parent chain (`400 invalid_document`).

#![cfg(test)]

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use toolpath::v1::{Parent, Step};
use uuid::Uuid;

/// One request the fake answered.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub(crate) method: String,
    /// Path and query, as sent.
    pub(crate) target: String,
    pub(crate) body: String,
}

impl Request {
    /// The ids of the `Step` lines of a JSONL body, or of the steps of a
    /// `POST /graphs` document.
    pub(crate) fn step_ids(&self) -> Vec<String> {
        if let Ok(upload) = serde_json::from_str::<serde_json::Value>(&self.body)
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
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v["Step"]["step"]["id"].as_str().map(String::from))
            .collect()
    }

    /// The `Head` lines of a JSONL body.
    pub(crate) fn heads(&self) -> Vec<String> {
        self.body
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v["Head"]["step_id"].as_str().map(String::from))
            .collect()
    }

    /// Whether this request wrote steps or created a graph.
    pub(crate) fn is_write(&self) -> bool {
        self.method == "POST" && self.target.contains("/graphs")
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FakePath {
    pub(crate) server_id: Uuid,
    /// The toolpath `path.id`.
    pub(crate) id: String,
    /// `(id, comparable payload)` in the order stored.
    steps: Vec<(String, serde_json::Value)>,
    pub(crate) head: Option<String>,
    pub(crate) parent: Option<Parent>,
}

impl FakePath {
    pub(crate) fn step_ids(&self) -> Vec<String> {
        self.steps.iter().map(|(id, _)| id.clone()).collect()
    }

    fn holds(&self, id: &str) -> bool {
        self.steps.iter().any(|(s, _)| s == id)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FakeGraph {
    pub(crate) id: Uuid,
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) frozen: bool,
    pub(crate) paths: Vec<FakePath>,
}

/// A response the test forces on requests whose target contains a
/// substring, ahead of the fake's own handling.
struct Forced {
    method: String,
    target_contains: String,
    status: u16,
    body: String,
}

#[derive(Default)]
struct State {
    graphs: Vec<FakeGraph>,
    log: Vec<Request>,
    forced: Vec<Forced>,
}

pub(crate) struct FakePathbase {
    port: u16,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

type Response = (u16, String);

fn api_error(status: u16, code: &str, error: &str) -> Response {
    (
        status,
        serde_json::json!({"code": code, "error": error}).to_string(),
    )
}

/// What the fake compares to tell "the same step" from "a different
/// step under that id": everything but signatures, which ride their own
/// lines in JSONL.
fn payload(step: &Step) -> serde_json::Value {
    serde_json::json!({"step": step.step, "change": step.change})
}

impl FakePathbase {
    pub(crate) fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let state = state.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        serve(stream, port, &state);
                    }
                }
            })
        };
        Self {
            port,
            state,
            stop,
            thread: Some(thread),
        }
    }

    pub(crate) fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub(crate) fn requests(&self) -> Vec<Request> {
        self.state.lock().unwrap().log.clone()
    }

    /// The requests that wrote steps or created a graph.
    pub(crate) fn writes(&self) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|r| r.is_write())
            .collect()
    }

    pub(crate) fn clear_log(&self) {
        self.state.lock().unwrap().log.clear();
    }

    pub(crate) fn graphs(&self) -> Vec<FakeGraph> {
        self.state.lock().unwrap().graphs.clone()
    }

    pub(crate) fn graph(&self, id: Uuid) -> Option<FakeGraph> {
        self.graphs().into_iter().find(|g| g.id == id)
    }

    /// The stored path at a record key (a path API URL).
    pub(crate) fn path_at(&self, url: &str) -> Option<FakePath> {
        let (graph, path) = ids_of(url)?;
        self.graph(graph)?
            .paths
            .into_iter()
            .find(|p| p.server_id == path)
    }

    pub(crate) fn freeze(&self, graph: Uuid) {
        let mut state = self.state.lock().unwrap();
        state
            .graphs
            .iter_mut()
            .find(|g| g.id == graph)
            .unwrap()
            .frozen = true;
    }

    pub(crate) fn delete(&self, graph: Uuid) {
        self.state.lock().unwrap().graphs.retain(|g| g.id != graph);
    }

    /// Answer every `method` request whose target contains `target`
    /// with `status` and `body` from now on.
    pub(crate) fn force(&self, method: &str, target: &str, status: u16, body: &str) {
        self.state.lock().unwrap().forced.push(Forced {
            method: method.to_string(),
            target_contains: target.to_string(),
            status,
            body: body.to_string(),
        });
    }
}

impl Drop for FakePathbase {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// `(graph, path)` server ids of a path API URL.
pub(crate) fn ids_of(url: &str) -> Option<(Uuid, Uuid)> {
    let segs: Vec<&str> = url.rsplitn(5, '/').collect();
    match segs.as_slice() {
        [path, "paths", graph, "graphs", _] => Some((graph.parse().ok()?, path.parse().ok()?)),
        _ => None,
    }
}

fn serve(stream: TcpStream, port: u16, state: &Mutex<State>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut content_length = 0;
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
    reader.read_exact(&mut body).ok();
    let mut parts = request_line.split_whitespace();
    let request = Request {
        method: parts.next().unwrap_or_default().to_string(),
        target: parts.next().unwrap_or_default().to_string(),
        body: String::from_utf8_lossy(&body).into_owned(),
    };

    let (status, body) = {
        let mut state = state.lock().unwrap();
        state.log.push(request.clone());
        let forced = state
            .forced
            .iter()
            .find(|f| f.method == request.method && request.target.contains(&f.target_contains));
        match forced {
            Some(f) => (f.status, f.body.clone()),
            None => route(&mut state, port, &request),
        }
    };
    let response = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = stream;
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn route(state: &mut State, port: u16, req: &Request) -> Response {
    let (path, query) = req
        .target
        .split_once('?')
        .unwrap_or((req.target.as_str(), ""));
    let segs: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["api", "v1", "u", "me"]) => {
            (200, crate::cmd_pathbase::tests::me_response_body("alex"))
        }
        ("POST", ["api", "v1", "u", _, "repos"]) => {
            api_error(409, "conflict", "repo already exists")
        }
        ("POST", ["api", "v1", "u", owner, "repos", repo, "graphs"]) => {
            create_graph(state, port, owner, repo, &req.body)
        }
        ("DELETE", ["api", "v1", "u", _, "repos", _, "graphs", g]) => {
            let Ok(g) = g.parse::<Uuid>() else {
                return api_error(400, "bad_request", "bad id");
            };
            state.graphs.retain(|graph| graph.id != g);
            (204, String::new())
        }
        ("POST", ["api", "v1", "u", _, "repos", _, "graphs", g, "paths"]) => {
            with_graph(state, g, |state, gi| open_path(state, port, gi, &req.body))
        }
        (
            "POST",
            [
                "api",
                "v1",
                "u",
                _,
                "repos",
                _,
                "graphs",
                g,
                "paths",
                p,
                "steps",
            ],
        ) => with_graph(state, g, |state, gi| {
            let Some(pi) = path_index(state, gi, p) else {
                return api_error(404, "not_found", "no such path");
            };
            apply(state, port, gi, pi, &req.body, false)
        }),
        (
            "GET",
            [
                "api",
                "v1",
                "u",
                _,
                "repos",
                _,
                "graphs",
                g,
                "paths",
                p,
                "step-ids",
            ],
        ) => with_graph(state, g, |state, gi| {
            let Some(pi) = path_index(state, gi, p) else {
                return api_error(404, "not_found", "no such path");
            };
            let mut ids = state.graphs[gi].paths[pi].step_ids();
            if query.contains("through_parents=true") {
                ids.extend(chain_ids(
                    state,
                    port,
                    state.graphs[gi].paths[pi].parent.as_ref(),
                ));
            }
            (200, serde_json::json!({"step_ids": ids}).to_string())
        }),
        _ => api_error(404, "not_found", "no such route"),
    }
}

fn with_graph(
    state: &mut State,
    g: &str,
    f: impl FnOnce(&mut State, usize) -> Response,
) -> Response {
    let gi = g
        .parse::<Uuid>()
        .ok()
        .and_then(|g| state.graphs.iter().position(|graph| graph.id == g));
    match gi {
        Some(gi) => f(state, gi),
        None => api_error(404, "not_found", "no such graph"),
    }
}

fn path_index(state: &State, gi: usize, p: &str) -> Option<usize> {
    let p = p.parse::<Uuid>().ok()?;
    state.graphs[gi]
        .paths
        .iter()
        .position(|path| path.server_id == p)
}

/// The ids stored along `parent`'s chain on this host, nearest first.
fn chain_ids<'a>(state: &'a State, port: u16, mut parent: Option<&'a Parent>) -> Vec<String> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    while let Some(p) = parent {
        let Some(path) = resolve(state, port, p) else {
            break;
        };
        if !seen.insert(path.server_id) {
            break;
        }
        ids.extend(path.step_ids());
        parent = path.parent.as_ref();
    }
    ids
}

/// The path a parent names, when its document is a graph on this host.
fn resolve<'a>(state: &'a State, port: u16, parent: &Parent) -> Option<&'a FakePath> {
    let prefix = format!("http://127.0.0.1:{port}/u/");
    let rest = parent.uri.strip_prefix(&prefix)?;
    let segs: Vec<&str> = rest.split('/').collect();
    let [_, _, "graphs", g] = segs.as_slice() else {
        return None;
    };
    let g: Uuid = g.parse().ok()?;
    state
        .graphs
        .iter()
        .find(|graph| graph.id == g)?
        .paths
        .iter()
        .find(|path| path.id == parent.path)
}

fn create_graph(state: &mut State, port: u16, owner: &str, repo: &str, body: &str) -> Response {
    let upload: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return api_error(400, "bad_request", &e.to_string()),
    };
    let doc = match toolpath::v1::Graph::from_json(&upload["document"].to_string()) {
        Ok(doc) => doc,
        Err(e) => return api_error(400, "invalid_document", &e.to_string()),
    };
    let id = Uuid::new_v4();
    state.graphs.push(FakeGraph {
        id,
        owner: owner.to_string(),
        repo: repo.to_string(),
        frozen: false,
        paths: Vec::new(),
    });
    let gi = state.graphs.len() - 1;
    let mut created = Vec::new();
    for p in &doc.paths {
        let toolpath::v1::PathOrRef::Path(path) = p else {
            continue;
        };
        let jsonl = path.to_jsonl_string().unwrap();
        let (status, response) = open_path(state, port, gi, &jsonl);
        if status >= 300 {
            state.graphs.remove(gi);
            return (status, response);
        }
        let server_id = state.graphs[gi].paths.last().unwrap().server_id;
        created.push(serde_json::json!({"id": path.path.id, "server_id": server_id}));
    }
    let mut response: serde_json::Value =
        serde_json::from_str(&crate::cmd_pathbase::tests::graph_document_json()).unwrap();
    response["id"] = serde_json::json!(id);
    response["url"] = serde_json::json!(format!(
        "http://127.0.0.1:{port}/u/{owner}/{repo}/graphs/{id}"
    ));
    response["document"] = serde_json::json!({});
    response["path_count"] = serde_json::json!(created.len());
    response["paths"] = serde_json::Value::Array(created);
    (201, response.to_string())
}

/// `POST …/paths`: the first line is a `PathOpen`; a path with that id
/// already in the graph takes the rest of the lines.
fn open_path(state: &mut State, port: u16, gi: usize, body: &str) -> Response {
    if state.graphs[gi].frozen {
        return api_error(409, "write_to_frozen", "the graph is frozen");
    }
    let Some(first) = body.lines().next() else {
        return api_error(400, "bad_request", "empty body");
    };
    let open: serde_json::Value = serde_json::from_str(first).unwrap_or_default();
    let Some(open) = open.get("PathOpen") else {
        return api_error(400, "bad_request", "the first line is not a PathOpen");
    };
    let id = open["id"].as_str().unwrap_or_default().to_string();
    let parent: Option<Parent> = open
        .get("parent")
        .and_then(|p| serde_json::from_value(p.clone()).ok());
    let pi = match state.graphs[gi].paths.iter().position(|p| p.id == id) {
        Some(pi) => pi,
        None => {
            state.graphs[gi].paths.push(FakePath {
                server_id: Uuid::new_v4(),
                id,
                steps: Vec::new(),
                head: None,
                parent,
            });
            state.graphs[gi].paths.len() - 1
        }
    };
    let (status, mut response) = apply(state, port, gi, pi, body, true);
    if status < 300 {
        let mut v: serde_json::Value = serde_json::from_str(&response).unwrap();
        v["path_id"] = serde_json::json!(state.graphs[gi].paths[pi].server_id);
        response = v.to_string();
        return (201, response);
    }
    if state.graphs[gi].paths[pi].steps.is_empty() {
        state.graphs[gi].paths.remove(pi);
    }
    (status, response)
}

/// Apply JSONL `body` to path `pi` all or nothing.
fn apply(
    state: &mut State,
    port: u16,
    gi: usize,
    pi: usize,
    body: &str,
    opening: bool,
) -> Response {
    let upstream = chain_ids(state, port, state.graphs[gi].paths[pi].parent.as_ref());
    if state.graphs[gi].frozen {
        return api_error(409, "write_to_frozen", "the graph is frozen");
    }
    let path = &state.graphs[gi].paths[pi];
    let mut new_steps: Vec<(String, serde_json::Value)> = Vec::new();
    let mut head = None;
    for (n, line) in body.lines().enumerate() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                return api_error(400, "bad_request", &format!("line {} is malformed", n + 1));
            }
        };
        if v.get("PathOpen").is_some() {
            if opening && n == 0 {
                continue;
            }
            return api_error(400, "bad_request", "unexpected PathOpen");
        }
        if let Some(h) = v.get("Head") {
            head = h["step_id"].as_str().map(String::from);
            continue;
        }
        let Some(step) = v.get("Step") else {
            continue;
        };
        let step: Step = match serde_json::from_value(step.clone()) {
            Ok(s) => s,
            Err(e) => return api_error(400, "bad_request", &e.to_string()),
        };
        let id = step.step.id.clone();
        let known = |s: &str| path.holds(s) || new_steps.iter().any(|(n, _)| n == s);
        if let Some((_, stored)) = path.steps.iter().find(|(s, _)| *s == id) {
            if *stored != payload(&step) {
                return api_error(
                    400,
                    "invalid_document",
                    &format!("step {id} differs from the stored one"),
                );
            }
            continue;
        }
        if upstream.contains(&id) {
            return api_error(
                400,
                "invalid_document",
                &format!("step {id} is stored along the parent chain"),
            );
        }
        if let Some(p) = step.step.parents.iter().find(|p| !known(p)) {
            return api_error(
                400,
                "invalid_document",
                &format!("step {id} cites unstored parent {p}"),
            );
        }
        new_steps.push((id, payload(&step)));
    }
    let inserted = new_steps.len();
    let path = &mut state.graphs[gi].paths[pi];
    path.steps.extend(new_steps);
    if head.is_some() {
        path.head = head;
    }
    (
        200,
        serde_json::json!({"inserted": inserted, "head": path.head}).to_string(),
    )
}
