# Encoding fixtures

## Text forms

`body.json`, `lines.jsonl`, `crlf-bom.jsonl`, `body.json.gz` and
`two-members.jsonl.gz` exercise `decode_input`'s JSON, JSON-lines and gzip
rules in `crates/toolpath-otel/tests/input.rs`; `binary_input.rs` under
`crates/toolpath-otel/src/tests/` covers the binary forms.

## Binary forms

Built from `../openrouter/synthetic-fork.ndjson` (4 deliveries) by the OTel
fixture tooling (`scripts/otel-fixtures/`, which lands separately). Protobuf
bytes come from an independent Python encoder that shares no code with the
Rust crate; zstd comes from the `zstd` CLI (v1.5.7, level 19). The Collector file
exporter layouts follow opentelemetry-collector-contrib v0.161.0
`exporter/fileexporter/file_writer.go`.

| File | Form |
|---|---|
| `synthetic-fork.ndjson.zst` | whole-stream zstd around JSON lines (`compression: zstd` with native compression, `format: json`) |
| `synthetic-fork-first.binpb` | one OTLP/HTTP protobuf body (the first delivery) |
| `synthetic-fork-first.binpb.zst` | the same body, zstd |
| `synthetic-fork-frames.pb` | `format: proto`: 4-byte big-endian length, then each body |
| `synthetic-fork-frames-zstd.pb` | `format: proto`, `compression: zstd`: each body zstd, then framed |
| `synthetic-fork-json-frames-zstd.pb` | `format: json`, `compression: zstd`: each JSON line zstd, then framed |
| `synthetic-fork-frames.pb.zst` | whole-stream zstd around `format: proto` frames |
| `nested-4.json.zst`, `nested-5.json.zst` | `{"resourceSpans":[]}` zstd'd 4 and 5 times: at and one past the 4-layer bound (`MAX_LAYERS`) |
