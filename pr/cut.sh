#!/usr/bin/env bash
# Cut the virtual PRs out of the squashed wip commit with `jj split` by fileset.
#   pr/cut.sh [wip-rev] [base]      wip-rev defaults to wip/bdelanghe@$JJ_REMOTE, base to main@$JJ_REMOTE
# Runs in the colocated jj clone named by $JJ_CLONE (remote $JJ_REMOTE, default `github`), never
# in this checkout. One new commit on the base with wip's whole tree (`jj new` + `jj restore
# --from wip`, described `HOLD: ...`; wip may be several commits: the squash plus what was signed
# on top), then for each pr/NN-*.md with a Paths block, in order, `jj split -r <remainder> <fileset…>`:
# the fileset becomes the PR's commit (described with the md's title, bookmarked pr/NN-<slug>),
# the rest stays on top and keeps the HOLD description. The final remainder must match
# pr/HOLD.md's Paths. Idempotent: a PR whose pr/NN-<slug> bookmark exists in the clone is
# skipped; the remainder is found again by its HOLD description. The clone's
# git.private-commits keeps the remainder unpushable. Writes pr/stack.json next to this script
# (stack shape + the jj operation id to `jj op restore`). Never pushes.
set -euo pipefail
dir=${PR_DIR:-$(cd "$(dirname "$0")" && pwd)}
remote=${JJ_REMOTE:-github}
wip=${1:-wip/bdelanghe@$remote}; base=${2:-main@$remote}
: "${JJ_CLONE:?set JJ_CLONE to the colocated jj clone (with a $remote remote)}"
cd "$JJ_CLONE"
jj config set --repo git.private-commits 'description(glob:"wip:*") | description(glob:"HOLD*")'
jj git fetch --remote "$remote" >/dev/null

paths_of() { awk '/^Paths \(/{f=1;next} f&&/^```$/{n++; if(n==2)exit; next} f&&n==1{print}' "$1"; }
title_of() { sed -n '1{s/^# //;p;}' "$1"; }
remainder() { jj log -r "descendants($base) & mutable() & description(glob:\"HOLD*\")" --no-graph -T 'change_id ++ "\n"'; }

tip=$(remainder | head -1)
if [ -z "$tip" ]; then
  # One commit on the base whose tree is wip's whole tree (not `jj duplicate`: that copies one
  # commit's own diff, and wip is the squash plus whatever was signed on top of it since).
  jj new --no-edit -m "HOLD: not in any PR (remainder of wip $(jj log -r "$wip" --no-graph -T 'commit_id.short()'))" "$base" >/dev/null 2>&1
  tip=$(remainder | head -1)
  [ -n "$tip" ] || { echo "jj new on $base failed" >&2; exit 1; }
  jj restore --from "$wip" --into "$tip" >/dev/null 2>&1 || { echo "jj restore --from $wip failed" >&2; exit 1; }
  git_tree() { git rev-parse "$(jj log -r "$1" --no-graph -T 'commit_id')^{tree}"; }
  [ "$(git_tree "$tip")" = "$(git_tree "$wip")" ] || { echo "remainder tree differs from $wip's tree" >&2; exit 1; }
fi
[ "$(remainder | wc -l | tr -d ' ')" = 1 ] || { echo "more than one HOLD remainder in the clone; jj abandon the stale one" >&2; exit 1; }

for f in "$dir"/[0-9][0-9]-*.md; do
  name=$(basename "$f" .md)
  fs=(); while IFS= read -r p; do [ -n "$p" ] && fs+=("$p"); done < <(paths_of "$f")
  [ "${#fs[@]}" -gt 0 ] || continue
  if [ -n "$(jj bookmark list "pr/$name" -T 'name' 2>/dev/null)" ]; then
    echo "pr/$name: bookmark exists, skipped"; continue
  fi
  jj split -r "$tip" -m "$(title_of "$f")" -- "${fs[@]}" >/dev/null 2>&1 \
    || { echo "pr/$name: split failed; fileset:"; printf '  %s\n' "${fs[@]}"; exit 1; }
  tip=$(remainder | head -1)
  piece=$(jj log -r "parents($tip)" --no-graph -T 'change_id ++ "\n"' | head -1)
  jj bookmark create "pr/$name" -r "$piece" >/dev/null
  echo "pr/$name -> $(jj log -r "$piece" --no-graph -T 'commit_id.short()') ($(jj diff -r "$piece" --summary | wc -l | tr -d ' ') files)"
done

# the remainder must be exactly HOLD.md's Paths (every remainder file matched by one pattern)
bad=0
while read -r _ p; do
  hit=0
  while IFS= read -r pat; do
    [ -n "$pat" ] || continue
    case "$pat" in
      glob:*) g=${pat#glob:}; g=${g%\"}; g=${g#\"}; g=${g%/\*\*}; case "$p" in "$g"/*) hit=1;; esac;;
      *) [ "$p" = "$pat" ] && hit=1;;
    esac
  done < <(paths_of "$dir/HOLD.md")
  [ "$hit" = 1 ] || { echo "remainder file not in HOLD.md Paths: $p"; bad=1; }
done < <(jj diff -r "$tip" --summary)
[ "$bad" = 0 ] || { echo "HOLD mismatch: fix pr/*.md Paths or pr/HOLD.md" >&2; exit 1; }

# stack.json: the stack's shape for a later check (linear, every commit described, filesets disjoint)
op=$(jj op log -n1 --no-graph -T 'id')
entries=$(jj log -r "$base..$tip" --reversed --no-graph -T '
  "{\"change_id\":" ++ json(change_id) ++ ",\"commit_id\":" ++ json(commit_id) ++
  ",\"tree_id\":\"@TREE:" ++ commit_id ++ "@\"" ++
  ",\"description\":" ++ json(description.first_line()) ++
  ",\"parents\":[" ++ parents.map(|p| json(p.commit_id())).join(",") ++ "]" ++
  ",\"bookmarks\":[" ++ bookmarks.map(|b| json(b.name())).join(",") ++ "]" ++
  ",\"files\":[" ++ self.diff().files().map(|e| "{\"status\":" ++ json(e.status()) ++ ",\"path\":" ++ json(e.path().display()) ++ "}").join(",") ++ "]}\n"' | paste -sd, -)
while read -r c; do entries=${entries//"@TREE:$c@"/$(git rev-parse "$c^{tree}")}; done < <(jj log -r "$base..$tip" --reversed --no-graph -T 'commit_id ++ "\n"')
printf '{"base":%s,"wip":%s,"op":"%s","stack":[%s]}\n' \
  "$(jj log -r "$base" --no-graph -T 'json(commit_id)')" "$(jj log -r "$wip" --no-graph -T 'json(commit_id)')" "$op" "$entries" > "$dir/stack.json"
echo "stack.json written; this exact stack is operation $op (jj op restore $op)"
echo
jj log -r "$base..$tip"
echo

# Every piece must build on its own tree (pr/README.md, "Shared files are not file-level"):
# cargo metadata, fmt --check, clippy -D warnings, RUSTFLAGS=-D warnings test --workspace, doc,
# the way scripts/quality_gates.sh runs them in CI, each in a temp worktree of the colocated
# clone at that piece's commit. A shared CARGO_TARGET_DIR keeps the twelve runs incremental.
# PR_GATES=0 skips it (never before a push). No push line is printed while a piece fails.
if [ "${PR_GATES:-1}" != 0 ]; then
  export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$JJ_CLONE/target}
  gate_wt=$(mktemp -d "${TMPDIR:-/tmp}/pr-gate.XXXXXX")
  git worktree add --detach -q "$gate_wt" "$(jj log -r "$base" --no-graph -T commit_id)"
  trap 'git -C "$JJ_CLONE" worktree remove --force "$gate_wt" >/dev/null 2>&1 || true' EXIT
  failed=0
  while read -r c bm; do
    [ -n "$bm" ] || continue
    git -C "$gate_wt" checkout -q --detach "$c"
    res=""
    for g in "metadata:cargo metadata --format-version 1 --offline" \
             "fmt:cargo fmt --all --check" \
             "clippy:cargo clippy --workspace --all-targets --offline -- -D warnings" \
             "test:env RUSTFLAGS=-D\ warnings cargo test --workspace --offline" \
             "doc:env RUSTDOCFLAGS=-D\ warnings cargo doc --workspace --no-deps --offline"; do
      name=${g%%:*}; cmd=${g#*:}
      if (cd "$gate_wt" && eval "$cmd" >"$gate_wt.$name.log" 2>&1); then res+=" $name=ok"; else res+=" $name=FAIL($gate_wt.$name.log)"; failed=1; fi
    done
    echo "$bm ${c:0:8}$res"
  done < <(jj log -r "$base..$tip & bookmarks(glob:\"pr/*\")" --reversed --no-graph -T 'commit_id ++ " " ++ bookmarks ++ "\n"')
  if [ "$failed" != 0 ]; then
    echo "a piece does not build on its own tree; move the hunks it is missing or carrying early (pr/README.md), then rerun" >&2
    exit 1
  fi
fi

echo "push line (Bobby runs it, never this script; pushes the pr/* bookmarks in the range, the HOLD remainder has none and is private):"
echo "  jj -R $JJ_CLONE git push --remote $remote -r '$base..$tip'"
