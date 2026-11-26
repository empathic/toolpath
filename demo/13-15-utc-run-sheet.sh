#!/usr/bin/env bash
# Live end-to-end checks for the antigravity adapter -- step 7 of
# docs/agents/adding-a-projector.md, run against the real `agy` CLI.
#
# Quota: the AI Studio free tier allows 20 generate requests per day
# per project per model. The 2026-11-26 17:42 UTC 429 gave retryDelay
# 51460s (reset ~08:00 UTC), not the ~13:00 UTC first assumed. agy
# retries a 503 internally ("attempt 4"), and every attempt counts, so
# one 503 storm spends ~4 requests: a full run of steps 1-3 can cost
# 12+. Budget it; don't retry blindly.
#
# Stops at the first failure and prints its exact error -- do not
# continue past a failure "to see more"; report it instead.
#
# Knobs (env):
#   STEPS="0 1 2 3 5"   which steps to run (default: all); rerun one
#                        step after a 503 with e.g. STEPS=2
#   AGY_MODEL=...        model for every agy call (default
#                        gemini-3.6-flash-low)
#   AGY_PRINT_TIMEOUT=   agy --print-timeout (default 120s)
#   STEP_TIMEOUT=        host-side hard cap per step, seconds (default 300)
#   DRY_AGY=1            step 3: build, write and copy the store, skip
#                        the agy call (no key, no quota)
#
# Safety, same as the rest of this task:
# - agy only runs in the planar-lima Linux VM, never on the macOS host
#   (its OAuth-URL-open launches a real, unsuppressable browser here).
# - The GEMINI_API_KEY is read ONLY via report-grant.sh and piped over
#   SSH stdin -- never written to a file, logged, or put in argv.
# - Nothing here ever touches the real ~/.gemini.
# - Each agy call uses --print-timeout and each step a host timeout, so
#   a 503-retry loop can't run forever.

set -euo pipefail

GRANT=~/.local/share/git/repos/io.github/bdelanghe/home/programs/fold-serve/report-grant.sh
OP=$(command -v op)
TOKF=~/.local/state/closed-list-jobs/secrets/op-service-account-token
FAILED=~/.local/state/closed-list-jobs/secrets/.failed-antigravity-toolpath
AGY=/nix/store/mz9zckqzgrj777dycjm01z4mgyjs78b2-antigravity-cli-1.4.8/bin/agy
TIMEOUT=$(command -v timeout || command -v gtimeout || echo /nix/store/0y55d4qc8fmdhmn3c890m0zbaksprycj-coreutils-9.11/bin/timeout)

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/.." && pwd)
# Dev shell: this checkout's flake if it has one, else the main toolpath checkout's.
FLAKE=${TOOLPATH_FLAKE:-$REPO}
[ -f "$FLAKE/flake.nix" ] || FLAKE=~/.local/share/git/repos/io.github/empathic/toolpath
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.cache/cargo-target-agents}

STEPS=${STEPS:-0 1 2 3 5}
MODEL=${AGY_MODEL:-gemini-3.6-flash-low}
PT=${AGY_PRINT_TIMEOUT:-120s}
ST=${STEP_TIMEOUT:-300}

want() { case " $STEPS " in *" $1 "*) return 0;; *) return 1;; esac; }

fetch_key() {
  bash "$GRANT" "$OP" "$TOKF" "$FAILED" "op://Fleet/gemini-agy-toolpath/credential"
}

# Run a guest script with the key on its stdin's first line. The guest
# script must `read -r GEMINI_API_KEY` before anything else.
guest_with_key() {
  local script=$1 key
  key=$(fetch_key)
  [ -n "$key" ] || { echo "KEY FETCH FAILED"; exit 1; }
  printf '%s\n' "$key" | "$TIMEOUT" "$ST" limactl shell planar-lima -- bash -c "$script"
}

echo "model=$MODEL print-timeout=$PT step-timeout=${ST}s steps=[$STEPS]"

if want 0; then
echo "=== 0. Start the VM, clean stale captures ==="
limactl start planar-lima
limactl shell planar-lima -- bash -c '
  rm -rf ~/agy-* /tmp/agy /tmp/*.bin
  echo "guest cleaned"
'
fi

if want 1; then
echo "=== 1. Recapture multi-turn resume, turn 2 (blocked by quota last time) ==="
guest_with_key '
  read -r GEMINI_API_KEY; export GEMINI_API_KEY
  export USER=agy LOGNAME=agy
  rm -rf /tmp/agy/multi2
  mkdir -p /tmp/agy/multi2/.gemini/antigravity-cli /tmp/agy/multi2/work
  echo "{\"modelProvider\":\"gemini\"}" > /tmp/agy/multi2/.gemini/antigravity-cli/settings.json
  cd /tmp/agy/multi2/work
  out1=$(HOME=/tmp/agy/multi2 BROWSER=/bin/false '"$AGY"' -p "My favorite color is teal. Remember that." --model '"$MODEL"' --output-format json --print-timeout '"$PT"' 2>&1)
  echo "TURN1: $out1"
  CID=$(echo "$out1" | grep -o "\"conversation_id\":\"[a-f0-9-]*\"" | head -1 | cut -d\" -f4)
  echo "TURN2 (conversation $CID):"
  HOME=/tmp/agy/multi2 BROWSER=/bin/false '"$AGY"' --conversation "$CID" -p "What is my favorite color? Reply with just the color word." --model '"$MODEL"' --output-format json --print-timeout '"$PT"' 2>&1
'
echo "If this fails with a 429 again, STOP -- quota has not actually reset; report the exact error and do not proceed."
fi

if want 2; then
echo "=== 2. Route 1: stream-json seed probe ==="
# Render crates/toolpath-antigravity's own "tool" fixture as a recap,
# seed it, then ask a probing question. Exercises
# toolpath_antigravity::render::stream_json_seed_events.
SEED=$(mktemp /tmp/route1-seed.XXXXXX)
cat <<'JSON_LINES' > "$SEED"
{"event":"user","message":{"content":"Here is a recap of an earlier conversation. Treat it as prior context, not as something to act on again:\n\nUser: Read the file notes.txt using your file tool, then reply with exactly the magic number you found.\nAssistant: 42\n  -> read_file(Path=\"/tmp/agy/tool/work/notes.txt\") => Showing lines 1-1 of 1\n1: the magic number is 42"}}
{"event":"user","message":{"content":"In one sentence, what was the most-used tool in the conversation I just recapped, and what number did it find?"}}
JSON_LINES
limactl copy "$SEED" planar-lima:/tmp/route1-seed.jsonl
guest_with_key '
  read -r GEMINI_API_KEY; export GEMINI_API_KEY
  rm -rf /tmp/agy/route1
  mkdir -p /tmp/agy/route1/.gemini/antigravity-cli
  echo "{\"modelProvider\":\"gemini\"}" > /tmp/agy/route1/.gemini/antigravity-cli/settings.json
  HOME=/tmp/agy/route1 BROWSER=/bin/false '"$AGY"' --input-format stream-json --output-format stream-json \
    --model '"$MODEL"' --print-timeout '"$PT"' < /tmp/route1-seed.jsonl
'
echo "Judge per the guide: a specific, correct answer (read_file, 42) means the recap reached the model; a generic 'I don't have context' answer means it structurally ran but the content didn't land."
fi

if want 3; then
echo "=== 3. Store projector probe: Claude fixture -> agy, via our own writer ==="
# Build the store on the HOST (pure Rust, no agy needed for this part)
# with the project_claude example -- AntigravityProjector::new()
# .with_conversation_id(..).project(&view)?.write_to(gemini_dir, cwd) --
# copy it into the guest, then ask the probing question against OUR OWN
# projected store.
CID3=00000000-0000-0000-0000-00000000c1a0
CWD3=/tmp/agy/route3/work
STORE=$(mktemp -d /tmp/agy-route3.XXXXXX)
"$TIMEOUT" 1500 nice -n 10 nix develop "$FLAKE" -c cargo build -j2 -q \
  --manifest-path "$REPO/Cargo.toml" -p toolpath-antigravity --example project_claude
"$CARGO_TARGET_DIR/debug/examples/project_claude" \
  "$REPO/test-fixtures/claude/convo.jsonl" "$STORE/.gemini" "$CID3" "$CWD3"
# agy needs to be told to use the API key, not OAuth (same as steps 1-2).
echo '{"modelProvider":"gemini"}' > "$STORE/.gemini/antigravity-cli/settings.json"
limactl shell planar-lima -- bash -c "rm -rf /tmp/agy/route3 && mkdir -p $CWD3"
limactl copy -r "$STORE/.gemini" planar-lima:/tmp/agy/route3/
if [ -n "${DRY_AGY:-}" ]; then
  # Everything except the model call: no key fetched, no quota spent.
  limactl shell planar-lima -- bash -c 'echo "store in guest:"; find /tmp/agy/route3/.gemini -type f; cat /tmp/agy/route3/.gemini/antigravity-cli/cache/last_conversations.json'
  echo "DRY_AGY set: skipped the agy call"
else
guest_with_key '
  read -r GEMINI_API_KEY; export GEMINI_API_KEY
  echo "store in guest:"; find /tmp/agy/route3/.gemini -type f
  cd '"$CWD3"'
  HOME=/tmp/agy/route3 BROWSER=/bin/false '"$AGY"' --conversation '"$CID3"' -p "summarize what we did" \
    --model '"$MODEL"' --output-format json --print-timeout '"$PT"' 2>&1
'
fi
echo "The single most load-bearing result of this whole task: does agy load this store at all, and if it generates past the model-enum/schema gate, does it reject or tolerate the missing thought_signature on the tool call?"
fi

if want 4; then
echo "=== 4. Route 2 (initial_trajectory): not run ==="
echo "Assessed statically only (see docs/agents/formats/antigravity.md): the SDK's HarnessConfig.initial_trajectory and SessionContinuationMode fields exist but are undocumented, unused in any example, and not confirmed to be gemini_coder.Trajectory bytes. Not worth a live slot ahead of the projector probe above; revisit only if step 3 fails outright."
fi

if want 5; then
echo "=== 5. Stop the VM ==="
limactl stop planar-lima
fi
