#!/usr/bin/env bash
# pr/open.sh — open the virtual PRs on wip as real draft PRs, from their records.
#
#   pr/open.sh [--repo owner/name] [NN ...]
#
# Reads pr/NN-<slug>.md (this directory): title = first line, head = pr/NN-<slug>, base = its
# `Base:` line (`main`, or another piece's bookmark); without one, the previous piece in NN order.
# Skips a record whose Status already says `opened #N`, refuses a head that is not on the remote,
# creates the PR as a draft with the record as body, then writes `opened #N` back into the record.
# Idempotent: rerun after a partial run. Bobby runs it (agents may not open PRs in empathic repos);
# the push of the pr/* branches is a separate, earlier step (cut.sh prints it).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
repo=""
if [ "${1:-}" = "--repo" ]; then repo=$2; shift 2; fi
if [ -z "$repo" ]; then
  repo=$(git -C "$root" remote get-url origin | sed -E 's#.*github.com[:/]##; s#\.git$##')
fi
records=()
if [ $# -gt 0 ]; then
  for n in "$@"; do records+=("$(ls "$here"/"$n"-*.md)"); done
else
  for f in "$here"/[0-9][0-9]-*.md; do records+=("$f"); done
fi
remote_heads=$(git -C "$root" ls-remote --heads origin 'pr/*' | awk '{print $2}' | sed 's#refs/heads/##')
prev=""
for f in "${records[@]}"; do
  stem=$(basename "$f" .md)
  head="pr/$stem"
  title=$(head -1 "$f" | sed 's/^# *//')
  status=$(grep -m1 -E '^Status:' "$f" || true)
  base=$(grep -m1 -E '^Base:' "$f" | sed -E 's/^Base:[[:space:]]*//; s/`//g' || true)
  [ -n "$base" ] || base=${prev:-main}
  if grep -qE '^Status:.*opened #[0-9]+' "$f"; then
    echo "$head: $(echo "$status" | grep -oE 'opened #[0-9]+'), skipped"
    prev=$head; continue
  fi
  if grep -qE '^Status:.*(folded|held|merged|not cut|superseded)' "$f"; then
    echo "$head: $status, not a PR"
    continue
  fi
  # `Base:` is read only as its own line (pr/00 style). The `Base:` inside older Status lines is
  # the first-cut plan and is stale; a stacked piece's real base is the piece before it in NN order.
  if ! grep -qx "$head" <<<"$remote_heads"; then
    echo "$head: not on origin — push the pr/* branches first (cut.sh prints the line)" >&2; exit 1
  fi
  url=$(gh pr create --repo "$repo" --draft --base "$base" --head "$head" --title "$title" --body-file "$f")
  num=${url##*/}
  if grep -qE '^Status:' "$f"; then
    perl -0pi -e "s/^Status:.*$/Status: opened #$num (draft) — \$&/m; s/— Status: /— was: /" "$f"
  else
    perl -0pi -e "s/^(# [^\n]*\n)/\$1\nStatus: opened #$num (draft)\n/" "$f"
  fi
  echo "$head -> $base: #$num"
  prev=$head
done
echo "records updated; commit pr/ on wip"
