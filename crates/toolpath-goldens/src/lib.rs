//! Goldens as insta snapshots.
//!
//! A golden is a transcript one agent wrote (Claude Code, Codex, pi,
//! Copilot, Gemini CLI, opencode or Cursor) and what each stage of
//! toolpath's cross-harness pipeline produces from it. Inputs live in one
//! place: `test-fixtures/<agent>/convo.*` for the real sessions every crate
//! shares, `goldens/<name>/input.*` for live captures and derived inputs.
//! This crate is the pipeline alone:
//!
//! * [`model`]: the types. A [`model::Set`] goes in; [`model::Stages`] comes
//!   out, or a [`model::StageError`] naming the stage that failed.
//! * [`pipeline`]: `read -> ir -> derive -> project (every other agent) ->
//!   round trips -> validate -> shed -> known defects`, in process, through
//!   the adapters the `path` binary uses. Pure given its inputs: no
//!   manifest, no hashes, no git, no child process.
//! * [`keypaths`], [`losses`], [`shed`], [`defects`]: the pure measurements
//!   over those outputs, each with unit tests.
//! * [`validate`]: the validation `path p validate` runs (base schema plus
//!   the bundled kinds).
//! * [`accounting`]: the token accounting contract, an invariant the view
//!   asserts rather than pins.
//! * [`canonical`]: the stable form of a projection that differs between
//!   identical runs (Copilot mints a session id).
//! * [`leaks`]: a scan of output bytes for strings that must not be in a
//!   golden (the developer's home directory, the temp directory).
//! * [`schemas`]: the JSON Schema of each snapshot kind, generated from these
//!   types where a type is behind it (`docs/baseline/schemas/`).
//!
//! The view is `tests/goldens.rs`: every set through the pipeline, one insta
//! snapshot per stage under `snapshots/`. `cargo test` checks them,
//! `cargo insta review` accepts a change, and a failing stage is named by the
//! snapshot that differs.

pub mod accounting;
pub mod canonical;
pub mod classify;
pub mod contracts;
pub mod defects;
pub mod keypaths;
pub mod leaks;
pub mod losses;
pub mod model;
pub mod observed;
pub mod pipeline;
pub mod schemas;
pub mod shed;
pub mod transform;
pub mod validate;
