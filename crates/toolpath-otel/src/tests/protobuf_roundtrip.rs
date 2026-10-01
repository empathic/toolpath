//! Every committed OTLP/JSON fixture directory round-trips through
//! protobuf: JSON → `encode_protobuf` → `decode_protobuf` derives the same
//! paths (compared as canonical JSON values) and the same skip summary as
//! the JSON itself. Directories are found by globbing, so captures added
//! later are covered without edits.
use crate::tests::otel::{DeriveConfig, OtelError, ProfileSelection, decode_input, derive_paths};
use crate::{SkipCounts, decode_protobuf, encode_protobuf};
use serde_json::Value;
use std::path::{Path as FsPath, PathBuf};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel")
}

fn json_files(dir: &FsPath) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.is_file()
                && matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("json" | "ndjson" | "jsonl")
                )
        })
        .collect();
    files.sort();
    files
}

fn subdirs(dir: &FsPath, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.push(p.clone());
            subdirs(&p, out);
        }
    }
}

/// `openrouter/`, `equivalence/`, and every directory under `semconv/` and
/// `openinference/` that holds OTLP/JSON.
fn fixture_dirs() -> Vec<PathBuf> {
    let root = fixtures();
    let mut dirs = vec![root.join("openrouter"), root.join("equivalence")];
    subdirs(&root.join("semconv"), &mut dirs);
    subdirs(&root.join("openinference"), &mut dirs);
    dirs.retain(|d| d.is_dir() && !values(d).is_empty());
    dirs.sort();
    dirs
}

/// Every OTLP value in the directory's JSON files; `manifest.json` and
/// `expected.json` are `NotOtlp` and skipped, as the CLI's directory rule does.
fn values(dir: &FsPath) -> Vec<Value> {
    let mut out = Vec::new();
    for f in json_files(dir) {
        let bytes = std::fs::read(&f).unwrap();
        match decode_input(&bytes, f.file_name().and_then(|n| n.to_str())) {
            Ok(v) => out.extend(v),
            Err(OtelError::NotOtlp) => {}
            Err(e) => panic!("{}: {e}", f.display()),
        }
    }
    out
}

fn profile(dir: &FsPath) -> ProfileSelection {
    if dir.components().any(|c| c.as_os_str() == "openinference") {
        ProfileSelection::OpenInference
    } else {
        ProfileSelection::Auto
    }
}

fn derived(values: &[Value], sel: ProfileSelection) -> (Value, SkipCounts) {
    let (paths, outcome) = derive_paths(values.iter(), sel, &DeriveConfig::default()).unwrap();
    (
        serde_json::to_value(&paths).unwrap(),
        SkipCounts::from_outcome(&outcome),
    )
}

#[test]
fn the_fixture_set_is_the_expected_one() {
    let dirs = fixture_dirs();
    let names: Vec<String> = dirs
        .iter()
        .map(|d| d.strip_prefix(fixtures()).unwrap().display().to_string())
        .collect();
    for must in [
        "openrouter",
        "equivalence",
        "openinference/openai-chat",
        "semconv/anthropic/span",
        "semconv/gemini/span",
        "semconv/openai-chat/span",
        "semconv/openai-responses/span",
        "semconv/openai-responses/span-continuation",
        "semconv/anthropic/event",
        "semconv/gemini/event",
        "semconv/openai-chat/event",
        "semconv/openai-responses/event",
    ] {
        assert!(
            names.iter().any(|n| n == must),
            "missing {must} in {names:?}"
        );
    }
}

#[test]
fn every_json_fixture_round_trips_through_protobuf() {
    for dir in fixture_dirs() {
        let original = values(&dir);
        let mut round = Vec::with_capacity(original.len());
        for v in &original {
            let bytes = encode_protobuf(v).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            let back = decode_protobuf(&bytes).unwrap();
            // Fixed point: the canonical form re-encodes to the same bytes.
            assert_eq!(encode_protobuf(&back).unwrap(), bytes, "{}", dir.display());
            round.push(back);
        }
        let sel = profile(&dir);
        let (paths_json, summary_json) = derived(&original, sel);
        let (paths_pb, summary_pb) = derived(&round, sel);
        assert!(
            paths_json.as_array().is_some_and(|a| !a.is_empty()),
            "{} derives no path",
            dir.display()
        );
        assert_eq!(summary_json, summary_pb, "{}", dir.display());
        assert_eq!(paths_json, paths_pb, "{}", dir.display());
    }
}
