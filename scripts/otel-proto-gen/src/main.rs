//! Generate prost message types for the two OTLP export requests.
//!
//! Usage: otel-proto-gen <opentelemetry-proto checkout> <out dir>
//! Services are not generated (no service generator is configured), and doc
//! comments are disabled: upstream comments contain indented blocks that
//! rustdoc runs as doctests.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, proto_root, out_dir] = args.as_slice() else {
        eprintln!("usage: otel-proto-gen <opentelemetry-proto checkout> <out dir>");
        std::process::exit(2);
    };
    let protos: Vec<String> = [
        "opentelemetry/proto/collector/trace/v1/trace_service.proto",
        "opentelemetry/proto/collector/logs/v1/logs_service.proto",
    ]
    .iter()
    .map(|p| format!("{proto_root}/{p}"))
    .collect();
    prost_build::Config::new()
        .out_dir(out_dir)
        .disable_comments(["."])
        .compile_protos(&protos, &[proto_root.as_str()])
        .expect("prost-build failed");
}
