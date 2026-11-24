# RFC 8785 test vectors

`input/*.json` and `output/*.json` are copied unchanged from the reference
implementation's test data:

- repository: https://github.com/cyberphone/json-canonicalization (Anders Rundgren, the RFC 8785 author)
- path: `testdata/input/`, `testdata/output/`
- commit: `32a48195b9b20fe67b1795e39e4509f14d860b45` (master, fetched 2026-10-19)
- licence: Apache-2.0, see `LICENSE` beside this file

Each `output/<name>.json` is the exact canonical byte sequence for
`input/<name>.json` (no trailing newline). `tests/jcs.rs` requires the
canonicalizer to reproduce every one byte for byte.
