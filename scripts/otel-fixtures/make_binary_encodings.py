#!/usr/bin/env python3
"""Build PR 3's binary fixtures in test-fixtures/otel/encodings/.

Source: test-fixtures/otel/openrouter/synthetic-fork.ndjson (4 deliveries).
Protobuf bytes come from otlp_pb.py (shares no code with the Rust crate);
zstd comes from the `zstd` CLI. Maintainer-run; the outputs are committed.

Usage: python3 scripts/otel-fixtures/make_binary_encodings.py
"""
import json
import pathlib
import struct
import subprocess
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import otlp_pb  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[2]
SRC = ROOT / "test-fixtures/otel/openrouter/synthetic-fork.ndjson"
OUT = ROOT / "test-fixtures/otel/encodings"


def zstd(data: bytes) -> bytes:
    return subprocess.run(
        ["zstd", "-q", "-c", "-19"], input=data, check=True, capture_output=True
    ).stdout


def frame(payload: bytes) -> bytes:
    """The Collector file exporter's framing: 4-byte big-endian length."""
    return struct.pack(">I", len(payload)) + payload


def nested(data: bytes, layers: int) -> bytes:
    for _ in range(layers):
        data = zstd(data)
    return data


def main() -> None:
    lines = [line for line in SRC.read_text().splitlines() if line.strip()]
    bodies = [otlp_pb.encode(json.loads(line)) for line in lines]
    empty = b'{"resourceSpans":[]}'
    files = {
        # compression: zstd, native (whole stream), format: json
        "synthetic-fork.ndjson.zst": zstd(SRC.read_bytes()),
        # one OTLP/HTTP protobuf body, and the same body zstd-compressed
        "synthetic-fork-first.binpb": bodies[0],
        "synthetic-fork-first.binpb.zst": zstd(bodies[0]),
        # format: proto
        "synthetic-fork-frames.pb": b"".join(frame(b) for b in bodies),
        # format: proto, compression: zstd (per message)
        "synthetic-fork-frames-zstd.pb": b"".join(frame(zstd(b)) for b in bodies),
        # format: json, compression: zstd (per message, length-prefixed)
        "synthetic-fork-json-frames-zstd.pb": b"".join(frame(zstd(line.encode())) for line in lines),
        # compression: zstd, native (whole stream), format: proto
        "synthetic-fork-frames.pb.zst": zstd(b"".join(frame(b) for b in bodies)),
        # nesting bound (input::MAX_LAYERS): 4 layers decode, 5 do not
        "nested-4.json.zst": nested(empty, 4),
        "nested-5.json.zst": nested(empty, 5),
    }
    for name, data in files.items():
        (OUT / name).write_bytes(data)
        print(f"{name}: {len(data)} bytes")


if __name__ == "__main__":
    main()
