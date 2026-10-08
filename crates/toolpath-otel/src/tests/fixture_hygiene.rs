//! Committed OTel fixtures, and the goldens and snapshots derived from them,
//! carry no host paths, host names or keys, and every capture directory has
//! its traces, manifest and oracle.

use super::common::captures::SEMCONV;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const CAPTURES: [&str; 10] = [
    "semconv/openai-chat/span",
    "semconv/openai-responses/span",
    "semconv/anthropic/span",
    "semconv/gemini/span",
    "openinference/openai-chat",
    // SYNTHETIC copy of openai-responses (see its manifest's "synthetic").
    "semconv/openai-responses/span-continuation",
    // Content on log records (capture.py --mode event).
    "semconv/openai-chat/event",
    "semconv/openai-responses/event",
    "semconv/anthropic/event",
    "semconv/gemini/event",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel")
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            files(&p, out)
        } else {
            out.push(p)
        }
    }
}

fn json(path: PathBuf) -> Value {
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// The directories the leak scan reads: the fixtures, and the goldens and
/// snapshots blessed from them.
fn scan_roots() -> Vec<PathBuf> {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    vec![
        root().join("semconv"),
        root().join("openinference"),
        root().join("equivalence"),
        root().join("openrouter"),
        crate_dir.join("tests/golden"),
        crate_dir.join("tests/snapshots"),
    ]
}

/// Every file under `roots`; a missing or empty root fails the scan.
fn scanned_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut all = Vec::new();
    for dir in roots {
        assert!(dir.is_dir(), "{} is missing", dir.display());
        let before = all.len();
        files(dir, &mut all);
        assert!(all.len() > before, "{} is empty", dir.display());
    }
    all
}

#[test]
#[should_panic(expected = "is missing")]
fn the_leak_scan_fails_on_a_missing_directory() {
    scanned_files(&[root().join("no-such-fixture-dir")]);
}

#[test]
fn the_leak_scan_fails_on_an_empty_directory() {
    let dir = std::env::temp_dir().join(format!("toolpath-otel-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let scan = std::panic::catch_unwind(|| scanned_files(std::slice::from_ref(&dir)));
    std::fs::remove_dir(&dir).unwrap();
    let msg = *scan.unwrap_err().downcast::<String>().unwrap();
    assert!(msg.ends_with("is empty"), "{msg}");
}

#[test]
fn the_leak_scan_covers_the_goldens_and_snapshots() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let roots = scan_roots();
    for dir in ["tests/golden", "tests/snapshots"] {
        assert!(roots.contains(&crate_dir.join(dir)), "{dir} is not scanned");
    }
}

#[test]
fn fixtures_hold_no_host_paths_names_or_keys() {
    let all = scanned_files(&scan_roots());
    for f in all {
        let text = std::fs::read_to_string(&f).unwrap();
        let name = f.display();
        assert!(!text.contains("/Users/"), "{name}");
        for (i, _) in text.match_indices("/home/") {
            assert!(text[i..].starts_with("/home/user"), "{name}: /home/ path");
        }
        for needle in [
            "\"host.name\"",
            "\"host.id\"",
            "process.executable",
            "process.command",
            "os.description",
            "Bearer ",
            "sk-fixture-dummy",
            "sk-ant-fixture-dummy",
            "fixture-dummy-key",
        ] {
            assert!(!text.contains(needle), "{name}: {needle}");
        }
    }
}

#[test]
fn every_capture_has_traces_manifest_and_oracle() {
    for dir in CAPTURES {
        let d = root().join(dir);
        let m = json(d.join("manifest.json"));
        // openai-chat records its own smaller lock (.venv-openai-v2), so only the
        // shared SDK pin is common to every manifest.
        assert!(
            m["packages"].as_object().is_some_and(|p| !p.is_empty()),
            "{dir}"
        );
        assert_eq!(m["packages"]["opentelemetry-sdk"], "1.45.0", "{dir}");
        assert!(
            m["scopes"].as_array().is_some_and(|s| !s.is_empty()),
            "{dir}"
        );
        assert_eq!(
            m["env"]["OTEL_RESOURCE_ATTRIBUTES"], "service.name=otel-fixture",
            "{dir}"
        );
        let e = json(d.join("expected.json"));
        assert!(e["requests"].as_u64().is_some_and(|n| n == 3), "{dir}");
        let t = json(d.join("traces.json"));
        assert!(
            t["resourceSpans"].as_array().is_some_and(|a| !a.is_empty()),
            "{dir}"
        );
    }
}

#[test]
fn only_the_continuation_copy_is_synthetic() {
    for dir in CAPTURES {
        let m = json(root().join(dir).join("manifest.json"));
        let synthetic = dir.ends_with("span-continuation");
        assert_eq!(m.get("synthetic").is_some(), synthetic, "{dir}");
        if synthetic {
            assert_eq!(m["synthetic"]["label"], "SYNTHETIC", "{dir}");
            assert_eq!(
                m["synthetic"]["added"],
                json!(["gen_ai.request.previous_response.id"]),
                "{dir}"
            );
        }
    }
}

#[test]
fn semconv_captures_recorded_span_only_content() {
    for p in SEMCONV {
        let dir = format!("semconv/{p}/span");
        let m = json(root().join(&dir).join("manifest.json"));
        assert_eq!(
            m["env"]["OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT"], "SPAN_ONLY",
            "{dir}"
        );
        assert_eq!(m["mode"], "span", "{dir}");
    }
}

#[test]
fn semconv_event_captures_recorded_event_only_content() {
    for p in SEMCONV {
        let dir = format!("semconv/{p}/event");
        let m = json(root().join(&dir).join("manifest.json"));
        assert_eq!(
            m["env"]["OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT"], "EVENT_ONLY",
            "{dir}"
        );
        assert_eq!(
            m["env"]["OTEL_INSTRUMENTATION_GENAI_EMIT_EVENT"], "true",
            "{dir}"
        );
        assert_eq!(m["mode"], "event", "{dir}");
    }
}

/// The event captures keep the exporter's logs request body as `logs.json`;
/// the openinference capture and the SYNTHETIC copy have no exporter.
#[test]
fn semconv_captures_keep_the_exported_request_bodies() {
    for p in SEMCONV {
        for (mode, files) in [("span", &[][..]), ("event", &["logs.json"][..])] {
            let d = root().join(format!("semconv/{p}/{mode}"));
            for f in files {
                let bytes = std::fs::read(d.join(f)).unwrap_or_default();
                assert!(!bytes.is_empty(), "semconv/{p}/{mode}/{f} missing or empty");
            }
            let m = json(d.join("manifest.json"));
            assert_eq!(
                m["otlp_exporter"]["package"], "opentelemetry-exporter-otlp-proto-http",
                "semconv/{p}/{mode}"
            );
        }
    }
    for dir in [
        "openinference/openai-chat",
        "semconv/openai-responses/span-continuation",
    ] {
        let d = root().join(dir);
        assert!(
            json(d.join("manifest.json")).get("otlp_exporter").is_none(),
            "{dir}"
        );
    }
}
