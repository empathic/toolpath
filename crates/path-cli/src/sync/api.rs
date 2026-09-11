//! The Pathbase sync API: graph metadata, guarded mutations, and the
//! stored document. Hand-rolled over reqwest because each mutation
//! carries an `Idempotency-Key` and exact staged bytes, which the
//! generated client has no way to send.

use super::journal::{OperationKind, PendingOperation};
use serde::{Deserialize, Serialize};
use std::fmt;
use toolpath::v1::Graph;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphState {
    Mutable,
    Frozen,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MetaBase {
    pub(crate) from: String,
    pub(crate) source_graph_id: String,
    pub(crate) source_path_id: String,
    pub(crate) source_step_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Lineage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_graph_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) continuation_graph_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MetaPath {
    pub(crate) id: String,
    pub(crate) server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) head: Option<String>,
    pub(crate) step_count: u64,
}

/// `GraphMetaResponse`: everything sync needs to decide, never a transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GraphMeta {
    pub(crate) id: String,
    pub(crate) url: String,
    pub(crate) state: GraphState,
    pub(crate) generation: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) base: Option<MetaBase>,
    #[serde(default)]
    pub(crate) lineage: Lineage,
    #[serde(default)]
    pub(crate) paths: Vec<MetaPath>,
}

/// A mutation's result: the graph as the server now has it, and whether
/// a continuation request created it or found it already there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Applied {
    pub(crate) meta: GraphMeta,
    pub(crate) created: bool,
}

/// Why a request did not succeed, by the server's error `code`, never
/// by status alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApiFailure {
    Frozen,
    GenerationConflict,
    IdempotencyConflict,
    NotFound,
    /// The idempotent replay of an operation whose target was deleted.
    TargetDeleted,
    Unauthorized,
    Forbidden,
    /// The server rejected the request as invalid (4xx with a code
    /// sync should not retry blindly).
    Rejected {
        code: String,
        message: String,
    },
    /// The request may or may not have been applied.
    Ambiguous(String),
    /// The server does not implement the sync API.
    UpgradeRequired,
}

impl fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiFailure::Frozen => f.write_str("graph is frozen"),
            ApiFailure::GenerationConflict => f.write_str("generation conflict"),
            ApiFailure::IdempotencyConflict => f.write_str("idempotency key conflict"),
            ApiFailure::NotFound => f.write_str("not found"),
            ApiFailure::TargetDeleted => f.write_str("operation target was deleted"),
            ApiFailure::Unauthorized => f.write_str("not authorized (HTTP 401)"),
            ApiFailure::Forbidden => f.write_str("forbidden (HTTP 403)"),
            ApiFailure::Rejected { code, message } => write!(f, "{code}: {message}"),
            ApiFailure::Ambiguous(m) => write!(f, "no definitive answer: {m}"),
            ApiFailure::UpgradeRequired => {
                f.write_str("server does not support sync; upgrade Pathbase")
            }
        }
    }
}

impl std::error::Error for ApiFailure {}

impl ApiFailure {
    /// A failure that established the request was not applied, so the
    /// staged operation can be retired and re-planned.
    pub(crate) fn is_definitive(&self) -> bool {
        !matches!(self, ApiFailure::Ambiguous(_))
    }
}

pub(crate) trait SyncApi {
    fn meta(&self, repo: &str, graph_id: &str) -> Result<GraphMeta, ApiFailure>;
    /// Send a staged operation with its exact bytes and idempotency key.
    fn execute(&self, op: &PendingOperation, body: &[u8]) -> Result<Applied, ApiFailure>;
    /// The stored document: owned paths and steps, bases preserved.
    fn stored_document(&self, repo: &str, graph_id: &str) -> Result<Graph, ApiFailure>;
}

/// Bodies above this are gzip-encoded on the wire.
pub(crate) const COMPRESS_ABOVE_BYTES: usize = 256 * 1024;

pub(crate) struct PathbaseSync {
    base_url: String,
    client: reqwest::blocking::Client,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl PathbaseSync {
    pub(crate) fn new(base_url: &str, token: &str) -> anyhow::Result<Self> {
        use anyhow::Context;
        let mut headers = reqwest::header::HeaderMap::new();
        let mut auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .context("invalid characters in auth token")?;
        auth.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, auth);
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("path-cli/", env!("CARGO_PKG_VERSION")))
            .timeout(crate::cmd_pathbase::http_timeout())
            .default_headers(headers)
            .build()
            .context("build pathbase http client")?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        })
    }

    fn graphs_url(&self, repo: &str) -> String {
        let (owner, name) = repo.split_once('/').unwrap_or((repo, ""));
        format!("{}/api/v1/u/{owner}/repos/{name}/graphs", self.base_url)
    }

    fn send(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> Result<(reqwest::StatusCode, String), ApiFailure> {
        let response = request.send().map_err(|e| {
            if e.is_timeout() {
                ApiFailure::Ambiguous(format!("request timed out: {e}"))
            } else if e.is_connect() || e.is_request() {
                ApiFailure::Ambiguous(format!("request failed: {e}"))
            } else {
                ApiFailure::Ambiguous(e.to_string())
            }
        })?;
        let status = response.status();
        let body = response
            .text()
            .map_err(|e| ApiFailure::Ambiguous(format!("reading response: {e}")))?;
        Ok((status, body))
    }

    fn failure(status: reqwest::StatusCode, body: &str) -> ApiFailure {
        let envelope: ErrorEnvelope = serde_json::from_str(body).unwrap_or(ErrorEnvelope {
            code: None,
            error: None,
        });
        let code = envelope.code.unwrap_or_default();
        let message = envelope.error.unwrap_or_else(|| short(body));
        match (status.as_u16(), code.as_str()) {
            (_, "frozen") => ApiFailure::Frozen,
            (_, "generation_conflict") => ApiFailure::GenerationConflict,
            (_, "idempotency_conflict") => ApiFailure::IdempotencyConflict,
            (410, _) | (_, "operation_target_deleted") => ApiFailure::TargetDeleted,
            (404, "") | (_, "not_found") => ApiFailure::NotFound,
            (401, _) => ApiFailure::Unauthorized,
            (403, _) => ApiFailure::Forbidden,
            (405, _) | (501, _) => ApiFailure::UpgradeRequired,
            (400..=499, _) => ApiFailure::Rejected {
                code: if code.is_empty() {
                    status.as_u16().to_string()
                } else {
                    code
                },
                message,
            },
            _ => ApiFailure::Ambiguous(format!("HTTP {status}: {message}")),
        }
    }

    fn decode<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, ApiFailure> {
        serde_json::from_str(body)
            .map_err(|e| ApiFailure::Ambiguous(format!("unexpected response shape: {e}")))
    }
}

fn short(body: &str) -> String {
    let cleaned: String = body.replace(['\n', '\r'], " ");
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return "<empty body>".to_string();
    }
    trimmed.chars().take(200).collect()
}

pub(crate) fn encode_body(bytes: &[u8]) -> (Vec<u8>, bool) {
    if bytes.len() <= COMPRESS_ABOVE_BYTES {
        return (bytes.to_vec(), false);
    }
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    if encoder.write_all(bytes).is_err() {
        return (bytes.to_vec(), false);
    }
    match encoder.finish() {
        Ok(compressed) => (compressed, true),
        Err(_) => (bytes.to_vec(), false),
    }
}

impl SyncApi for PathbaseSync {
    fn meta(&self, repo: &str, graph_id: &str) -> Result<GraphMeta, ApiFailure> {
        let url = format!("{}/{graph_id}/meta", self.graphs_url(repo));
        let (status, body) = self.send(self.client.get(url))?;
        if status.is_success() {
            Self::decode(&body)
        } else if status.as_u16() == 404 && !body.contains("not_found") {
            // A server without the meta route answers a bare 404.
            Err(ApiFailure::UpgradeRequired)
        } else {
            Err(Self::failure(status, &body))
        }
    }

    fn execute(&self, op: &PendingOperation, body: &[u8]) -> Result<Applied, ApiFailure> {
        let graphs = self.graphs_url(&op.repo);
        let (method, url) = match &op.kind {
            OperationKind::Create => (reqwest::Method::POST, graphs),
            OperationKind::Update { graph_id } => {
                (reqwest::Method::PUT, format!("{graphs}/{graph_id}"))
            }
            OperationKind::Freeze { graph_id } => {
                (reqwest::Method::POST, format!("{graphs}/{graph_id}/freeze"))
            }
            OperationKind::Continuation {
                source_graph_id, ..
            } => (
                reqwest::Method::POST,
                format!("{graphs}/{source_graph_id}/continuations"),
            ),
        };
        let (payload, compressed) = encode_body(body);
        let mut request = self
            .client
            .request(method, url)
            .header("Idempotency-Key", &op.key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload);
        if compressed {
            request = request.header(reqwest::header::CONTENT_ENCODING, "gzip");
        }
        let (status, text) = self.send(request)?;
        if !status.is_success() {
            return Err(Self::failure(status, &text));
        }
        let created = status.as_u16() == 201;
        let meta = match &op.kind {
            // POST /graphs answers with the document envelope; the lean
            // shape is one more read away.
            OperationKind::Create => {
                #[derive(Deserialize)]
                struct Created {
                    id: String,
                }
                let Created { id } = Self::decode(&text)?;
                self.meta(&op.repo, &id)?
            }
            _ => Self::decode(&text)?,
        };
        Ok(Applied { meta, created })
    }

    fn stored_document(&self, repo: &str, graph_id: &str) -> Result<Graph, ApiFailure> {
        let url = format!("{}/{graph_id}/download", self.graphs_url(repo));
        let (status, body) = self.send(self.client.get(url))?;
        if !status.is_success() {
            return Err(Self::failure(status, &body));
        }
        Graph::from_json(&body).map_err(|e| ApiFailure::Ambiguous(format!("stored document: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_are_classified_by_code_before_status() {
        let f = |status: u16, body: &str| {
            PathbaseSync::failure(reqwest::StatusCode::from_u16(status).unwrap(), body)
        };
        assert_eq!(
            f(409, r#"{"code":"frozen","error":"x"}"#),
            ApiFailure::Frozen
        );
        assert_eq!(
            f(409, r#"{"code":"generation_conflict","error":"x"}"#),
            ApiFailure::GenerationConflict
        );
        assert_eq!(
            f(409, r#"{"code":"conflict","error":"already exists"}"#),
            ApiFailure::Rejected {
                code: "conflict".into(),
                message: "already exists".into()
            }
        );
        assert_eq!(f(410, "{}"), ApiFailure::TargetDeleted);
        assert_eq!(
            f(404, r#"{"code":"not_found","error":"x"}"#),
            ApiFailure::NotFound
        );
        assert_eq!(f(401, ""), ApiFailure::Unauthorized);
        assert_eq!(f(405, ""), ApiFailure::UpgradeRequired);
        assert!(matches!(f(502, "<html>"), ApiFailure::Ambiguous(_)));
        assert!(!f(503, "").is_definitive());
        assert!(f(400, r#"{"code":"invalid_base","error":"bad"}"#).is_definitive());
    }

    #[test]
    fn small_bodies_stay_plain_and_large_ones_gzip() {
        let (small, compressed) = encode_body(b"{}");
        assert_eq!(small, b"{}");
        assert!(!compressed);
        let big = vec![b'a'; COMPRESS_ABOVE_BYTES + 1];
        let (payload, compressed) = encode_body(&big);
        assert!(compressed);
        assert!(payload.len() < big.len());
        use std::io::Read;
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(payload.as_slice())
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, big);
    }

    #[test]
    fn meta_roundtrips_with_optional_parts_absent() {
        let json = r#"{"id":"g","url":"https://h/u/o/r/graphs/g","state":"frozen","generation":3}"#;
        let meta: GraphMeta = serde_json::from_str(json).unwrap();
        assert_eq!(meta.state, GraphState::Frozen);
        assert!(meta.base.is_none());
        assert_eq!(meta.lineage, Lineage::default());
        assert!(meta.paths.is_empty());
    }
}
