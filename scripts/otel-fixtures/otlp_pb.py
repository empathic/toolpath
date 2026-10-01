"""Independent OTLP/JSON -> OTLP protobuf encoder (no protobuf library).

Builds committed binary fixtures so the Rust decoder is tested against
bytes it did not produce. It covers the fields the fixtures use and ignores
every other key. On the M0 `synthetic-fork.ndjson` deliveries its output is
byte-identical to prost's (checked when this plan was written).
"""
import base64
import struct


def _varint(n):
    if n < 0:
        n += 1 << 64
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def _key(field, wire):
    return _varint((field << 3) | wire)


def _len(field, payload):
    return _key(field, 2) + _varint(len(payload)) + payload


def _str(field, s):
    return _len(field, s.encode()) if s else b""


def _uint(field, n):
    return _key(field, 0) + _varint(int(n)) if n else b""


def _fixed64(field, n):
    return _key(field, 1) + struct.pack("<Q", int(n)) if n and int(n) else b""


def _fixed32(field, n):
    return _key(field, 5) + struct.pack("<I", int(n)) if n else b""


def _hex(field, s):
    return _len(field, bytes.fromhex(s)) if s else b""


def any_value(v):
    if not v:
        return b""
    if v.get("stringValue") is not None:
        return _len(1, v["stringValue"].encode())
    if v.get("boolValue") is not None:
        return _key(2, 0) + _varint(1 if v["boolValue"] else 0)
    if v.get("intValue") is not None:
        return _key(3, 0) + _varint(int(v["intValue"]))
    if v.get("doubleValue") is not None:
        return _key(4, 1) + struct.pack("<d", float(v["doubleValue"]))
    if v.get("arrayValue") is not None:
        return _len(5, b"".join(_len(1, any_value(x)) for x in v["arrayValue"].get("values") or []))
    if v.get("kvlistValue") is not None:
        return _len(6, b"".join(_len(1, key_value(x)) for x in v["kvlistValue"].get("values") or []))
    if v.get("bytesValue") is not None:
        return _len(7, base64.b64decode(v["bytesValue"]))
    return b""


def key_value(kv):
    out = _str(1, kv.get("key") or "")
    if kv.get("value") is not None:
        out += _len(2, any_value(kv["value"]))
    return out


def _attrs(field, items):
    return b"".join(_len(field, key_value(kv)) for kv in items or [])


def _resource(r):
    return _attrs(1, r.get("attributes")) + _uint(2, r.get("droppedAttributesCount"))


def _scope(s):
    return (_str(1, s.get("name") or "") + _str(2, s.get("version") or "")
            + _attrs(3, s.get("attributes")) + _uint(4, s.get("droppedAttributesCount")))


def _event(e):
    return _fixed64(1, e.get("timeUnixNano")) + _str(2, e.get("name") or "") + _attrs(3, e.get("attributes"))


def _status(s):
    return _str(2, s.get("message") or "") + _uint(3, s.get("code"))


def _span(s):
    out = (_hex(1, s.get("traceId")) + _hex(2, s.get("spanId")) + _str(3, s.get("traceState") or "")
           + _hex(4, s.get("parentSpanId")) + _str(5, s.get("name") or "") + _uint(6, s.get("kind"))
           + _fixed64(7, s.get("startTimeUnixNano")) + _fixed64(8, s.get("endTimeUnixNano"))
           + _attrs(9, s.get("attributes")) + _uint(10, s.get("droppedAttributesCount"))
           + b"".join(_len(11, _event(e)) for e in s.get("events") or []))
    if s.get("status") is not None:
        out += _len(15, _status(s["status"]))
    return out + _fixed32(16, s.get("flags"))


def _log(r):
    out = _fixed64(1, r.get("timeUnixNano")) + _uint(2, r.get("severityNumber")) + _str(3, r.get("severityText") or "")
    if r.get("body") is not None:
        out += _len(5, any_value(r["body"]))
    return (out + _attrs(6, r.get("attributes")) + _fixed32(8, r.get("flags")) + _hex(9, r.get("traceId"))
            + _hex(10, r.get("spanId")) + _fixed64(11, r.get("observedTimeUnixNano"))
            + _str(12, r.get("eventName") or ""))


def _container(entry, scopes_key, leaves_key, leaf):
    out = _len(1, _resource(entry["resource"])) if entry.get("resource") is not None else b""
    for sc in entry.get(scopes_key) or []:
        inner = _len(1, _scope(sc["scope"])) if sc.get("scope") is not None else b""
        inner += b"".join(_len(2, leaf(x)) for x in sc.get(leaves_key) or [])
        out += _len(2, inner)
    return out


def encode(delivery):
    """One OTLP/JSON delivery (traces or logs, not both) -> protobuf bytes."""
    if delivery.get("resourceLogs") is not None:
        return b"".join(_len(1, _container(e, "scopeLogs", "logRecords", _log)) for e in delivery["resourceLogs"])
    return b"".join(_len(1, _container(e, "scopeSpans", "spans", _span)) for e in delivery.get("resourceSpans") or [])
