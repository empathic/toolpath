"""Finished spans -> OTLP/JSON exactly as an OTLP/HTTP exporter would send them.

The spans are encoded with the OTLP exporter's own protobuf encoder
(`encode_spans` -> ExportTraceServiceRequest), then rendered with
`MessageToDict` (lowerCamelCase keys, 64-bit integers as decimal strings,
enums as integers) and trace/span ids rewritten from base64 to lowercase hex,
which is what OTLP/JSON requires. PR 3 writes the exporter's request body
beside it as `.binpb` (capture.py, pb_to_json.py).

Determinism: ids come from FixtureIds (sha256 of the scenario name and a
counter); every *UnixNano timestamp is replaced by its rank, so re-runs are
byte-identical while order is preserved. The fixture clock makes the SDK
stamp those ranks in the first place, so the exporter body (which carries
the SDK's times, not the JSON's) agrees with the JSON without a rewrite.
"""
import base64
import hashlib
import importlib
import itertools
import json
import threading

from google.protobuf.json_format import MessageToDict
from opentelemetry.exporter.otlp.proto.common.trace_encoder import encode_spans
from opentelemetry.sdk.trace.id_generator import IdGenerator

ID_KEYS = ("traceId", "spanId", "parentSpanId")
TIME_KEYS = ("startTimeUnixNano", "endTimeUnixNano", "timeUnixNano", "observedTimeUnixNano")
BASE_NS = 1_800_000_000_000_000_000
STEP_NS = 1_000_000


class FixtureIds(IdGenerator):
    """Deterministic, non-zero ids: sha256("<seed>/<n>") truncated."""

    def __init__(self, seed):
        self.seed = seed
        self.n = 0

    def _next(self, nbytes):
        self.n += 1
        digest = hashlib.sha256(f"{self.seed}/{self.n}".encode()).digest()
        return int.from_bytes(digest[:nbytes], "big") or 1

    def generate_span_id(self):
        return self._next(8)

    def generate_trace_id(self):
        return self._next(16)


# Modules that bind `from time import time_ns` and stamp spans and span
# events, or log records, with it (opentelemetry-sdk / -api 1.45.0).
TRACE_CLOCK_MODULES = ("opentelemetry.sdk.trace",)
LOG_CLOCK_MODULES = ("opentelemetry._logs._internal", "opentelemetry.sdk._logs._internal")


def install_fixture_clock(logs):
    """Make the OpenTelemetry SDK read time from a counter: BASE_NS, then one
    STEP_NS later per reading. When every reading lands in the capture (the
    usual case), normalize_times finds the times already ranked and changes
    nothing, so the exporter's request body carries exactly the JSON's times.
    `logs` also clocks log records; pass it only when they are captured
    (google-genai emits a details record even in span mode, and a reading
    that never reaches the capture leaves a gap in the ranks). Only
    OpenTelemetry's own `time_ns` names are patched; `time.time_ns` itself,
    which HTTP clients and the mocks may read, is left alone."""
    counter = itertools.count()
    lock = threading.Lock()

    def time_ns():
        with lock:
            return BASE_NS + next(counter) * STEP_NS

    for name in TRACE_CLOCK_MODULES + (LOG_CLOCK_MODULES if logs else ()):
        module = importlib.import_module(name)
        if not hasattr(module, "time_ns"):
            raise SystemExit(f"fixture clock: {name} no longer binds time_ns; "
                             "update the *_CLOCK_MODULES lists")
        module.time_ns = time_ns


def to_otlp_json(spans):
    doc = MessageToDict(
        encode_spans(spans),
        use_integers_for_enums=True,
        preserving_proto_field_name=False,
    )
    _hex_ids(doc)
    return doc


def _hex_ids(node):
    if isinstance(node, dict):
        for key, value in node.items():
            if key in ID_KEYS and isinstance(value, str):
                node[key] = base64.b64decode(value).hex()
            else:
                _hex_ids(value)
    elif isinstance(node, list):
        for item in node:
            _hex_ids(item)


def _times(node, out):
    if isinstance(node, dict):
        for key, value in node.items():
            if key in TIME_KEYS:
                out.add(int(value))
            else:
                _times(value, out)
    elif isinstance(node, list):
        for item in node:
            _times(item, out)


def _rewrite(node, rank):
    if isinstance(node, dict):
        for key, value in node.items():
            if key in TIME_KEYS:
                node[key] = str(rank[int(value)])
            else:
                _rewrite(value, rank)
    elif isinstance(node, list):
        for item in node:
            _rewrite(item, rank)


def normalize_times(doc):
    """Replace every time in `doc` (a document or a list of documents ranked
    together) by its rank; return the map applied (identity when the times
    were already ranked)."""
    seen = set()
    _times(doc, seen)
    rank = {t: BASE_NS + i * STEP_NS for i, t in enumerate(sorted(seen))}
    _rewrite(doc, rank)
    return rank


def dumps(doc):
    return json.dumps(doc, indent=2, ensure_ascii=False) + "\n"
