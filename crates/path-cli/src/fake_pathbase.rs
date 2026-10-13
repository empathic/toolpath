//! An in-memory Pathbase for tests of the sync client: the routes
//! `path share` writes through, keeping graphs, paths and steps between
//! requests so a test can run several passes against one server, freeze
//! or delete a graph between them, and read back what was stored and
//! every request that stored it.
//!
//! It answers as Pathbase does wherever the client's tests depend on the
//! answer: a step is stored once (an identical resend, or an identical
//! repeat within one streamed body, is skipped; a different payload
//! under a stored id, signatures aside, is `400 invalid_document`), a
//! `POST /graphs` document that repeats a step id is `400
//! invalid_document`, every parent must be stored in the path or earlier
//! in the request (`400 invalid_document`), a frozen graph refuses writes
//! (`409 write_to_frozen`), a missing graph or path is `404 not_found`, a
//! step stored along the parent chain is `400 inherited_step_redefined`,
//! a parent on this host must name a readable frozen graph, a member path
//! and a step it owns (`400 invalid_parent`, `409 parent_not_frozen`)
//! while one on another host is stored unresolved, the step-ids walk
//! stops at a parent the caller cannot read and names it in
//! `truncated_at`, and a route it does not have is answered `200` with an
//! HTML page, as a release build's web app answers it.

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
    /// The caller may not read it: a parent chain stops here.
    pub(crate) unreadable: bool,
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
    /// Answer `POST /graphs` without `paths`, as a server that predates
    /// the field does.
    unlisted_paths: bool,
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

/// The page a release Pathbase serves for a route it does not have.
const WEB_APP_PAGE: &str = "<!doctype html><html><body>Pathbase</body></html>";

/// What the fake compares to tell "the same step" from "a different
/// step under that id": the whole step less its signatures, which ride
/// their own lines in JSONL.
fn payload(step: &Step) -> serde_json::Value {
    let mut value = serde_json::to_value(step).expect("a step serializes");
    if let Some(meta) = value.get_mut("meta").and_then(|m| m.as_object_mut()) {
        meta.remove("signatures");
    }
    value
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

    /// Make `graph` unreadable to the caller.
    pub(crate) fn hide(&self, graph: Uuid) {
        let mut state = self.state.lock().unwrap();
        state
            .graphs
            .iter_mut()
            .find(|g| g.id == graph)
            .unwrap()
            .unreadable = true;
    }

    /// Answer `POST /graphs` without listing the stored paths from now
    /// on.
    pub(crate) fn unlist_created_paths(&self) {
        self.state.lock().unwrap().unlisted_paths = true;
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
    let content_type = if body.starts_with('<') {
        "text/html"
    } else {
        "application/json"
    };
    let response = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n{body}",
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
        ("GET", ["api", "v1", "u", owner, "repos", repo, "graphs", g, "meta"]) => {
            with_graph(state, g, |state, gi| {
                let graph = &state.graphs[gi];
                let body = serde_json::json!({
                    "id": graph.id,
                    "url": page_url(port, owner, repo, graph.id),
                    "mutability": if graph.frozen { "frozen" } else { "mutable" },
                });
                (200, body.to_string())
            })
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
            let path = &state.graphs[gi].paths[pi];
            let mut ids = path.step_ids();
            let mut body = serde_json::json!({});
            if query.contains("through_parents=true") {
                let chain = walk(state, port, path.parent.as_ref(), true);
                ids.extend(chain.ids);
                if let Some(at) = chain.truncated_at {
                    body["truncated_at"] = serde_json::json!(at);
                }
            }
            body["step_ids"] = serde_json::json!(ids);
            (200, body.to_string())
        }),
        _ => (200, WEB_APP_PAGE.to_string()),
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

/// What a walk up a parent chain found: the ids stored along it on this
/// host, nearest first, and the parent it stopped at for being
/// unreadable.
struct Chain {
    ids: Vec<String>,
    truncated_at: Option<Parent>,
}

/// Walk the chain from `parent`. With `readable_only`, as a read does,
/// stop at a parent the caller cannot read; otherwise, as the owned-step
/// check does, every parent on this host counts.
fn walk<'a>(
    state: &'a State,
    port: u16,
    mut parent: Option<&'a Parent>,
    readable_only: bool,
) -> Chain {
    let mut chain = Chain {
        ids: Vec::new(),
        truncated_at: None,
    };
    let mut seen = HashSet::new();
    while let Some(p) = parent {
        let Some((graph, path)) = resolve(state, port, p) else {
            break;
        };
        if readable_only && graph.unreadable {
            chain.truncated_at = Some(p.clone());
            break;
        }
        if !seen.insert(path.server_id) {
            break;
        }
        chain.ids.extend(path.step_ids());
        parent = path.parent.as_ref();
    }
    chain
}

/// The graph page URL, which a child path's `parent.uri` names.
fn page_url(port: u16, owner: &str, repo: &str, graph: Uuid) -> String {
    format!("http://127.0.0.1:{port}/u/{owner}/{repo}/graphs/{graph}")
}

/// Where a parent `uri` on this host points: `Some(owner, repo, graph)`,
/// or `None` for a malformed one. `Err` for another host's.
fn parse_parent_uri(port: u16, uri: &str) -> Result<Option<(String, String, Uuid)>, ()> {
    let Some(rest) = uri.strip_prefix(&format!("http://127.0.0.1:{port}/")) else {
        return Err(());
    };
    let segs: Vec<&str> = rest.split('/').collect();
    Ok(match segs.as_slice() {
        ["u", owner, repo, "graphs", g] => g
            .parse()
            .ok()
            .map(|g| (owner.to_string(), repo.to_string(), g)),
        _ => None,
    })
}

/// The graph and path a parent names, when it resolves on this host.
fn resolve<'a>(
    state: &'a State,
    port: u16,
    parent: &Parent,
) -> Option<(&'a FakeGraph, &'a FakePath)> {
    let (owner, repo, g) = parse_parent_uri(port, &parent.uri).ok()??;
    let graph = state
        .graphs
        .iter()
        .find(|graph| graph.id == g && graph.owner == owner && graph.repo == repo)?;
    let path = graph.paths.iter().find(|path| path.id == parent.path)?;
    Some((graph, path))
}

/// Refuse a parent on this host that does not name a readable frozen
/// graph, a member path and a step it owns. Another host's is fine.
fn check_parent(state: &State, port: u16, parent: &Parent) -> Result<(), Response> {
    let invalid = |why: &str| api_error(400, "invalid_parent", why);
    match parse_parent_uri(port, &parent.uri) {
        Err(()) => return Ok(()),
        Ok(None) => return Err(invalid("does not name a graph on this host")),
        Ok(Some(_)) => {}
    }
    let Some((graph, path)) = resolve(state, port, parent) else {
        return Err(invalid("no readable graph holds that path"));
    };
    if graph.unreadable {
        return Err(invalid("no readable graph holds that path"));
    }
    if !graph.frozen {
        return Err(api_error(
            409,
            "parent_not_frozen",
            "the parent is not frozen",
        ));
    }
    if !path.holds(&parent.step) {
        return Err(invalid("the parent path does not own that step"));
    }
    Ok(())
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
    for p in &doc.paths {
        let toolpath::v1::PathOrRef::Path(path) = p else {
            continue;
        };
        let mut ids = HashSet::new();
        if let Some(dup) = path.steps.iter().find(|s| !ids.insert(&s.step.id)) {
            return api_error(
                400,
                "invalid_document",
                &format!("step id {} appears twice", dup.step.id),
            );
        }
    }
    let id = Uuid::new_v4();
    state.graphs.push(FakeGraph {
        id,
        owner: owner.to_string(),
        repo: repo.to_string(),
        frozen: false,
        unreadable: false,
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
    response["url"] = serde_json::json!(page_url(port, owner, repo, id));
    response["document"] = serde_json::json!({});
    response["path_count"] = serde_json::json!(created.len());
    if !state.unlisted_paths {
        response["paths"] = serde_json::Value::Array(created);
    }
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
        Some(pi) if state.graphs[gi].paths[pi].parent != parent => {
            return api_error(409, "conflict", "the path is open with another parent");
        }
        Some(pi) => pi,
        None => {
            if let Some(p) = &parent
                && let Err(refusal) = check_parent(state, port, p)
            {
                return refusal;
            }
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
    let upstream = walk(
        state,
        port,
        state.graphs[gi].paths[pi].parent.as_ref(),
        false,
    )
    .ids;
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
        if upstream.contains(&id) {
            return api_error(
                400,
                "inherited_step_redefined",
                &format!("step {id} is stored along the parent chain"),
            );
        }
        let earlier = path.steps.iter().chain(&new_steps).find(|(s, _)| *s == id);
        if let Some((_, stored)) = earlier {
            if *stored != payload(&step) {
                return api_error(
                    400,
                    "invalid_document",
                    &format!("step {id} differs from the stored one"),
                );
            }
            continue;
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

/// The fake's answers, checked through the client calls the sync client
/// makes, so a test that passes against it passes for a reason Pathbase
/// shares.
mod tests {
    use super::*;
    use crate::cmd_pathbase::{
        AppendError, append_steps, create_graph, graph_url, path_step_ids, refusal_code,
        remote_path_url,
    };
    use toolpath::v1::{Graph, Path, PathIdentity, StepIdentity, StepMeta};

    fn step(id: &str, parent: Option<&str>) -> Step {
        Step {
            step: StepIdentity {
                id: id.into(),
                parents: parent.map(|p| vec![p.to_string()]).unwrap_or_default(),
                actor: "agent:claude".into(),
                timestamp: "2026-01-01T00:00:00Z".into(),
            },
            change: Default::default(),
            meta: None,
        }
    }

    fn path(steps: Vec<Step>, parent: Option<Parent>) -> Path {
        Path {
            path: PathIdentity {
                id: "p".into(),
                base: None,
                head: steps.last().unwrap().step.id.clone(),
                graph_ref: None,
                parent,
            },
            steps,
            meta: None,
        }
    }

    /// Create a graph of `path`; its stored path's API URL.
    fn create(fake: &FakePathbase, path: Path) -> anyhow::Result<String> {
        let doc = Graph::from_path(path);
        let created = create_graph(
            &fake.base(),
            "tok",
            "alex",
            "x",
            None,
            &doc,
            &doc.to_json().unwrap(),
            false,
        )?;
        Ok(remote_path_url(
            &fake.base(),
            "alex",
            "x",
            &created.id,
            &created.paths[0].server_id,
        ))
    }

    fn append(fake: &FakePathbase, url: &str, steps: Vec<Step>) -> Result<(), AppendError> {
        let at = path(steps.clone(), None);
        append_steps(&fake.base(), "tok", url, &at, steps, "h", 1 << 20)
    }

    #[test]
    fn a_step_is_stored_once_and_never_changed() {
        let fake = FakePathbase::start();
        let err = create(&fake, path(vec![step("a", None), step("a", None)], None)).unwrap_err();
        assert_eq!(refusal_code(&err), Some("invalid_document"), "{err:#}");

        let url = create(&fake, path(vec![step("a", None)], None)).unwrap();
        let mut changed = step("a", None);
        changed.meta = Some(StepMeta {
            intent: Some("other".into()),
            ..Default::default()
        });
        assert!(matches!(
            append(&fake, &url, vec![changed]),
            Err(AppendError::InvalidDocument(_))
        ));
        append(
            &fake,
            &url,
            vec![step("a", None), step("b", Some("a")), step("b", Some("a"))],
        )
        .unwrap();
        assert_eq!(fake.path_at(&url).unwrap().step_ids(), ["a", "b"]);
        assert!(matches!(
            append(
                &fake,
                &url,
                vec![step("c", Some("b")), step("c", Some("a"))]
            ),
            Err(AppendError::InvalidDocument(_))
        ));
        assert!(matches!(
            append(&fake, &url, vec![step("d", Some("elsewhere"))]),
            Err(AppendError::InvalidDocument(_))
        ));
    }

    #[test]
    fn a_parent_on_this_host_must_resolve_and_one_elsewhere_is_kept() {
        let fake = FakePathbase::start();
        let url = create(
            &fake,
            path(vec![step("a", None), step("b", Some("a"))], None),
        )
        .unwrap();
        let (graph, _) = ids_of(&url).unwrap();
        let page = graph_url(&fake.base(), "tok", &url).unwrap();
        let parent = |uri: &str, step: &str| Parent {
            uri: uri.into(),
            path: "p".into(),
            step: step.into(),
        };
        let child = |p: Parent| path(vec![step("c", None)], Some(p));

        let err = create(&fake, child(parent(&page, "b"))).unwrap_err();
        assert_eq!(refusal_code(&err), Some("parent_not_frozen"), "{err:#}");
        fake.freeze(graph);
        for wrong in [
            parent(&page, "zz"),
            parent(&format!("{}/u/alex/x/nowhere", fake.base()), "b"),
            parent(
                &format!("{}/u/alex/x/graphs/{}", fake.base(), Uuid::new_v4()),
                "b",
            ),
        ] {
            let err = create(&fake, child(wrong)).unwrap_err();
            assert_eq!(refusal_code(&err), Some("invalid_parent"), "{err:#}");
        }
        let err = create(&fake, path(vec![step("a", None)], Some(parent(&page, "b")))).unwrap_err();
        assert_eq!(
            refusal_code(&err),
            Some("inherited_step_redefined"),
            "{err:#}"
        );

        let foreign = create(
            &fake,
            child(parent("https://elsewhere.dev/u/a/b/graphs/g", "b")),
        );
        assert!(foreign.is_ok());

        let continued = create(&fake, child(parent(&page, "b"))).unwrap();
        let read = path_step_ids(&fake.base(), "tok", &continued, true).unwrap();
        assert_eq!(read.step_ids, ["c", "a", "b"]);
        assert_eq!(read.truncated_at, None);
        fake.hide(graph);
        let read = path_step_ids(&fake.base(), "tok", &continued, true).unwrap();
        assert_eq!(read.step_ids, ["c"]);
        assert_eq!(read.truncated_at, Some(parent(&page, "b")));
    }

    /// A route the server lacks answers with a page, which the client
    /// reads as an unsupported server, never as a deleted graph.
    #[test]
    fn an_unknown_route_answers_with_a_page() {
        let fake = FakePathbase::start();
        let url = create(&fake, path(vec![step("a", None)], None)).unwrap();
        let Err(AppendError::Other(msg)) =
            path_step_ids(&fake.base(), "tok", &format!("{url}/elsewhere"), false)
        else {
            panic!("an unknown route is not a stored path");
        };
        assert!(
            msg.contains("does not support incremental sharing"),
            "{msg}"
        );
    }
}
