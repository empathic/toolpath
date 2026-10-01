//! The crate's internals under the names the fixture tests use.

pub use crate::ProfileSelection;
pub use crate::error::{OtelError, Result};
pub use crate::generation::{Absent, CacheBasis, Generation, History, Message, ToolCall, Usage};
pub use crate::hash::derived_session_id;
pub use crate::input::decode_input;
pub use crate::provider::session_to_view;
pub use crate::session::Session;
pub use crate::stitch::{TurnGraph, stitch};
pub use crate::walk::{ReadOutcome, SkipReason, read_deliveries};
pub use toolpath_convo::DeriveConfig;

pub mod derive {
    pub use crate::derive::{canonical_step_json, conversation_key};
}
pub mod harness {
    pub use crate::harness::{infer_harness, signals};
}
pub mod hash {
    pub use crate::hash::{canonical_json, chain_id, root_id, sha256_hex};
}
pub mod normalize {
    pub use crate::normalize::{
        canonical, completion_message, content_hash, is_dropped, normalize,
    };
}
pub mod otlp {
    pub use crate::otlp::is_otlp;
}

use serde_json::Value;
use toolpath::v1::Path;

pub fn derive_path(session: &Session, config: &DeriveConfig) -> Path {
    crate::derive::derive_session(session, config)
}

pub(crate) use crate::group::group_generations as group_sessions;

/// [`group_sessions`] over a read batch, each session derived, with the
/// truncated mark the public `derive_path` applies.
pub fn derive_paths<'a>(
    deliveries: impl IntoIterator<Item = &'a Value>,
    sel: ProfileSelection,
    config: &DeriveConfig,
) -> Result<(Vec<Path>, ReadOutcome)> {
    let mut out = read_deliveries(deliveries, sel)?;
    let paths = group_sessions(std::mem::take(&mut out.generations))
        .into_iter()
        .map(|mut s| {
            s.truncated = s.session_id.as_deref().is_some_and(|id| {
                out.skipped.iter().any(|k| {
                    k.reason == SkipReason::Truncated && k.session_id.as_deref() == Some(id)
                })
            });
            derive_path(&s, config)
        })
        .collect();
    Ok((paths, out))
}
