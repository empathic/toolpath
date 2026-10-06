//! decode_protobuf / encode_protobuf: known answers from an independent
//! wire encoder, signal discrimination, strictness, and a bounded fuzz
//! sweep. Known-answer bytes come from an independent hand-rolled wire
//! encoder (no protobuf library) and were cross-checked byte-for-byte
//! against prost 0.14.4.
#![cfg(feature = "protobuf")]

use serde_json::{Value, json};
use std::panic::{self, AssertUnwindSafe};
use toolpath_otel::{OtelError, decode_protobuf, encode_protobuf};

const TRACES_HEX: &str = "0a6d0a0c0a0a0a0373766312030a0178125d0a060a017312013112530a100102030405060708090a0b0c0d0e0f101208a1a2a3a4a5a6a7a82a0463686174300339e80300000000000041d0070000000000004a080a016b12030a01764a070a016e1202182a7a081204626f6f6d1802";
const LOGS_HEX: &str = "0a5f0a0c0a0a0a0373766312030a0178124f0a060a0173120131124509050000000000000010092a040a02686932070a0161120210014a100102030405060708090a0b0c0d0e0f105208a1a2a3a4a5a6a7a8620d67656e5f61692e63686f696365";
/// ExportLogsServiceRequest { resource_logs: [{ scope_logs: [{ log_records:
/// [{ body: "hi" }] }] }] }: time 0, no severity, no ids.
const BODY_ONLY_HEX: &str = "0a0a120812062a040a026869";

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn traces_json() -> Value {
    json!({"resourceSpans": [{
        "resource": {"attributes": [{"key": "svc", "value": {"stringValue": "x"}}]},
        "scopeSpans": [{"scope": {"name": "s", "version": "1"}, "spans": [{
            "traceId": "0102030405060708090a0b0c0d0e0f10", "spanId": "a1a2a3a4a5a6a7a8",
            "name": "chat", "kind": 3, "startTimeUnixNano": "1000", "endTimeUnixNano": "2000",
            "attributes": [{"key": "k", "value": {"stringValue": "v"}},
                           {"key": "n", "value": {"intValue": "42"}}],
            "status": {"message": "boom", "code": 2}}]}]}]})
}

fn logs_json() -> Value {
    json!({"resourceLogs": [{
        "resource": {"attributes": [{"key": "svc", "value": {"stringValue": "x"}}]},
        "scopeLogs": [{"scope": {"name": "s", "version": "1"}, "logRecords": [{
            "timeUnixNano": "5", "severityNumber": 9, "body": {"stringValue": "hi"},
            "attributes": [{"key": "a", "value": {"boolValue": true}}],
            "traceId": "0102030405060708090a0b0c0d0e0f10", "spanId": "a1a2a3a4a5a6a7a8",
            "eventName": "gen_ai.choice"}]}]}]})
}

#[test]
fn decodes_a_traces_body_to_canonical_otlp_json() {
    assert_eq!(decode_protobuf(&unhex(TRACES_HEX)).unwrap(), traces_json());
}

#[test]
fn decodes_an_sdk_shaped_logs_body_as_logs() {
    assert_eq!(decode_protobuf(&unhex(LOGS_HEX)).unwrap(), logs_json());
}

#[test]
fn encodes_to_the_independent_bytes() {
    assert_eq!(encode_protobuf(&traces_json()).unwrap(), unhex(TRACES_HEX));
    assert_eq!(encode_protobuf(&logs_json()).unwrap(), unhex(LOGS_HEX));
}

#[test]
fn a_body_only_log_record_never_decodes_as_traces() {
    assert_eq!(
        decode_protobuf(&unhex(BODY_ONLY_HEX)).unwrap(),
        json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [{"body": {"stringValue": "hi"}}]}]}]})
    );
}

#[test]
fn an_empty_request_is_empty_traces() {
    assert_eq!(decode_protobuf(&[]).unwrap(), json!({"resourceSpans": []}));
}

/// Without a span or log record the two requests are the same bytes, so
/// the signal reads as traces; the resources and scopes are kept.
#[test]
fn a_request_without_records_keeps_its_resources_and_scopes() {
    let resource = json!({"attributes": [{"key": "a", "value": {"stringValue": "b"}}]});
    let scope = json!({"name": "s", "version": "1"});
    let logs = json!({"resourceLogs": [{"resource": resource, "scopeLogs": [{"scope": scope}]}]});
    let bytes = encode_protobuf(&logs).unwrap();
    let traces =
        json!({"resourceSpans": [{"resource": resource, "scopeSpans": [{"scope": scope}]}]});
    assert_eq!(encode_protobuf(&traces).unwrap(), bytes);
    let decoded = decode_protobuf(&bytes).unwrap();
    assert_eq!(decoded, traces);
    assert_eq!(encode_protobuf(&decoded).unwrap(), bytes);
    let resource_only = json!({"resourceLogs": [{"resource": resource}]});
    assert_eq!(
        decode_protobuf(&encode_protobuf(&resource_only).unwrap()).unwrap(),
        json!({"resourceSpans": [{"resource": resource}]})
    );
}

#[test]
fn one_bad_span_id_fails_the_whole_request_naming_both_decodes() {
    let mut v = traces_json();
    v["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["spanId"] = json!("a1a2a3a4a5a6a7");
    let err = decode_protobuf(&encode_protobuf(&v).unwrap()).unwrap_err();
    assert!(!err.is_not_otlp(), "a wire-valid traces body is OTLP");
    let msg = match err {
        OtelError::Protobuf(msg) => msg,
        other => panic!("expected Protobuf, got {other:?}"),
    };
    assert!(msg.contains("7-byte span_id"), "{msg}");
    assert!(msg.contains("not an OTLP logs request"), "{msg}");
}

#[test]
fn bytes_that_are_neither_request_are_not_otlp_body() {
    let err = decode_protobuf(&[0xff, 0x01, 0x02]).unwrap_err();
    assert!(
        matches!(&err, OtelError::NotOtlpBody(m)
            if m.contains("not an OTLP traces request") && m.contains("not an OTLP logs request")),
        "{err:?}"
    );
    assert!(err.is_not_otlp());
}

#[test]
fn encode_errors_stay_protobuf() {
    let err = encode_protobuf(&json!({"x": 1})).unwrap_err();
    assert!(matches!(&err, OtelError::Protobuf(_)), "{err:?}");
    assert!(!err.is_not_otlp());
}

#[test]
fn encoding_two_signals_is_an_error() {
    let mut v = traces_json();
    v["resourceLogs"] = logs_json()["resourceLogs"].clone();
    let err = encode_protobuf(&v).unwrap_err();
    assert!(
        err.to_string().contains("one request carries one signal"),
        "{err}"
    );
    assert!(encode_protobuf(&json!({"resourceMetrics": []})).is_err());
    assert_eq!(
        encode_protobuf(&json!({"resourceSpans": null})).unwrap(),
        Vec::<u8>::new()
    );
}

#[test]
fn encoding_is_strict_and_names_the_path() {
    let span = "/resourceSpans/0/scopeSpans/0/spans/0";
    for (ptr, bad, needle) in [
        (
            format!("{span}/traceId"),
            json!("zz"),
            "traceId: expected a hex id",
        ),
        (
            format!("{span}/traceId"),
            json!("abc"),
            "traceId: expected a hex id",
        ),
        (
            format!("{span}/attributes/0/value"),
            json!({"stringValue": "a", "intValue": "1"}),
            "more than one value",
        ),
        (
            format!("{span}/attributes/0/value"),
            json!({"intValue": "x"}),
            "intValue: expected a decimal integer",
        ),
        (
            format!("{span}/attributes/0/value"),
            json!({"intValue": 1.5}),
            "intValue: expected an integer",
        ),
        (
            format!("{span}/attributes/0/value"),
            json!("text"),
            "value: expected an object",
        ),
        (
            format!("{span}/kind"),
            json!("SPAN_KIND_NOPE"),
            "kind: unknown enum name",
        ),
        (
            format!("{span}/startTimeUnixNano"),
            json!("-1"),
            "startTimeUnixNano: out of range",
        ),
        (
            "/resourceSpans/0/scopeSpans/0/spans".to_string(),
            json!({}),
            "spans: expected an array",
        ),
    ] {
        let mut v = traces_json();
        *v.pointer_mut(&ptr).unwrap() = bad;
        let err = encode_protobuf(&v).unwrap_err().to_string();
        assert!(err.contains(needle), "{ptr}: {err}");
    }
}

#[test]
fn enums_accept_names_in_any_case_and_numbers_as_strings() {
    let span = "/resourceSpans/0/scopeSpans/0/spans/0";
    let mut v = traces_json();
    *v.pointer_mut(&format!("{span}/kind")).unwrap() = json!("SPAN_KIND_CLIENT");
    *v.pointer_mut(&format!("{span}/status/code")).unwrap() = json!("status_code_error");
    assert_eq!(encode_protobuf(&v).unwrap(), unhex(TRACES_HEX));
    *v.pointer_mut(&format!("{span}/kind")).unwrap() = json!("3");
    *v.pointer_mut(&format!("{span}/status/code")).unwrap() = json!(2);
    assert_eq!(encode_protobuf(&v).unwrap(), unhex(TRACES_HEX));
}

#[test]
fn every_any_value_kind_round_trips() {
    let v = json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [{"body": {"kvlistValue": {"values": [
        {"key": "d", "value": {"doubleValue": 1.5}},
        {"key": "nan", "value": {"doubleValue": "NaN"}},
        {"key": "inf", "value": {"doubleValue": "-Infinity"}},
        {"key": "b", "value": {"bytesValue": "AQID/w=="}},
        {"key": "t", "value": {"boolValue": false}},
        {"key": "s", "value": {"stringValueStrindex": 4}},
        {"key": "a", "value": {"arrayValue": {"values": [{"intValue": "-7"}, {}]}}},
        {"key": "e", "value": {"arrayValue": {"values": []}}}
    ]}}}]}]}]});
    let rt = decode_protobuf(&encode_protobuf(&v).unwrap()).unwrap();
    assert_eq!(rt, v);
}

#[test]
fn bytes_values_accept_url_safe_and_unpadded_base64() {
    let with = |b: &str| json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [{"body": {"bytesValue": b}}]}]}]});
    let rt = decode_protobuf(&encode_protobuf(&with("AQID_w")).unwrap()).unwrap();
    assert_eq!(rt, with("AQID/w=="));
    for ok in ["", "AQ", "AQI", "AQID", "AQIDBA"] {
        assert!(encode_protobuf(&with(ok)).is_ok(), "{ok}");
    }
    for bad in ["A", "AQ*D", "AQIDB"] {
        assert!(encode_protobuf(&with(bad)).is_err(), "{bad}");
    }
}

#[test]
fn any_value_nesting_is_bounded_on_encode() {
    let mut v = json!({"stringValue": "leaf"});
    for _ in 0..70 {
        v = json!({"arrayValue": {"values": [v]}});
    }
    let d = json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [{"body": v}]}]}]});
    let err = encode_protobuf(&d).unwrap_err().to_string();
    assert!(err.contains("nested deeper than"), "{err}");
}

#[test]
fn decode_then_encode_is_a_fixed_point_on_the_known_bytes() {
    for hex in [TRACES_HEX, LOGS_HEX, BODY_ONLY_HEX] {
        let bytes = unhex(hex);
        assert_eq!(
            encode_protobuf(&decode_protobuf(&bytes).unwrap()).unwrap(),
            bytes
        );
    }
}

/// Truncations, byte flips and xorshift noise: `Ok` or `Err`, never a panic.
#[test]
fn garbage_and_truncated_bytes_never_panic() {
    let mut inputs: Vec<Vec<u8>> = Vec::new();
    for hex in [TRACES_HEX, LOGS_HEX, BODY_ONLY_HEX] {
        let src = unhex(hex);
        for cut in 0..src.len() {
            inputs.push(src[..cut].to_vec());
        }
        for i in 0..src.len() {
            for x in [0x00u8, 0xff, 0x80, 0x0a, 0x7a] {
                let mut m = src.clone();
                m[i] = x;
                inputs.push(m);
            }
        }
    }
    let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..3000 {
        let n = (s % 97) as usize;
        let mut b = Vec::with_capacity(n);
        for _ in 0..n {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            b.push(s as u8);
        }
        inputs.push(b);
        s = s.wrapping_add(1);
    }
    for (i, bytes) in inputs.iter().enumerate() {
        let r = panic::catch_unwind(AssertUnwindSafe(|| decode_protobuf(bytes)));
        assert!(r.is_ok(), "input {i} panicked: {bytes:02x?}");
    }
}
