//! Input decoding (spec: Transport and encoding, Input decoding errors).

use serde_json::{Value, json};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use toolpath_otel::{OtelError, decode_input, decode_input_with_limit};

fn otel_fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel")
}

fn encoding(name: &str) -> Vec<u8> {
    std::fs::read(otel_fixtures().join("encodings").join(name)).unwrap()
}

/// The first two Claude Code deliveries, parsed straight from the M0 file.
fn m0_lines() -> Vec<Value> {
    std::fs::read_to_string(otel_fixtures().join("openrouter/claude-code.ndjson"))
        .unwrap()
        .lines()
        .take(2)
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn text(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

#[test]
fn a_body_and_json_lines_decode_to_the_parsed_deliveries() {
    let want = m0_lines();
    assert_eq!(
        decode_input(&encoding("body.json"), Some("body.json")).unwrap(),
        want[..1]
    );
    assert_eq!(
        decode_input(&encoding("lines.jsonl"), Some("lines.jsonl")).unwrap(),
        want
    );
}

#[test]
fn crlf_and_bom_decode_like_the_original() {
    assert_eq!(
        decode_input(&encoding("crlf-bom.jsonl"), Some("crlf-bom.jsonl")).unwrap(),
        m0_lines()
    );
    let bom_body = [b"\xef\xbb\xbf".as_slice(), &encoding("body.json")].concat();
    assert_eq!(decode_input(&bom_body, None).unwrap(), m0_lines()[..1]);
}

#[cfg(feature = "compression")]
#[test]
fn gzip_decodes_like_the_plain_bytes() {
    assert_eq!(
        decode_input(&encoding("body.json.gz"), Some("body.json.gz")).unwrap(),
        m0_lines()[..1]
    );
}

#[cfg(feature = "compression")]
#[test]
fn multi_member_gzip_reads_every_member() {
    assert_eq!(
        decode_input(
            &encoding("two-members.jsonl.gz"),
            Some("two-members.jsonl.gz")
        )
        .unwrap(),
        m0_lines()
    );
}

#[cfg(feature = "compression")]
#[test]
fn a_bad_gzip_stream_is_decompress() {
    let r = decode_input(&[0x1f, 0x8b, 0x08, 0x00, 0x01, 0x02], Some("x.gz"));
    assert!(matches!(r, Err(OtelError::Decompress(_))), "{r:?}");
}

#[cfg(feature = "compression")]
#[test]
fn nested_gzip_is_followed_up_to_the_limit() {
    use std::io::Write;
    let gz = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    };
    let body = encoding("body.json");
    let twice = gz(&gz(&body));
    assert_eq!(decode_input(&twice, None).unwrap(), m0_lines()[..1]);
    let five = gz(&gz(&gz(&gz(&gz(&body)))));
    assert!(matches!(
        decode_input(&five, None),
        Err(OtelError::Decompress(_))
    ));
}

#[cfg(not(feature = "compression"))]
#[test]
fn gzip_without_the_feature_is_feature_disabled() {
    let r = decode_input(&encoding("body.json.gz"), Some("body.json.gz"));
    assert!(
        matches!(r, Err(OtelError::FeatureDisabled("compression"))),
        "{r:?}"
    );
}

#[test]
fn non_otlp_json_and_empty_input_are_not_otlp() {
    let expected = std::fs::read(otel_fixtures().join("openrouter/expected.json")).unwrap();
    for (bytes, name) in [
        (expected, "expected.json"),
        (text(""), "empty.json"),
        (text("  \n\t\n"), "blank.json"),
        (text("[1, 2]"), "array.json"),
        (text("42"), "scalar.json"),
        (text("{\"a\": 1}\n{\"b\": 2}\n"), "manifest.jsonl"),
    ] {
        let r = decode_input(&bytes, Some(name));
        assert!(matches!(r, Err(OtelError::NotOtlp)), "{name}: {r:?}");
    }
}

#[test]
fn logs_only_and_metrics_only_values_are_otlp() {
    for v in [
        json!({"resourceLogs": []}),
        json!({"resourceMetrics": [{}]}),
    ] {
        let bytes = serde_json::to_vec(&v).unwrap();
        assert_eq!(decode_input(&bytes, None).unwrap(), vec![v]);
    }
}

#[test]
fn a_non_otlp_line_among_otlp_lines_fails_the_file_naming_the_line() {
    let r = decode_input(
        &text("{\"resourceSpans\": []}\n\n{\"note\": 1}\n{\"resourceSpans\": []}\n"),
        None,
    );
    match r {
        Err(OtelError::Json(msg)) => assert!(msg.starts_with("line 3:"), "{msg}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn unparseable_json_is_a_json_error_naming_the_line() {
    let r = decode_input(
        &text("{\"resourceSpans\": []}\n{\"resourceSpans\": [\n"),
        Some("t.json"),
    );
    match r {
        Err(OtelError::Json(msg)) => assert!(msg.starts_with("line 2:"), "{msg}"),
        other => panic!("{other:?}"),
    }
}

/// With the `protobuf` feature off; with it on,
/// `binary_input.rs::the_error_contract_holds` pins binary bodies instead.
#[cfg(not(feature = "protobuf"))]
#[test]
fn binary_bytes_depend_on_the_protobuf_extension() {
    let bin = [0x0a, 0xff, 0x00, 0x12, 0x80];
    for name in ["t.binpb", "t.pb", "T.PROTOBUF"] {
        let r = decode_input(&bin, Some(name));
        assert!(
            matches!(r, Err(OtelError::FeatureDisabled("protobuf"))),
            "{name}: {r:?}"
        );
    }
    for name in [None, Some("t.json"), Some("t.bin")] {
        let r = decode_input(&bin, name);
        assert!(matches!(r, Err(OtelError::Json(_))), "{name:?}: {r:?}");
    }
}

#[test]
fn bounded_fuzz_never_panics() {
    let mut seeds = vec![
        encoding("body.json"),
        encoding("lines.jsonl"),
        encoding("body.json.gz"),
    ];
    seeds.push(encoding("two-members.jsonl.gz"));
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let mut panics = Vec::new();
    for (s, seed) in seeds.iter().enumerate() {
        for round in 0..150 {
            let mut b = seed.clone();
            match next() % 3 {
                0 => b.truncate((next() as usize) % (b.len() + 1)),
                1 => {
                    for _ in 0..8 {
                        let i = (next() as usize) % b.len();
                        b[i] = next() as u8;
                    }
                }
                _ => b.insert((next() as usize) % (b.len() + 1), next() as u8),
            }
            let name = ["x.json", "x.binpb", "x.gz"][round % 3];
            if panic::catch_unwind(AssertUnwindSafe(|| decode_input(&b, Some(name)))).is_err() {
                panics.push(format!("seed {s} round {round}"));
            }
        }
    }
    panic::set_hook(hook);
    assert!(panics.is_empty(), "{panics:?}");
}

#[cfg(feature = "compression")]
fn too_large(err: &OtelError, limit: u64, text: &str) -> bool {
    matches!(err, OtelError::TooLarge { limit: l } if *l == limit)
        && err.is_too_large()
        && !err.is_not_otlp()
        && err.to_string() == format!("cannot decompress: decompressed output exceeds {text}")
}

#[cfg(feature = "compression")]
#[test]
fn a_caller_limit_bounds_gzip() {
    let gz = encoding("body.json.gz");
    let plain = encoding("body.json").len() as u64;
    assert_eq!(
        decode_input_with_limit(&gz, Some("body.json.gz"), plain).unwrap(),
        decode_input(&gz, Some("body.json.gz")).unwrap()
    );
    let err = decode_input_with_limit(&gz, Some("body.json.gz"), 16 << 10).unwrap_err();
    assert!(too_large(&err, 16 << 10, "16 KiB"), "{err:?}");
    let err = decode_input_with_limit(&gz, None, plain - 1).unwrap_err();
    assert!(
        too_large(&err, plain - 1, &format!("{} B", plain - 1)),
        "{err:?}"
    );
}

#[cfg(feature = "compression")]
#[test]
fn a_caller_limit_bounds_zstd() {
    let z = encoding("synthetic-fork.ndjson.zst");
    assert!(decode_input(&z, None).is_ok());
    let err = decode_input_with_limit(&z, None, 4 << 10).unwrap_err();
    assert!(too_large(&err, 4 << 10, "4 KiB"), "{err:?}");
}

#[cfg(feature = "compression")]
#[test]
fn a_caller_limit_is_one_budget_across_nested_layers() {
    use std::io::Write;
    let gz = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    };
    let body = encoding("body.json");
    let once = gz(&body);
    let twice = gz(&once);
    let need = (once.len() + body.len()) as u64;
    assert_eq!(
        decode_input_with_limit(&twice, None, need).unwrap(),
        m0_lines()[..1]
    );
    // Each layer alone fits; together they overrun, and the error names
    // the whole limit rather than what the inner layer had left.
    let err = decode_input_with_limit(&twice, None, need - 1).unwrap_err();
    assert!(
        too_large(&err, need - 1, &format!("{} B", need - 1)),
        "{err:?}"
    );
}

#[cfg(feature = "compression")]
#[test]
fn a_caller_limit_is_one_budget_across_collector_frames() {
    let framed = encoding("synthetic-fork-json-frames-zstd.pb");
    assert_eq!(decode_input(&framed, None).unwrap().len(), 4);
    // Every frame alone is under 16 KiB; the first two together are not.
    let err = decode_input_with_limit(&framed, None, 16 << 10).unwrap_err();
    assert!(too_large(&err, 16 << 10, "16 KiB"), "{err:?}");
}

#[cfg(feature = "compression")]
fn gzip(b: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

#[cfg(feature = "compression")]
fn zstd(b: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(b, ruzstd::encoding::CompressionLevel::Fastest)
}

#[cfg(feature = "compression")]
const EMPTY_TRACES: &[u8] = br#"{"resourceSpans":[]}"#;

#[test]
fn uncompressed_input_is_not_limited() {
    for name in ["body.json", "lines.jsonl", "crlf-bom.jsonl"] {
        let bytes = encoding(name);
        assert_eq!(
            decode_input_with_limit(&bytes, Some(name), 0).unwrap(),
            decode_input(&bytes, Some(name)).unwrap(),
            "{name}"
        );
    }
    #[cfg(feature = "protobuf")]
    for name in ["synthetic-fork-first.binpb", "synthetic-fork-frames.pb"] {
        let bytes = encoding(name);
        assert_eq!(
            decode_input_with_limit(&bytes, Some(name), 0).unwrap(),
            decode_input(&bytes, Some(name)).unwrap(),
            "{name}"
        );
    }
}

#[cfg(feature = "compression")]
#[test]
fn a_zero_limit_refuses_any_decompressed_byte() {
    for c in [gzip(EMPTY_TRACES), zstd(EMPTY_TRACES)] {
        let err = decode_input_with_limit(&c, None, 0).unwrap_err();
        assert!(too_large(&err, 0, "0 B"), "{err:?}");
    }
    // Nothing decompressed is within a zero limit.
    let r = decode_input_with_limit(&gzip(b""), None, 0);
    assert!(matches!(r, Err(OtelError::NotOtlp)), "{r:?}");
}

#[cfg(feature = "compression")]
#[test]
fn the_limit_counts_decompressed_bytes_not_input_bytes() {
    let limit = EMPTY_TRACES.len() as u64;
    for c in [gzip(EMPTY_TRACES), zstd(EMPTY_TRACES)] {
        assert!(c.len() as u64 > limit);
        assert_eq!(
            decode_input_with_limit(&c, None, limit).unwrap(),
            vec![json!({"resourceSpans": []})]
        );
    }
}

#[cfg(feature = "compression")]
#[test]
fn a_caller_limit_bounds_zstd_exactly() {
    let body = encoding("body.json");
    let z = zstd(&body);
    let plain = body.len() as u64;
    assert_eq!(
        decode_input_with_limit(&z, None, plain).unwrap(),
        m0_lines()[..1]
    );
    let err = decode_input_with_limit(&z, None, plain - 1).unwrap_err();
    assert!(
        too_large(&err, plain - 1, &format!("{} B", plain - 1)),
        "{err:?}"
    );
}

#[cfg(feature = "compression")]
#[test]
fn gzip_inside_zstd_shares_one_budget() {
    let inner = gzip(EMPTY_TRACES);
    let outer = zstd(&inner);
    let need = (inner.len() + EMPTY_TRACES.len()) as u64;
    assert_eq!(
        decode_input_with_limit(&outer, None, need).unwrap().len(),
        1
    );
    let err = decode_input_with_limit(&outer, None, need - 1).unwrap_err();
    assert!(
        too_large(&err, need - 1, &format!("{} B", need - 1)),
        "{err:?}"
    );
}

#[cfg(feature = "compression")]
#[test]
fn the_layer_bound_is_checked_before_the_limit() {
    let mut layers = vec![EMPTY_TRACES.to_vec()];
    for _ in 0..5 {
        layers.push(gzip(layers.last().unwrap()));
    }
    // The four outer layers' output exhausts the budget exactly, so a
    // fifth decompression would overrun it.
    let four: u64 = layers[1..5].iter().map(|l| l.len() as u64).sum();
    let err = decode_input_with_limit(&layers[5], None, four).unwrap_err();
    assert!(
        matches!(&err, OtelError::Decompress(m) if m == "more than 4 nested compression layers"),
        "{err:?}"
    );
    assert!(!err.is_too_large());
}

#[cfg(feature = "compression")]
#[test]
fn a_corrupt_stream_is_decompress_at_any_limit() {
    let z = zstd(&encoding("body.json"));
    for (bytes, limit) in [
        (vec![0x1f, 0x8b, 0x08, 0x00, 0x01, 0x02], 0),
        (vec![0x1f, 0x8b, 0x08, 0x00, 0x01, 0x02], u64::MAX),
        (z[..z.len() - 3].to_vec(), 1 << 20),
    ] {
        let err = decode_input_with_limit(&bytes, None, limit).unwrap_err();
        assert!(
            matches!(err, OtelError::Decompress(_)) && !err.is_too_large(),
            "{limit}: {err:?}"
        );
    }
}

#[cfg(feature = "compression")]
#[test]
fn a_stream_corrupt_past_the_limit_is_too_large() {
    // Missing its CRC trailer, so corrupt only after 4096 good bytes.
    let gz = gzip(&[b' '; 4096]);
    let cut = &gz[..gz.len() - 8];
    let err = decode_input_with_limit(cut, None, 1024).unwrap_err();
    assert!(too_large(&err, 1024, "1 KiB"), "{err:?}");
    let err = decode_input_with_limit(cut, None, 1 << 20).unwrap_err();
    assert!(matches!(err, OtelError::Decompress(_)), "{err:?}");
}

#[cfg(feature = "compression")]
#[test]
fn the_largest_limit_does_not_overflow() {
    for c in [gzip(EMPTY_TRACES), zstd(EMPTY_TRACES)] {
        assert_eq!(
            decode_input_with_limit(&c, None, u64::MAX).unwrap().len(),
            1
        );
    }
}

#[test]
fn decode_input_is_the_limit_at_one_gib() {
    for name in ["body.json", "body.json.gz", "synthetic-fork.ndjson.zst"] {
        let bytes = encoding(name);
        let with = decode_input_with_limit(&bytes, Some(name), 1 << 30);
        match decode_input(&bytes, Some(name)) {
            Ok(v) => assert_eq!(with.unwrap(), v, "{name}"),
            Err(e) => assert_eq!(with.unwrap_err().to_string(), e.to_string(), "{name}"),
        }
    }
}

#[cfg(not(feature = "compression"))]
#[test]
fn compressed_input_without_the_feature_ignores_the_limit() {
    for bytes in [
        encoding("body.json.gz"),
        encoding("synthetic-fork.ndjson.zst"),
    ] {
        for limit in [0, u64::MAX] {
            let r = decode_input_with_limit(&bytes, None, limit);
            assert!(
                matches!(r, Err(OtelError::FeatureDisabled("compression"))),
                "{limit}: {r:?}"
            );
        }
    }
}
