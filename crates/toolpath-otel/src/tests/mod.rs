//! Fixture-driven tests over the crate internals.

#[cfg(all(feature = "protobuf", feature = "compression"))]
mod binary_input;
mod captures;
mod captures_event;
pub(crate) mod common;
mod derive;
mod derived_keys;
mod equivalence;
mod equivalence_fixtures;
mod fixture_hygiene;
mod fixtures;
mod lazy_read;
mod openinference_profile;
pub(crate) mod otel;
pub(crate) mod otlp_oracle;
mod privacy_mode;
#[cfg(feature = "protobuf")]
mod protobuf_roundtrip;
mod provider;
mod regression;
mod resolution;
mod robustness;
mod seam;
mod semconv_logs;
mod semconv_profile;
mod stitch;
