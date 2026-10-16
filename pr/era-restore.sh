#!/usr/bin/env bash
# era-restore.sh — give every piece of the toolpath stack the content its files had at the end
# of that piece's era in the pre-squash wip history, so each piece is a tree that was built and
# tested at the time. Runs in the colocated jj clone (rewrite/jj-tp). Never pushes.
#
# Rule applied (pr/README.md, "Shared files are not file-level"):
#   * a file several pieces touched is assigned per HUNK: each hunk goes to the piece that
#     introduces what it references; generated files (goldens outputs, insta snapshots, the
#     manifest) follow the code that produced them;
#   * Cargo.toml names only the members present in the piece's tree; Cargo.lock is wip's lock
#     pruned to that manifest (`cargo metadata --offline` after restoring wip's lock: prune only,
#     no version drift);
#   * HOLD-era hunks (wip commits before the goldens work, main..465de3dd) stay in HOLD even when
#     they sit in a file a piece owns (crates/path-cli/src/cmd_export.rs);
#   * the history scrub is kept: crates/path-cli/src/goldens.rs keeps its scrubbed content and
#     every piece is asserted free of the scrubbed strings.
#
# Inputs: owners.tsv (piece \t status \t path, from the current cut), the era map below.
# Rollback: `jj op restore <id>` with the id printed first.
set -euo pipefail
S=${S:-/private/tmp/claude-501/-Users-bobby--local-share-git-repos-io-github-bdelanghe-scratch-main/7c8ae6d3-afd1-4605-913b-9fd31e45468f/scratchpad}
export PATH=$HOME/.rustup/toolchains/1.94.1-aarch64-apple-darwin/bin:$PATH
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$S/rewrite/tp/target}
cd "$S/rewrite/jj-tp"
WIP=${WIP:-squash}              # the wip tip whose tree HOLD must equal (bookmark or commit)
MAIN=main@github
HOLD_ERA=465de3ddcc08           # last wip commit before the goldens work; its hunks are HOLD's
order=(pr/01-convo-breakdowns pr/02-goldens-core pr/06-resume-fixes pr/07-insta-goldens pr/08-resume-opencode pr/09-antigravity-format pr/10-opencode-session-ids pr/11-harness-formats pr/12-goldens-all-harnesses pr/13-webmock pr/14-claims-audit pr/15-no-secrets)
declare -A era=(
  [pr/02-goldens-core]=2c36ac1489ef          # chore(pr): what the first jj split taught (before fix(pi))
  [pr/06-resume-fixes]=3596bb1d0858          # chore(pr): HOLD.md restored (before toolpath-goldens)
  [pr/07-insta-goldens]=6aeff368db6c         # chore(pr): 07
  [pr/08-resume-opencode]=0655ae99e948       # chore(pr): 08
  [pr/09-antigravity-format]=7f34092d3abf    # chore(pr): 09
  [pr/10-opencode-session-ids]=5117415347b6  # chore(pr): 10
  [pr/11-harness-formats]=deb5baa299a1       # chore(pr): 11
  [pr/12-goldens-all-harnesses]=6917370f87fd # chore(pr): 12
  [pr/13-webmock]=c6430a74d5cf               # chore(pr): 13 (before the secret-header fix aaaa928a)
  [pr/14-claims-audit]=78c66963d316          # chore(pr): 14 = pre-squash tip
  [pr/15-no-secrets]=$WIP                    # everything else that is not HOLD
)
# files a piece owns that also carry HOLD-era hunks and must NOT take them (3-way: main + era delta)
mixed=(crates/path-cli/src/cmd_export.rs)
# files whose only difference from their era is the scrub: never restored from history
never=(Cargo.toml Cargo.lock crates/path-cli/src/goldens.rs)

echo "rollback: jj op restore $(jj op log -n1 --no-graph -T 'id.short()')"
is_in() { local x=$1; shift; for y in "$@"; do [ "$x" = "$y" ] && return 0; done; return 1; }
owned_upto() { # paths owned by pieces <= $1 (pr/14 skips pr/13's: the webmock scrub fix is pr/15's)
  local P=$1 Q
  for Q in "${order[@]}"; do
    if [ "$P" = pr/14-claims-audit ] && [ "$Q" = pr/13-webmock ]; then continue; fi
    awk -F'\t' -v q="$Q" '$1==q{print $3}' "$S/rewrite/owners.tsv"
    [ "$Q" = "$P" ] && break
  done | sort -u
}
relock() { # wip's lock pruned to this tree's manifest; fails if the manifest needs anything new
  jj restore --from "$WIP" -- Cargo.lock >/dev/null 2>&1 || true
  cargo metadata --format-version 1 --offline >/dev/null
}
for P in "${order[@]}"; do
  E=${era[$P]:-}; [ -n "$E" ] || continue
  plain=(); three=()
  while IFS= read -r p; do
    is_in "$p" "${never[@]}" && continue
    if is_in "$p" "${mixed[@]}"; then three+=("$p"); else plain+=("\"$p\""); fi
  done < <(owned_upto "$P")
  jj new "$P" -m "era: $P <- $E" >/dev/null 2>&1
  jj restore --from "$E" -- "${plain[@]}" >/dev/null 2>&1
  for f in "${three[@]}"; do
    # main's file + the goldens-era delta (HOLD_ERA -> E) for that file; HOLD-era hunks stay out
    jj restore --from "$MAIN" -- "\"$f\"" >/dev/null 2>&1
    if ! jj diff --from "$HOLD_ERA" --to "$E" --git -- "\"$f\"" | git apply --whitespace=nowarn; then
      echo "$P: $f: the era delta does not apply onto main's file; resolve by hand (jj diff --from $HOLD_ERA --to $E -- $f), then rerun"; exit 1
    fi
  done
  relock
  if [ "$(jj diff --summary | wc -l | tr -d ' ')" = 0 ]; then
    jj abandon @ >/dev/null 2>&1; echo "$P: unchanged"
  else
    jj squash --use-destination-message >/dev/null 2>&1
    echo "$P: $(jj diff -r "$P" --summary | wc -l | tr -d ' ') files in piece"
  fi
  c=$(jj log -r "$P" --no-graph -T commit_id)
  # only the files this piece changes against main: main itself carries '/Users/bobby' in old design docs
  changed=$(jj diff -r "$P" --name-only | grep -v '^pr/' || true)
  leaks=""
  [ -n "$changed" ] && leaks=$(printf '%s\n' "$changed" | xargs git grep -l -e 'bobby@empathic.dev' -e '/Users/bobby' "$c" -- 2>/dev/null | sed "s/^$c://" || true)
  [ -z "$leaks" ] || { echo "$P: scrubbed strings reappeared in: $leaks"; exit 1; }
done
# The rustdoc fix made on wip (wip-stack-repair: #![allow(rustdoc::invalid_html_tags)] in
# goldens.rs) belongs to pr/02, the piece that introduces the file; every later piece inherits
# it because none of them touches goldens.rs. Run with WIP=wip-stack-repair@github (after
# `git -C $S/rewrite/tp fetch $S/tp wip-stack-repair:wip-stack-repair; jj git fetch --remote github`).
if [ -n "$(jj diff --from pr/02-goldens-core --to "$WIP" --summary -- crates/path-cli/src/goldens.rs)" ]; then
  jj new pr/02-goldens-core -m 'docfix' >/dev/null 2>&1
  jj restore --from "$WIP" -- crates/path-cli/src/goldens.rs >/dev/null 2>&1
  jj squash --use-destination-message >/dev/null 2>&1
  echo "pr/02-goldens-core: goldens.rs taken from $WIP (rustdoc fix)"
fi
# HOLD absorbs whatever wip has that no piece has (including the HOLD-era hunks of mixed files)
hold=$(jj log -r 'description(glob:"HOLD*") & mutable()' --no-graph -T change_id)
jj new "$hold" -m 'era: HOLD <- wip' >/dev/null 2>&1
jj restore --from "$WIP" >/dev/null 2>&1
if [ "$(jj diff --summary | wc -l | tr -d ' ')" = 0 ]; then jj abandon @ >/dev/null 2>&1; else jj squash --use-destination-message >/dev/null 2>&1; fi
echo "--- conflicts:"; jj log -r 'main@github.. & conflicts()' --no-graph -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"'
echo "--- HOLD vs $WIP (must be empty):"; jj diff --from "$hold" --to "$WIP" --summary; echo "(end)"
echo "--- HOLD now carries (compare with pr/HOLD.md Paths):"; jj diff -r "$hold" --summary | grep -v -e ' pr/' -e 'cassettes/' -e '\.qual$' || true
echo
echo "next: for each piece, $S/rewrite/gate-piece.sh $S/rewrite/jj-tp $S/rewrite/wt/gate <pr/NN> $S/rewrite/gate-logs"
