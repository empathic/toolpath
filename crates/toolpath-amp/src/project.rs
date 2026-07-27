//! Project a provider-agnostic [`ConversationView`] into an Amp thread
//! export document — the reverse of [`crate::provider::to_view`].
//!
//! **Stub.** The real projector is piece 03's deliverable: it opens with the
//! deferred resume/writer recon (Amp threads are server-authoritative, so
//! whether a fabricated local record can be resumed at all is an open
//! question — see `docs/agents/formats/amp/resume-and-sessions.md`). Until
//! then, projecting returns an error instead of pretending.

use crate::types::ThreadExport;
use toolpath_convo::{ConversationProjector, ConversationView, ConvoError, Result};

/// Projects a [`ConversationView`] into an Amp [`ThreadExport`].
#[derive(Debug, Clone, Default)]
pub struct AmpProjector;

impl AmpProjector {
    pub fn new() -> Self {
        Self
    }
}

impl ConversationProjector for AmpProjector {
    type Output = ThreadExport;

    fn project(&self, _view: &ConversationView) -> Result<Self::Output> {
        Err(ConvoError::Provider(
            "AmpProjector is not implemented yet (piece 03: resume/writer recon pending)".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_refuses_to_project() {
        let view = ConversationView::default();
        assert!(AmpProjector::new().project(&view).is_err());
    }
}
