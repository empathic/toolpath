"""OTLP protobuf request body -> OTLP/JSON, via Google's protobuf library.

Independent of the Rust decoder under test. OTLP/JSON is proto3 JSON with
lowerCamelCase keys and integer enums, except that trace and span ids are
hex instead of base64; this fixes the ids up.

`rerank_body` is the capture's fallback for timestamps (see README,
"Determinism"): it rewrites the body's times through the same rank map
`otlp_json.normalize_times` applied to the JSON, and leaves the body's bytes
untouched when that map is the identity (the fixture clock's usual case).
"""
import base64

from google.protobuf.json_format import MessageToDict
from opentelemetry.proto.collector.logs.v1.logs_service_pb2 import ExportLogsServiceRequest
from opentelemetry.proto.collector.trace.v1.trace_service_pb2 import ExportTraceServiceRequest

_ID_KEYS = {"traceId", "spanId", "parentSpanId"}
_REQUESTS = {"traces": ExportTraceServiceRequest, "logs": ExportLogsServiceRequest}


def _hex_ids(node):
    if isinstance(node, dict):
        return {
            k: base64.b64decode(v).hex() if k in _ID_KEYS and isinstance(v, str) else _hex_ids(v)
            for k, v in node.items()
        }
    if isinstance(node, list):
        return [_hex_ids(x) for x in node]
    return node


def parse(body: bytes, signal: str):
    message = _REQUESTS[signal]()
    message.ParseFromString(body)
    return message


def body_to_otlp_json(body: bytes, signal: str) -> dict:
    return _hex_ids(MessageToDict(parse(body, signal), use_integers_for_enums=True))


def _timed(message, signal):
    """Every (message, field) pair of the request that holds a UnixNano time."""
    if signal == "traces":
        for rs in message.resource_spans:
            for ss in rs.scope_spans:
                for span in ss.spans:
                    yield span, "start_time_unix_nano"
                    yield span, "end_time_unix_nano"
                    for event in span.events:
                        yield event, "time_unix_nano"
    else:
        for rl in message.resource_logs:
            for sl in rl.scope_logs:
                for record in sl.log_records:
                    yield record, "time_unix_nano"
                    yield record, "observed_time_unix_nano"


def rerank_body(body: bytes, signal: str, rank: dict) -> bytes:
    """`body` with every non-zero time t replaced by rank[t]. The body comes
    back unchanged, byte for byte, when `rank` maps every time to itself."""
    if all(k == v for k, v in rank.items()):
        return body
    message = parse(body, signal)
    for holder, field in _timed(message, signal):
        t = getattr(holder, field)
        if t:
            if t not in rank:
                raise SystemExit(f"{signal} body carries time {t}, which the JSON does not")
            setattr(holder, field, rank[t])
    return message.SerializeToString(deterministic=True)
