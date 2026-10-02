use super::common::captures::SEMCONV;
use super::common::equivalence::comparison_set;
use crate::tests::otel::{DeriveConfig, OtelError, ProfileSelection, decode_input, derive_paths};
use serde_json::Value;
use std::path::{Path as FsPath, PathBuf};
use toolpath::v1::Path;

fn capture(provider: &str, mode: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-fixtures/otel/semconv")
        .join(provider)
        .join(mode)
}

/// OTLP values of the directory's files with one of `exts`, in file-name
/// order; `manifest.json`/`expected.json` are NotOtlp and skipped.
fn values(dir: &FsPath, exts: &[&str]) -> Vec<Value> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| exts.contains(&e))
        })
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let name = f.file_name().and_then(|n| n.to_str());
        match decode_input(&std::fs::read(&f).unwrap(), name) {
            Ok(v) => out.extend(v),
            Err(OtelError::NotOtlp) => {}
            Err(e) => panic!("{}: {e}", f.display()),
        }
    }
    out
}

fn derive(values: &[Value]) -> Vec<Path> {
    derive_paths(
        values.iter(),
        ProfileSelection::Auto,
        &DeriveConfig::default(),
    )
    .unwrap()
    .0
}

#[cfg(feature = "protobuf")]
fn canonical(paths: &[Path]) -> Value {
    serde_json::to_value(paths).unwrap()
}

#[cfg(feature = "protobuf")]
#[test]
fn binpb_bodies_derive_the_same_paths_as_the_json() {
    for p in SEMCONV {
        for mode in ["span", "event"] {
            let dir = capture(p, mode);
            let json = canonical(&derive(&values(&dir, &["json"])));
            let binpb = canonical(&derive(&values(&dir, &["binpb"])));
            assert_eq!(json, binpb, "{p}/{mode}");
        }
    }
}

#[cfg(feature = "protobuf")]
#[test]
fn both_encodings_in_one_directory_read_each_record_once() {
    for p in SEMCONV {
        for mode in ["span", "event"] {
            let dir = capture(p, mode);
            let json = canonical(&derive(&values(&dir, &["json"])));
            let both = canonical(&derive(&values(&dir, &["json", "binpb"])));
            assert_eq!(json, both, "{p}/{mode}");
        }
    }
}

#[test]
fn event_captures_carry_content_on_log_records_only() {
    for p in SEMCONV {
        let dir = capture(p, "event");
        let traces = std::fs::read_to_string(dir.join("traces.json")).unwrap();
        for key in ["\"gen_ai.input.messages\"", "\"gen_ai.output.messages\""] {
            assert!(!traces.contains(key), "{p}: event-mode spans carry {key}");
        }
        let logs = values(&dir, &["json"]);
        let records: usize = logs
            .iter()
            .filter_map(|v| v.get("resourceLogs")?.as_array())
            .flatten()
            .filter_map(|rl| rl.get("scopeLogs")?.as_array())
            .flatten()
            .filter_map(|sl| sl.get("logRecords")?.as_array().map(Vec::len))
            .sum();
        assert!(records > 0, "{p}: no log records");
    }
}

#[test]
fn event_capture_derives_the_span_capture_path() {
    for p in SEMCONV {
        let span = derive(&values(&capture(p, "span"), &["json"]));
        let event = derive(&values(&capture(p, "event"), &["json"]));
        assert!(!span.is_empty(), "{p}: span capture derives nothing");
        assert_eq!(span.len(), event.len(), "{p}");
        for (s, e) in span.iter().zip(&event) {
            assert_eq!(comparison_set(s), comparison_set(e), "{p}");
        }
    }
}

#[test]
fn event_manifests_answer_the_open_questions() {
    for p in SEMCONV {
        let m: Value = serde_json::from_str(
            &std::fs::read_to_string(capture(p, "event").join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(m["mode"], "event", "{p}");
        for key in [
            "event_name_source",
            "details_content_location",
            "legacy_events",
            "otlp_exporter",
        ] {
            assert!(!m[key].is_null(), "{p}: manifest lacks {key}");
        }
    }
}
