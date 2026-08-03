//! Native interactive picker — the embedded backend behind
//! [`crate::fuzzy::pick`] (Atuin-inspired, built on ratatui).
//!
//! Module layout:
//!
//! - [`matcher`] — nucleo wrapper + fzf `--with-nth` field projection.
//! - [`preview`] — debounced async preview pipeline (no tokio).
//! - [`state`] — pure, event-vector-testable picker state machine.
//! - [`render`] — layout ladder + pane rendering.
//! - this file — terminal lifecycle and the event loop.
//!
//! The picker renders on **stderr** so stdout stays clean for piped
//! results, honors the full [`crate::fuzzy::PickOptions`] contract, and
//! returns [`crate::fuzzy::PickResult`] exactly like the external fzf
//! backend.

// Removed when fuzzy.rs switches its embedded backend to this module
// (the module is dark until then, and `-D warnings` would reject it).
#![allow(dead_code)]

mod matcher;
mod preview;
