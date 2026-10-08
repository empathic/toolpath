//! Readers for the span-content captures, including the SYNTHETIC
//! continuation copy of openai-responses.

use crate::tests::otel::decode_input;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const SEMCONV: [&str; 4] = ["openai-chat", "openai-responses", "anthropic", "gemini"];

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel")
}

pub fn semconv_dir(name: &str) -> PathBuf {
    fixtures().join(format!("semconv/{name}/span"))
}

pub fn openinference_dir() -> PathBuf {
    fixtures().join("openinference/openai-chat")
}

pub fn traces_in(dir: &Path) -> Vec<Value> {
    decode_input(
        &std::fs::read(dir.join("traces.json")).unwrap(),
        Some("traces.json"),
    )
    .unwrap()
}

pub fn expected_in(dir: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join("expected.json")).unwrap()).unwrap()
}

pub fn manifest_in(dir: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap()
}

/// Every span of every delivery, mutably (for in-memory transformations).
pub fn for_each_span(values: &mut [Value], mut f: impl FnMut(&mut Value)) {
    for d in values {
        for rs in d["resourceSpans"].as_array_mut().unwrap() {
            for ss in rs["scopeSpans"].as_array_mut().unwrap() {
                for sp in ss["spans"].as_array_mut().unwrap() {
                    f(sp);
                }
            }
        }
    }
}

/// The SYNTHETIC continuation copy (manifest `synthetic.label == "SYNTHETIC"`).
pub fn continuation_dir() -> PathBuf {
    fixtures().join("semconv/openai-responses/span-continuation")
}
