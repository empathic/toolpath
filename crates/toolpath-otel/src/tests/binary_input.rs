//! decode_input over binary forms: every encoding of synthetic-fork.ndjson
//! decodes to the same deliveries and derives the same paths as the JSON
//! source; the error contract; a bounded fuzz sweep.
use crate::input::MAX_LAYERS;
use crate::tests::otel::{DeriveConfig, OtelError, ProfileSelection, decode_input, derive_paths};
use crate::{SkipCounts, decode_protobuf, encode_protobuf};
use serde_json::Value;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel")
}

fn read(rel: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(rel)).unwrap()
}

fn decode(rel: &str) -> Result<Vec<Value>, OtelError> {
    let name = rel.rsplit('/').next().unwrap();
    decode_input(&read(rel), Some(name))
}

fn source() -> Vec<Value> {
    decode("openrouter/synthetic-fork.ndjson").unwrap()
}

/// What protobuf decoding makes of a JSON delivery (the oracle for binary forms).
fn canon(v: &Value) -> Value {
    decode_protobuf(&encode_protobuf(v).unwrap()).unwrap()
}

const BINARY: [&str; 5] = [
    "encodings/synthetic-fork-frames.pb",
    "encodings/synthetic-fork-frames-zstd.pb",
    "encodings/synthetic-fork-frames.pb.zst",
    "encodings/synthetic-fork-first.binpb",
    "encodings/synthetic-fork-first.binpb.zst",
];

#[test]
fn json_forms_decode_to_the_source_values() {
    assert_eq!(
        decode("encodings/synthetic-fork.ndjson.zst").unwrap(),
        source()
    );
    assert_eq!(
        decode("encodings/synthetic-fork-json-frames-zstd.pb").unwrap(),
        source()
    );
}

#[test]
fn protobuf_forms_decode_to_the_canonical_source() {
    let all: Vec<Value> = source().iter().map(canon).collect();
    for rel in &BINARY[..3] {
        assert_eq!(decode(rel).unwrap(), all, "{rel}");
    }
    for rel in &BINARY[3..] {
        assert_eq!(decode(rel).unwrap(), all[..1], "{rel}");
    }
}

#[test]
fn every_form_derives_the_source_paths() {
    let derive = |values: &[Value]| {
        let (paths, outcome) = derive_paths(
            values.iter(),
            ProfileSelection::Auto,
            &DeriveConfig::default(),
        )
        .unwrap();
        (
            serde_json::to_value(&paths).unwrap(),
            SkipCounts::from_outcome(&outcome),
        )
    };
    let want = derive(&source());
    for rel in [
        "encodings/synthetic-fork.ndjson.zst",
        "encodings/synthetic-fork-json-frames-zstd.pb",
        BINARY[0],
        BINARY[1],
        BINARY[2],
    ] {
        assert_eq!(derive(&decode(rel).unwrap()), want, "{rel}");
    }
}

#[test]
fn a_tiny_zstd_known_answer_decodes() {
    // `printf '{"resourceSpans":[]}' | zstd -q -c -19 --no-check` (zstd 1.5.7)
    let z = hex("28b52ffd0068a100007b227265736f757263655370616e73223a5b5d7d");
    let v = decode_input(&z, Some("x.json.zst")).unwrap();
    assert_eq!(v, vec![serde_json::json!({"resourceSpans": []})]);
}

/// The fixtures are 4 and 5 layers deep: they pin the default bound.
#[test]
fn nested_compression_is_bounded() {
    assert_eq!(decode("encodings/nested-4.json.zst").unwrap().len(), 1);
    let err = decode("encodings/nested-5.json.zst").unwrap_err();
    let want = format!("more than {MAX_LAYERS} nested compression layers");
    assert!(
        matches!(&err, OtelError::Decompress(m) if *m == want),
        "{err:?}"
    );
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = (payload.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(payload);
    out
}

#[test]
fn the_error_contract_holds() {
    let d = |b: &[u8], name: Option<&str>| decode_input(b, name);
    assert!(matches!(d(b"", None), Err(OtelError::NotOtlp)));
    assert!(matches!(
        d(b"  \n", Some("x.binpb")),
        Err(OtelError::NotOtlp)
    ));
    assert!(matches!(
        d(br#"{"resourceSpans":"#, None),
        Err(OtelError::Json(_))
    ));
    assert!(matches!(d(b"[1,2", None), Err(OtelError::Json(_))));
    // Bytes that are neither JSON nor an OTLP request: NotOtlpBody, which
    // a directory import skips. Sniffed ones also name the JSON error.
    let err = d(b"hello world", None).unwrap_err();
    assert!(
        matches!(&err, OtelError::NotOtlpBody(m) if m.contains("not JSON either")),
        "{err:?}"
    );
    assert!(err.is_not_otlp());
    let err = d(&[0xff, 0x01, 0x02], Some("x.binpb")).unwrap_err();
    assert!(
        matches!(&err, OtelError::NotOtlpBody(m) if m.contains("not an OTLP logs request")),
        "{err:?}"
    );
    assert!(err.is_not_otlp());
    // The same garbage inside a Collector frame fails: a framed file is
    // OTLP output that went wrong, not a stray file.
    let err = d(&frame(&[0xff, 0x01, 0x02]), None).unwrap_err();
    assert!(
        matches!(&err, OtelError::Protobuf(m) if m.starts_with("frame 0: not an OTLP traces request")),
        "{err:?}"
    );
    assert!(!err.is_not_otlp());
    let mut bad_zstd = hex("28b52ffd");
    bad_zstd.extend([0xff; 12]);
    assert!(matches!(d(&bad_zstd, None), Err(OtelError::Decompress(_))));
    // A framed file whose JSON frame is not OTLP is an error, not a skip.
    let err = d(&frame(br#"{"not":"otlp"}"#), None).unwrap_err();
    assert!(
        matches!(&err, OtelError::Json(m) if m.starts_with("frame 0:")),
        "{err:?}"
    );
}

/// Cut short after a complete OTLP frame 0: framing gone wrong. Cut short
/// before frame 0 is complete: nothing shows it is OTLP, so not OTLP.
#[test]
fn truncated_or_overlong_frames_are_framing_errors() {
    let body = encode_protobuf(&source()[0]).unwrap();
    let whole = [frame(&body), frame(&body)].concat();
    for cut in [whole.len() - 1, body.len() + 4 + 2] {
        let err = decode_input(&whole[..cut], None).unwrap_err();
        assert!(matches!(err, OtelError::Framing(_)), "cut {cut}: {err:?}");
    }
    for cut in [3, body.len()] {
        let err = decode_input(&whole[..cut], None).unwrap_err();
        assert!(
            matches!(&err, OtelError::NotOtlpBody(m) if m.starts_with("a leading 0x00 byte")),
            "cut {cut}: {err:?}"
        );
    }
    // A length prefix far past the end (16 MiB - 1 declared, 3 bytes present).
    let err = decode_input(&[0x00, 0xff, 0xff, 0xff, 1, 2, 3], None).unwrap_err();
    assert!(
        matches!(&err, OtelError::NotOtlpBody(m) if m.contains("declares 16777215 bytes")),
        "{err:?}"
    );
}

/// A first frame of 16 MiB or more makes the file start with a non-zero
/// byte; frames that tile the file exactly are still frames.
#[test]
fn a_first_frame_of_16_mib_or_more_still_frames() {
    let mut big = source()[0].clone();
    big["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["name"] =
        Value::String("x".repeat(1 << 24));
    let bytes = [
        frame(&encode_protobuf(&big).unwrap()),
        frame(&encode_protobuf(&source()[1]).unwrap()),
    ]
    .concat();
    assert_ne!(bytes[0], 0x00);
    let want = vec![canon(&big), canon(&source()[1])];
    assert_eq!(decode_input(&bytes, Some("traces.pb")).unwrap(), want);
    assert_eq!(decode_input(&bytes, None).unwrap(), want);
}

#[test]
fn sniffed_binary_with_no_resources_is_an_error() {
    // Field 13, varint 5: an unknown field, so protobuf decodes it as an
    // empty request.
    let bytes = [0x68, 0x05];
    let err = decode_input(&bytes, Some("notes.txt")).unwrap_err();
    assert!(
        matches!(&err, OtelError::NotOtlpBody(m) if m.contains("no resourceSpans or resourceLogs entry") && m.contains("not JSON either")),
        "{err:?}"
    );
    assert!(err.is_not_otlp());
    let ok = decode_input(&bytes, Some("empty.binpb")).unwrap();
    assert_eq!(ok, vec![serde_json::json!({"resourceSpans": []})]);
}

#[test]
fn a_framed_file_with_a_bad_span_id_names_the_frame() {
    let mut v = source()[0].clone();
    v["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["spanId"] = serde_json::json!("abcd");
    let bytes = [
        frame(&encode_protobuf(&source()[1]).unwrap()),
        frame(&encode_protobuf(&v).unwrap()),
    ]
    .concat();
    let err = decode_input(&bytes, Some("traces.pb")).unwrap_err();
    assert!(
        matches!(&err, OtelError::Protobuf(m) if m.starts_with("frame 1:") && m.contains("2-byte span_id")),
        "{err:?}"
    );
    assert!(!err.is_not_otlp());
    // Unframed, a wire-valid traces body with a bad id is still Protobuf
    // (a directory import fails on it), not NotOtlpBody.
    let err = decode_input(&encode_protobuf(&v).unwrap(), Some("traces.binpb")).unwrap_err();
    assert!(
        matches!(&err, OtelError::Protobuf(m) if m.contains("2-byte span_id")),
        "{err:?}"
    );
}

/// A zero-length frame is a framing error naming the frame, not a silent
/// empty delivery.
#[test]
fn an_empty_frame_is_a_framing_error() {
    let err = decode_input(&[0, 0, 0, 0], None).unwrap_err();
    assert!(
        matches!(&err, OtelError::Framing(m) if m == "frame 0 is empty"),
        "{err:?}"
    );
    let body = encode_protobuf(&source()[0]).unwrap();
    let bytes = [frame(&body), frame(&[])].concat();
    let err = decode_input(&bytes, Some("traces.pb")).unwrap_err();
    assert!(
        matches!(&err, OtelError::Framing(m) if m == "frame 1 is empty"),
        "{err:?}"
    );
}

/// A Collector file whose first frame is 16 MiB or more (so it starts with
/// a non-zero byte) cut short in a later frame: frame 0 is OTLP, so the
/// file is framing gone wrong, `Framing`, not a stray non-OTLP body.
#[test]
fn a_big_first_frame_then_a_truncated_frame_is_framing() {
    let mut big = source()[0].clone();
    big["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["name"] =
        Value::String("x".repeat(1 << 24));
    let bytes = [
        frame(&encode_protobuf(&big).unwrap()),
        frame(&encode_protobuf(&source()[1]).unwrap()),
    ]
    .concat();
    assert_ne!(bytes[0], 0x00);
    let cut = &bytes[..bytes.len() - 1];
    for name in [None, Some("traces.pb")] {
        let err = decode_input(cut, name).unwrap_err();
        assert!(
            matches!(&err, OtelError::Framing(m) if m.starts_with("frame 1 declares")),
            "{name:?}: {err:?}"
        );
    }
}

/// Every binary fixture, truncated and byte-flipped at bounded positions:
/// `Ok` or `Err`, never a panic.
#[test]
fn binary_fixtures_never_panic_when_mangled() {
    let mut rels: Vec<&str> = BINARY.to_vec();
    rels.extend([
        "encodings/synthetic-fork.ndjson.zst",
        "encodings/synthetic-fork-json-frames-zstd.pb",
        "encodings/nested-5.json.zst",
    ]);
    for rel in rels {
        let src = read(rel);
        let name = rel.rsplit('/').next().unwrap();
        let step = (src.len() / 64).max(1);
        let mut cases: Vec<Vec<u8>> = (0..src.len())
            .step_by(step)
            .map(|c| src[..c].to_vec())
            .collect();
        for i in (0..src.len()).step_by(step) {
            for x in [0x00u8, 0xff, 0x28] {
                let mut m = src.clone();
                m[i] = x;
                cases.push(m);
            }
        }
        for (k, bytes) in cases.iter().enumerate() {
            let r = panic::catch_unwind(AssertUnwindSafe(|| decode_input(bytes, Some(name))));
            assert!(r.is_ok(), "{rel} case {k} panicked");
        }
    }
}
