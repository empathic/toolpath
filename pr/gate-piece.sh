#!/usr/bin/env bash
# gate-piece.sh <jj-clone> <worktree> <bookmark> <logdir>
# Checks one piece on its own tree: cargo metadata, fmt, clippy -D warnings (all targets),
# RUSTFLAGS=-D warnings cargo test --workspace, RUSTDOCFLAGS=-D warnings cargo doc.
set -uo pipefail
clone=$1 wt=$2 bm=$3 logdir=$4
export PATH=$HOME/.rustup/toolchains/1.94.1-aarch64-apple-darwin/bin:$PATH
c=$(jj -R "$clone" log -r "$bm" --no-graph -T commit_id)
[ -d "$wt" ] || git -C "$clone" worktree add --detach -q "$wt" "$c"
cd "$wt" && git checkout -q --detach "$c" || { echo "$bm: checkout failed"; exit 1; }
n=${bm#pr/}
res=""
t0=$SECONDS
if cargo metadata --format-version 1 --offline >/dev/null 2>"$logdir/$n.meta.log"; then res+=" metadata=ok"; else res+=" metadata=FAIL"; fi
if cargo fmt --all --check >"$logdir/$n.fmt.log" 2>&1; then res+=" fmt=ok"; else res+=" fmt=FAIL"; fi
if cargo clippy --workspace --all-targets --offline -- -D warnings >"$logdir/$n.clippy.log" 2>&1; then res+=" clippy=ok"; else res+=" clippy=FAIL"; fi
if RUSTFLAGS="-D warnings" cargo test --workspace --offline >"$logdir/$n.test.log" 2>&1; then res+=" test=ok($(grep -c '^test result' "$logdir/$n.test.log")suites)"; else res+=" test=FAIL"; fi
if RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --offline >"$logdir/$n.doc.log" 2>&1; then res+=" doc=ok"; else res+=" doc=FAIL"; fi
echo "$bm ${c:0:8}$res ($((SECONDS-t0))s)"
