//! The Pathbase sync API: graph metadata, guarded mutations, and the
//! stored document, over the generated `pathbase_client`. Each
//! mutation carries the staged `Idempotency-Key` as the generated
//! header parameter; failures are classified by the server's typed
//! error code, with the status as the fallback.

use super::journal::{OperationKind, PendingOperation};
use crate::cmd_pathbase::{block_on, pathbase_client};
use pathbase_client::types::{ApiErrorCode, ApiErrorResponse, ToolpathDocument};
use reqwest::StatusCode;
use std::fmt;
use toolpath::v1::Graph;
use uuid::Uuid;

pub(crate) use pathbase_client::types::{
    ContinuationBody, FreezeGraphBody, GraphMetaResponse, GraphState, ReplaceGraphBody,
    UploadGraphBody,
};

type ClientError = pathbase_client::Error<ApiErrorResponse>;

/// A mutation's result: the graph as the server now has it, and whether
/// the request created it or found it already there.
#[derive(Debug, Clone)]
pub(crate) struct Applied {
    pub(crate) meta: GraphMetaResponse,
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
    fn meta(&self, repo: &str, graph_id: &str) -> Result<GraphMetaResponse, ApiFailure>;
    /// Send a staged operation with its exact bytes and idempotency key.
    fn execute(&self, op: &PendingOperation, body: &[u8]) -> Result<Applied, ApiFailure>;
    /// The stored document: owned paths and steps, bases preserved.
    fn stored_document(&self, repo: &str, graph_id: &str) -> Result<Graph, ApiFailure>;
}

/// A `Graph` as the generated request bodies carry it. A graph
/// serializes to `{graph, paths, meta?}`, which is `ToolpathDocument`.
pub(crate) fn to_document(doc: &Graph) -> anyhow::Result<ToolpathDocument> {
    Ok(serde_json::from_value(serde_json::to_value(doc)?)?)
}

pub(crate) struct PathbaseSync {
    client: pathbase_client::Client,
}

impl PathbaseSync {
    pub(crate) fn new(base_url: &str, token: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client: pathbase_client(base_url, Some(token))?,
        })
    }
}

fn split_repo(repo: &str) -> (&str, &str) {
    repo.split_once('/').unwrap_or((repo, ""))
}

fn graph_uuid(id: &str) -> Result<Uuid, ApiFailure> {
    id.parse().map_err(|_| ApiFailure::Rejected {
        code: "invalid_graph_id".into(),
        message: format!("{id:?} is not a graph UUID"),
    })
}

/// The staged bytes were written as the serialization of this type, so
/// parsing them back and letting the client serialize sends the same
/// bytes the first attempt sent.
fn staged<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ApiFailure> {
    serde_json::from_slice(body).map_err(|e| ApiFailure::Rejected {
        code: "invalid_body".into(),
        message: format!("staged body does not parse: {e}"),
    })
}

fn failure(err: ClientError) -> ApiFailure {
    use pathbase_client::Error;
    match err {
        Error::ErrorResponse(resp) => {
            let status = resp.status();
            let ApiErrorResponse { code, error } = resp.into_inner();
            by_code(status, code, error)
        }
        Error::UnexpectedResponse(resp) => {
            let status = resp.status();
            let body = block_on(resp.text()).unwrap_or_default();
            by_status(status, &body)
        }
        // The answer did not match the sync API this client was built
        // against: a bare 404 from a server without the route, or a
        // success body from before graphs had `state` and `generation`.
        Error::InvalidResponsePayload(_, _) => ApiFailure::UpgradeRequired,
        Error::InvalidRequest(message) => ApiFailure::Rejected {
            code: "invalid_request".into(),
            message,
        },
        other => ApiFailure::Ambiguous(other.to_string()),
    }
}

fn by_code(status: StatusCode, code: ApiErrorCode, message: String) -> ApiFailure {
    match code {
        ApiErrorCode::Frozen => ApiFailure::Frozen,
        ApiErrorCode::GenerationConflict => ApiFailure::GenerationConflict,
        ApiErrorCode::IdempotencyConflict => ApiFailure::IdempotencyConflict,
        ApiErrorCode::OperationTargetDeleted => ApiFailure::TargetDeleted,
        ApiErrorCode::NotFound => ApiFailure::NotFound,
        ApiErrorCode::Unauthorized => ApiFailure::Unauthorized,
        ApiErrorCode::Forbidden => ApiFailure::Forbidden,
        ApiErrorCode::InternalError => ApiFailure::Ambiguous(format!("HTTP {status}: {message}")),
        _ if status.is_server_error() => ApiFailure::Ambiguous(format!("HTTP {status}: {message}")),
        other => ApiFailure::Rejected {
            code: other.to_string(),
            message,
        },
    }
}

/// For a status the spec does not declare, where there is no typed code.
fn by_status(status: StatusCode, body: &str) -> ApiFailure {
    match status.as_u16() {
        401 => ApiFailure::Unauthorized,
        403 => ApiFailure::Forbidden,
        404 => ApiFailure::NotFound,
        410 => ApiFailure::TargetDeleted,
        405 | 501 => ApiFailure::UpgradeRequired,
        400..=499 => ApiFailure::Rejected {
            code: status.as_u16().to_string(),
            message: short(body),
        },
        _ => ApiFailure::Ambiguous(format!("HTTP {status}: {}", short(body))),
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

impl SyncApi for PathbaseSync {
    fn meta(&self, repo: &str, graph_id: &str) -> Result<GraphMetaResponse, ApiFailure> {
        let (owner, name) = split_repo(repo);
        let id = graph_uuid(graph_id)?;
        block_on(self.client.get_graph_meta(owner, name, &id))
            .map(|resp| resp.into_inner())
            .map_err(failure)
    }

    fn execute(&self, op: &PendingOperation, body: &[u8]) -> Result<Applied, ApiFailure> {
        let (owner, name) = split_repo(&op.repo);
        let key = Some(op.key.as_str());
        let (status, meta) = match &op.kind {
            OperationKind::Create => {
                let body: UploadGraphBody = staged(body)?;
                let resp =
                    block_on(self.client.create_graph(owner, name, key, &body)).map_err(failure)?;
                let status = resp.status();
                let id = resp.into_inner().id;
                // POST /graphs answers with the document envelope; the
                // lean shape is one more read away.
                let meta = block_on(self.client.get_graph_meta(owner, name, &id))
                    .map_err(failure)?
                    .into_inner();
                (status, meta)
            }
            OperationKind::Update { graph_id } => {
                let id = graph_uuid(graph_id)?;
                let body: ReplaceGraphBody = staged(body)?;
                let resp = block_on(self.client.replace_graph(owner, name, &id, key, &body))
                    .map_err(failure)?;
                (resp.status(), resp.into_inner())
            }
            OperationKind::Freeze { graph_id } => {
                let id = graph_uuid(graph_id)?;
                let body: FreezeGraphBody = staged(body)?;
                let resp = block_on(self.client.freeze_graph(owner, name, &id, key, &body))
                    .map_err(failure)?;
                (resp.status(), resp.into_inner())
            }
            OperationKind::Continuation {
                source_graph_id, ..
            } => {
                let id = graph_uuid(source_graph_id)?;
                let body: ContinuationBody = staged(body)?;
                let resp = block_on(
                    self.client
                        .create_continuation(owner, name, &id, key, &body),
                )
                .map_err(failure)?;
                (resp.status(), resp.into_inner())
            }
        };
        Ok(Applied {
            meta,
            created: status == StatusCode::CREATED,
        })
    }

    fn stored_document(&self, repo: &str, graph_id: &str) -> Result<Graph, ApiFailure> {
        let (owner, name) = split_repo(repo);
        let id = graph_uuid(graph_id)?;
        let map = block_on(self.client.download_graph(owner, name, &id))
            .map_err(failure)?
            .into_inner();
        serde_json::from_value(serde_json::Value::Object(map))
            .map_err(|e| ApiFailure::Ambiguous(format!("stored document: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pathbase_client::ResponseValue;
    use toolpath::v1::{Base, Path as TpPath, Step};

    fn coded(status: u16, code: ApiErrorCode, message: &str) -> ApiFailure {
        failure(pathbase_client::Error::ErrorResponse(ResponseValue::new(
            ApiErrorResponse {
                code,
                error: message.into(),
            },
            StatusCode::from_u16(status).unwrap(),
            Default::default(),
        )))
    }

    #[test]
    fn failures_are_classified_by_code_before_status() {
        assert_eq!(coded(409, ApiErrorCode::Frozen, "x"), ApiFailure::Frozen);
        assert_eq!(
            coded(409, ApiErrorCode::GenerationConflict, "x"),
            ApiFailure::GenerationConflict
        );
        assert_eq!(
            coded(409, ApiErrorCode::IdempotencyConflict, "x"),
            ApiFailure::IdempotencyConflict
        );
        assert_eq!(
            coded(409, ApiErrorCode::Conflict, "already exists"),
            ApiFailure::Rejected {
                code: "conflict".into(),
                message: "already exists".into()
            }
        );
        assert_eq!(
            coded(410, ApiErrorCode::OperationTargetDeleted, ""),
            ApiFailure::TargetDeleted
        );
        assert_eq!(
            coded(404, ApiErrorCode::NotFound, "x"),
            ApiFailure::NotFound
        );
        assert_eq!(
            coded(401, ApiErrorCode::Unauthorized, ""),
            ApiFailure::Unauthorized
        );
        assert_eq!(
            coded(403, ApiErrorCode::Forbidden, ""),
            ApiFailure::Forbidden
        );
        assert!(matches!(
            coded(500, ApiErrorCode::InternalError, "boom"),
            ApiFailure::Ambiguous(_)
        ));
        assert!(!coded(503, ApiErrorCode::BadRequest, "").is_definitive());
        assert!(coded(400, ApiErrorCode::InvalidBase, "bad").is_definitive());
        assert_eq!(
            coded(400, ApiErrorCode::InheritedStepRedefined, "s"),
            ApiFailure::Rejected {
                code: "inherited_step_redefined".into(),
                message: "s".into()
            }
        );
    }

    #[test]
    fn undeclared_statuses_fall_back_to_the_status() {
        let s = |code: u16| StatusCode::from_u16(code).unwrap();
        assert_eq!(by_status(s(401), ""), ApiFailure::Unauthorized);
        assert_eq!(by_status(s(404), ""), ApiFailure::NotFound);
        assert_eq!(by_status(s(405), ""), ApiFailure::UpgradeRequired);
        assert_eq!(by_status(s(410), ""), ApiFailure::TargetDeleted);
        assert_eq!(
            by_status(s(422), "nope\n"),
            ApiFailure::Rejected {
                code: "422".into(),
                message: "nope".into()
            }
        );
        assert!(matches!(
            by_status(s(502), "<html>"),
            ApiFailure::Ambiguous(_)
        ));
    }

    #[test]
    fn client_side_and_shape_errors() {
        let bad_shape: ClientError = pathbase_client::Error::InvalidResponsePayload(
            Default::default(),
            serde_json::from_str::<i32>("").unwrap_err(),
        );
        assert_eq!(failure(bad_shape), ApiFailure::UpgradeRequired);
        let bad_request: ClientError = pathbase_client::Error::InvalidRequest("header".into());
        assert!(matches!(
            failure(bad_request),
            ApiFailure::Rejected { code, .. } if code == "invalid_request"
        ));
        let hook: ClientError = pathbase_client::Error::Custom("hook".into());
        assert!(!failure(hook).is_definitive());
        assert!(graph_uuid("g1").is_err());
        assert!(graph_uuid("6f1c2c3e-9c2a-4b6f-8f1e-0b1a2c3d4e5f").is_ok());
    }

    fn graph() -> Graph {
        let mut path = TpPath::new("p", Some(Base::vcs("github:o/r", "abc")), "b");
        let mut a = Step::new("a", "agent:test", "2026-09-10T00:00:00Z");
        a.change.insert(
            "src/main.rs".into(),
            serde_json::from_value(serde_json::json!({ "raw": "+fn main() {}\n" })).unwrap(),
        );
        let mut b = Step::new("b", "agent:test", "2026-09-10T00:00:01Z");
        b.step.parents = vec!["a".into()];
        path.steps.push(a);
        path.steps.push(b);
        Graph::from_path(path)
    }

    fn round_trips<T: serde::Serialize + serde::de::DeserializeOwned>(body: &T) -> Vec<u8> {
        let staged_bytes = serde_json::to_vec(body).unwrap();
        let parsed: T = staged(&staged_bytes).unwrap();
        assert_eq!(serde_json::to_vec(&parsed).unwrap(), staged_bytes);
        staged_bytes
    }

    #[test]
    fn staged_bodies_round_trip_through_the_generated_types_byte_for_byte() {
        let document = to_document(&graph()).unwrap();
        let create = round_trips(&UploadGraphBody {
            document: document.clone(),
            name: None,
            visibility: None,
            freeze_after: Some(false),
        });
        let create: serde_json::Value = serde_json::from_slice(&create).unwrap();
        assert_eq!(create["freeze_after"], false);
        assert!(create.get("visibility").is_none());
        assert_eq!(
            create["document"]["paths"][0]["steps"][1]["step"]["id"],
            "b"
        );
        let update = round_trips(&ReplaceGraphBody {
            document: document.clone(),
            expected_generation: 7,
            freeze_after: Some(true),
        });
        let update: serde_json::Value = serde_json::from_slice(&update).unwrap();
        assert_eq!(update["expected_generation"], 7);
        let cont = round_trips(&ContinuationBody {
            document,
            source_path: "p".into(),
            expected_generation: None,
            freeze_after: Some(false),
        });
        let cont: serde_json::Value = serde_json::from_slice(&cont).unwrap();
        assert!(cont.get("expected_generation").is_none());
        assert_eq!(cont["source_path"], "p");
        round_trips(&FreezeGraphBody {
            expected_generation: 3,
        });
    }

    #[test]
    fn the_document_survives_the_typed_body_unchanged() {
        let doc = graph();
        let typed = to_document(&doc).unwrap();
        let back: Graph = serde_json::from_value(serde_json::to_value(&typed).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&doc).unwrap()
        );
    }

    #[test]
    fn meta_decodes_with_optional_parts_absent_and_state_keeps_its_wire_form() {
        let json = r#"{"id":"6f1c2c3e-9c2a-4b6f-8f1e-0b1a2c3d4e5f","url":"https://h/u/o/r/graphs/6f1c","state":"frozen","generation":3,"updated_at":"2026-09-10T18:00:00Z","lineage":{},"paths":[]}"#;
        let meta: GraphMetaResponse = serde_json::from_str(json).unwrap();
        assert_eq!(meta.state, GraphState::Frozen);
        assert!(meta.base.is_none());
        assert!(meta.lineage.source_graph_id.is_none());
        assert!(meta.paths.is_empty());
        assert_eq!(
            serde_json::to_string(&GraphState::Mutable).unwrap(),
            "\"mutable\""
        );
        assert_eq!(
            serde_json::to_string(&GraphState::Frozen).unwrap(),
            "\"frozen\""
        );
    }
}
