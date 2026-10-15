//! Offline round trip: a fake upstream on loopback streams an SSE body in three chunks; the mock
//! records it, then (upstream gone) replays it with the same bytes and the same chunk framing, and
//! answers a request it has never seen with a loud 599. Plus: the committed pathbase cassette
//! replays without any server.

use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use toolpath_webmock::{Config, Mode, Store, keys, serve};

const SSE: [&str; 3] = [
    "event: message_start\ndata: {\"type\":\"message_start\"}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"END\"}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
];

struct ChunkStream(std::vec::IntoIter<&'static str>);
impl futures_core::Stream for ChunkStream {
    type Item = Result<Frame<Bytes>, std::io::Error>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(
            self.0
                .next()
                .map(|s| Ok(Frame::data(Bytes::from_static(s.as_bytes())))),
        )
    }
}

/// The stand-in for api.anthropic.com: one SSE response, three frames, a request-id header.
async fn fake_upstream() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let svc = service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let authed = req.headers().contains_key("authorization");
                    let _ = req.into_body().collect().await;
                    let resp = Response::builder()
                        .status(if authed { 200 } else { 401 })
                        .header("content-type", "text/event-stream; charset=utf-8")
                        .header("request-id", "req_fake_1")
                        .body(StreamBody::new(ChunkStream(SSE.to_vec().into_iter())))
                        .unwrap();
                    Ok::<_, std::convert::Infallible>(resp)
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    (addr, handle)
}

fn cfg(agent: &str, mode: Mode, store: Arc<Store>) -> Config {
    Config {
        agent: agent.into(),
        mode,
        store,
        log: Arc::new(|l: &str| eprintln!("[test] {l}")),
    }
}

const BODY: &str = r#"{"model":"claude-haiku-4-5-20251001","stream":true,"messages":[{"role":"user","content":"Reply END"}],"metadata":{"user_id":"u"}}"#;

#[tokio::test]
async fn record_then_replay_keeps_bytes_and_framing_and_misses_loudly() {
    let tmp = tempfile::tempdir().unwrap();
    let (up_addr, up_handle) = fake_upstream().await;

    // record
    let store = Arc::new(Store::open(tmp.path().join("claude")).unwrap());
    let (rec_addr, rec_handle) = serve(
        cfg(
            "claude",
            Mode::Record {
                upstream: format!("http://{up_addr}"),
            },
            store.clone(),
        ),
        0,
    )
    .await
    .unwrap();
    let client = reqwest::Client::new();
    let r = client
        .post(format!("http://{rec_addr}/v1/messages?beta=true"))
        .header("authorization", "Bearer secret")
        .header("content-type", "application/json")
        .body(BODY)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let recorded = r.bytes().await.unwrap();
    assert_eq!(recorded, Bytes::from(SSE.concat()));
    // the writer runs after the client has its bytes; give it a moment
    for _ in 0..50 {
        if store.len() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(store.len(), 1, "one cassette written");
    let k = keys("POST", "/v1/messages?beta=true", BODY);
    let (c, how) = store.lookup(&k).expect("recorded request is found");
    assert_eq!(how, "EXACT");
    assert_eq!(
        c.response.chunks.len(),
        3,
        "SSE framing kept: one stored chunk per upstream frame"
    );
    assert!(c.response.sse);
    assert_eq!(
        c.request.headers.get("authorization").map(String::as_str),
        Some("<redacted>")
    );
    rec_handle.abort();
    up_handle.abort(); // the upstream is now unreachable

    // replay from a fresh Store (re-indexed from disk) with a volatile field changed
    let store2 = Arc::new(Store::open(tmp.path().join("claude")).unwrap());
    let (rep_addr, rep_handle) = serve(cfg("claude", Mode::Replay, store2), 0).await.unwrap();
    let body2 = BODY.replace(
        r#""metadata":{"user_id":"u"}"#,
        r#""metadata":{"user_id":"u"},"request_id":"different-every-time""#,
    );
    let r = client
        .post(format!("http://{rep_addr}/v1/messages?beta=true"))
        .header("authorization", "Bearer bogus")
        .header("content-type", "application/json")
        .body(body2)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers().get("request-id").unwrap(), "req_fake_1");
    assert_eq!(
        r.bytes().await.unwrap(),
        Bytes::from(SSE.concat()),
        "replayed bytes equal the recorded stream"
    );

    // miss: a prompt that was never recorded
    let r = client
        .post(format!("http://{rep_addr}/v1/messages?beta=true"))
        .header("content-type", "application/json")
        .body(BODY.replace("Reply END", "something else"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 599);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["webmock"], "cassette miss");
    assert!(
        std::fs::read_dir(tmp.path().join("claude"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().starts_with("miss-")),
        "miss file written"
    );
    rep_handle.abort();
}

/// The committed `cassettes/pathbase/` fixture (one `path share --anon` of the codex golden
/// against a local Pathbase) replays with no server at all.
#[tokio::test]
async fn committed_pathbase_cassette_replays() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("cassettes/pathbase");
    let store = Arc::new(Store::open(&dir).unwrap());
    assert_eq!(store.len(), 1);
    let (addr, handle) = serve(cfg("pathbase", Mode::Replay, store.clone()), 0)
        .await
        .unwrap();
    let cassette_path = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.extension().is_some_and(|x| x == "json")
                && !p
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("index")
        })
        .unwrap();
    let c: toolpath_webmock::Cassette =
        serde_json::from_str(&std::fs::read_to_string(cassette_path).unwrap()).unwrap();
    assert_eq!(c.request.url, "/api/v1/u/anon/repos/pathstash/graphs");
    let r = reqwest::Client::new()
        .post(format!("http://{addr}{}", c.request.url))
        .header("content-type", "application/json")
        .body(c.request.body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), c.response.status);
    let v: serde_json::Value = r.json().await.unwrap();
    assert!(
        v["url"]
            .as_str()
            .unwrap_or("")
            .contains("/u/anon/pathstash/graphs/"),
        "server-rendered share url came back: {v}"
    );
    handle.abort();
}
