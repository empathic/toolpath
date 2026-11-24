#!/usr/bin/env bash
# Regenerate the examples/ documents a producer can make (see
# crates/path-cli/tests/examples_generated.rs and examples/README.md), then
# review the diff. Without UPDATE_EXAMPLES the same test fails on any drift.
set -euo pipefail
cd "$(dirname "$0")/.."
UPDATE_EXAMPLES=1 cargo test -q -p path-cli --test examples_generated
git --no-pager diff --stat -- examples/
