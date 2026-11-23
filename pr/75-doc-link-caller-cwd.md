# fix(goldens): CALLER_CWD's doc no longer links a private item

Status: draft · Base: pr/73 · crates/toolpath-goldens/src/pipeline.rs (one doc line)

## Why
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --features path-cli/develop,toolpath/verify --no-deps` failed at
pipeline.rs:38: the public `CALLER_CWD` doc (rewritten in pr/73) linked the private fn `resume_dir` with an intra-doc link
(`rustdoc::private_intra_doc_links`).

## What
The link becomes plain code, `` `resume_dir` ``. No behaviour change.

## Checked
doc (the command above), fmt, clippy `--workspace --all-targets` with both features `-D warnings`, `cargo test -p toolpath-goldens`,
`qualifier review` (0 drifted), `just baseline`.
