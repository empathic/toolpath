#!/usr/bin/env bash
# Launch a local OpenClaw instance in Docker for capturing real agent
# sessions — used to validate the `toolpath-openclaw` provider against
# genuine on-disk transcripts instead of synthesized fixtures.
#
# Grounded in openclaw/openclaw @ 453f5968 (v2026.6.11):
#   - Prebuilt image openclaw/openclaw:latest (Docker Hub — public). The
#     ghcr.io/openclaw/openclaw mirror needs `docker login ghcr.io`; override
#     with OPENCLAW_IMAGE if you prefer it. ENTRYPOINT tini, default CMD
#     `node openclaw.mjs gateway` (== `node dist/index.js gateway`).
#   - The gateway binds LOOPBACK by default, so `-p` alone is unreachable
#     from the host; we override to `--bind lan` + a gateway token.
#   - State (config, credentials, SQLite, and agents/<id>/sessions/) lives at
#     /home/node/.openclaw. We volume-mount it so the host — and
#     `path p import openclaw --base <dir>` — can read the transcripts.
#
# Usage:
#   scripts/openclaw-docker.sh up                 # pull + start gateway (onboards if ANTHROPIC_API_KEY is set)
#   scripts/openclaw-docker.sh status             # container + /healthz + paths
#   scripts/openclaw-docker.sh agent "<message>"  # run one agent turn (writes a session transcript)
#   scripts/openclaw-docker.sh sessions           # list sessions + host session dir
#   scripts/openclaw-docker.sh import [<id>]      # validate via toolpath-openclaw (list, or import+render one)
#   scripts/openclaw-docker.sh logs               # follow gateway logs
#   scripts/openclaw-docker.sh down               # stop + remove the container (keeps state dir)
#   scripts/openclaw-docker.sh nuke               # down + delete the host state dir
#
# Env (override as needed; UPPER = intended for the caller):
#   ANTHROPIC_API_KEY        Anthropic key; required for onboarding + `agent`.
#   OPENCLAW_IMAGE           default openclaw/openclaw:latest (Docker Hub)
#   OPENCLAW_HOST_STATE_DIR  default ~/.openclaw-docker  (host mount for /home/node/.openclaw)
#   OPENCLAW_HOST_PORT       default 18789
#   OPENCLAW_MODEL           default anthropic/claude-opus-4-8
#   OPENCLAW_AGENT_ID        default main
#   OPENCLAW_CONTAINER       default openclaw-toolpath

set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────

_root="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE="${OPENCLAW_IMAGE:-openclaw/openclaw:latest}"
STATE_DIR="${OPENCLAW_HOST_STATE_DIR:-${HOME}/.openclaw-docker}"
HOST_PORT="${OPENCLAW_HOST_PORT:-18789}"
MODEL="${OPENCLAW_MODEL:-anthropic/claude-opus-4-8}"
AGENT_ID="${OPENCLAW_AGENT_ID:-main}"
CONTAINER="${OPENCLAW_CONTAINER:-openclaw-toolpath}"

_gw_port=18789                       # in-container gateway port (fixed by the image)
_state_mount=/home/node/.openclaw    # in-container state dir (fixed by the image)
_token_file="${STATE_DIR}/.gateway-token"

# ── Helpers ───────────────────────────────────────────────────────────────

_log()  { printf '\033[1;36m[openclaw]\033[0m %s\n' "$*" >&2; }
_warn() { printf '\033[1;33m[openclaw] WARN:\033[0m %s\n' "$*" >&2; }
_die()  { printf '\033[1;31m[openclaw] ERROR:\033[0m %s\n' "$*" >&2; exit 1; }

_need_docker() {
    command -v docker >/dev/null 2>&1 || _die "docker not found on PATH"
    docker info >/dev/null 2>&1 || _die "docker daemon not reachable (is Docker running?)"
}

_container_exists()  { docker inspect "${CONTAINER}" >/dev/null 2>&1; }
_container_running() {
    [[ "$(docker inspect -f '{{.State.Running}}' "${CONTAINER}" 2>/dev/null)" == "true" ]]
}

_healthy() { curl -fsS "http://127.0.0.1:${HOST_PORT}/healthz" >/dev/null 2>&1; }

_load_or_make_token() {
    if [[ -s "${_token_file}" ]]; then
        cat "${_token_file}"
        return 0
    fi
    local _t
    _t="$(openssl rand -hex 32)"
    printf '%s' "${_t}" >"${_token_file}"
    chmod 600 "${_token_file}"
    printf '%s' "${_t}"
}

_write_min_config() {
    # Minimal model config. Grounded in docs/providers/anthropic.md, which
    # pairs the env API key with agents.defaults.model.primary.
    local _cfg="${STATE_DIR}/openclaw.json"
    [[ -f "${_cfg}" ]] && return 0
    cat >"${_cfg}" <<JSON
{
  "agents": { "defaults": { "model": { "primary": "${MODEL}" } } }
}
JSON
    _log "wrote ${_cfg}"
}

_onboard() {
    # Blessed non-interactive onboarding (docs/start/wizard-cli-automation.md)
    # writes the auth profile so `openclaw agent` can authenticate. Skip if a
    # profile already exists, or if no key is available.
    local _auth="${STATE_DIR}/agents/${AGENT_ID}/agent/auth-profiles.json"
    if [[ -f "${_auth}" ]]; then
        _log "auth profile present; skipping onboarding"
        return 0
    fi
    if [[ -z "${ANTHROPIC_API_KEY:-}" ]]; then
        _warn "ANTHROPIC_API_KEY unset — skipping onboarding (agent turns will fail until set)"
        return 0
    fi
    _log "running onboarding (writes the auth profile)…"
    # --skip-health: onboarding runs before our gateway container starts, so
    # its default gateway-reachability probe would otherwise fail; we only need
    # it to write the config + auth profile.
    docker run --rm \
        -e "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}" \
        -v "${STATE_DIR}:${_state_mount}" \
        "${IMAGE}" node dist/index.js onboard \
        --non-interactive --accept-risk --mode local \
        --auth-choice apiKey --anthropic-api-key "${ANTHROPIC_API_KEY}" \
        --gateway-bind lan --skip-health \
        || _warn "onboarding exited nonzero — the written openclaw.json may still suffice; check '$0 logs'"
}

_wait_healthy() {
    _log "waiting for gateway /healthz on :${HOST_PORT}…"
    local _i
    for _i in $(seq 1 60); do
        if _healthy; then
            _log "gateway healthy"
            return 0
        fi
        if ! _container_running; then
            docker logs --tail 40 "${CONTAINER}" >&2 || true
            _die "gateway container exited during startup (logs above)"
        fi
        sleep 2
    done
    docker logs --tail 40 "${CONTAINER}" >&2 || true
    _die "gateway did not become healthy within ~120s (logs above)"
}

# ── Subcommands ───────────────────────────────────────────────────────────

cmd_up() {
    _need_docker
    mkdir -p "${STATE_DIR}"
    # On Linux the container runs as uid 1000 (node) and needs to write the
    # bind mount. Docker Desktop (macOS/Windows) maps this automatically.
    if [[ "$(uname -s)" == "Linux" ]]; then
        chown -R 1000:1000 "${STATE_DIR}" 2>/dev/null \
            || _warn "could not chown ${STATE_DIR} to uid 1000 (may need sudo)"
    fi

    if ! docker image inspect "${IMAGE}" >/dev/null 2>&1; then
        _log "pulling ${IMAGE} (first run; may take a while)…"
        docker pull "${IMAGE}"
    fi

    if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
        _write_min_config
        _onboard
    else
        _warn "ANTHROPIC_API_KEY unset — the gateway will start unconfigured;"
        _warn "set it and re-run '$0 up' before '$0 agent'."
    fi

    if _container_running; then
        _log "already running (${CONTAINER})"
        cmd_status
        return 0
    fi
    _container_exists && docker rm -f "${CONTAINER}" >/dev/null

    local _token
    _token="$(_load_or_make_token)"

    local -a _args=(
        run -d --name "${CONTAINER}"
        -p "${HOST_PORT}:${_gw_port}"
        -e "OPENCLAW_GATEWAY_TOKEN=${_token}"
        -v "${STATE_DIR}:${_state_mount}"
        --add-host host.docker.internal:host-gateway
        --restart unless-stopped
    )
    [[ -n "${ANTHROPIC_API_KEY:-}" ]] && _args+=(-e "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}")

    _log "starting gateway (${CONTAINER})…"
    # --bind lan makes the published port reachable from the host;
    # --allow-unconfigured lets the gateway start before/without full config.
    docker "${_args[@]}" "${IMAGE}" \
        node dist/index.js gateway --bind lan --port "${_gw_port}" --allow-unconfigured \
        >/dev/null

    _wait_healthy
    cmd_status
    _log "Control UI: http://127.0.0.1:${HOST_PORT}/  (gateway token: ${_token_file})"
    _log "Send a turn: $0 agent \"hello from docker\""
}

cmd_status() {
    _need_docker
    if _container_running; then
        _log "container ${CONTAINER}: running"
    else
        _log "container ${CONTAINER}: not running"
    fi
    if _healthy; then
        _log "healthz:  OK (http://127.0.0.1:${HOST_PORT})"
    else
        _warn "healthz:  not responding on :${HOST_PORT}"
    fi
    _log "state dir (host): ${STATE_DIR}"
    _log "sessions dir:     ${STATE_DIR}/agents/${AGENT_ID}/sessions"
}

cmd_agent() {
    _need_docker
    [[ $# -ge 1 ]] || _die "usage: $0 agent \"<message>\""
    _container_running || _die "gateway not running; run '$0 up' with ANTHROPIC_API_KEY set"
    local _msg="$*"
    _log "running one agent turn (agent=${AGENT_ID}, model=${MODEL})…"
    # --local forces embedded execution: the turn runs in-process and still
    # writes the transcript to the sessions dir, avoiding the gateway auth-token
    # handshake (which the exec'd client and the gateway don't share). The
    # container inherits ANTHROPIC_API_KEY from `up`, so auth uses that. If this
    # errors on auth, re-run '$0 up' with ANTHROPIC_API_KEY set.
    docker exec "${CONTAINER}" node dist/index.js agent \
        --agent "${AGENT_ID}" --model "${MODEL}" --local --message "${_msg}"
    _log "transcript written under ${STATE_DIR}/agents/${AGENT_ID}/sessions"
}

cmd_sessions() {
    _need_docker
    if _container_running; then
        docker exec "${CONTAINER}" node dist/index.js sessions --agent "${AGENT_ID}" --json || true
    fi
    _log "host session files (${STATE_DIR}/agents/${AGENT_ID}/sessions):"
    ls -1 "${STATE_DIR}/agents/${AGENT_ID}/sessions" 2>/dev/null || _warn "  (none yet)"
}

cmd_import() {
    local _bin="${_root}/target/debug/path"
    if [[ ! -x "${_bin}" ]]; then
        _log "building the path binary…"
        (cd "${_root}" && cargo build -q -p path-cli)
    fi
    _log "sessions visible to toolpath-openclaw (--base ${STATE_DIR}):"
    "${_bin}" p list openclaw --base "${STATE_DIR}" --format tsv || _warn "  (list failed / no sessions)"
    if [[ $# -ge 1 ]]; then
        _log "importing + rendering session $1…"
        "${_bin}" p import openclaw --base "${STATE_DIR}" --agent "${AGENT_ID}" --session "$1" --no-cache \
            | "${_bin}" p render md --input -
    else
        _log "pass a <session-id> from the list above to import + render it."
    fi
}

cmd_logs() {
    _need_docker
    docker logs -f "${CONTAINER}"
}

cmd_down() {
    _need_docker
    if _container_exists; then
        docker rm -f "${CONTAINER}" >/dev/null
        _log "removed container ${CONTAINER} (state dir ${STATE_DIR} kept)"
    else
        _log "container ${CONTAINER} not present"
    fi
}

cmd_nuke() {
    cmd_down
    if [[ -n "${STATE_DIR}" && "${STATE_DIR}" != "/" && -d "${STATE_DIR}" ]]; then
        rm -rf "${STATE_DIR}"
        _log "deleted state dir ${STATE_DIR}"
    fi
}

# ── Dispatch ──────────────────────────────────────────────────────────────

_cmd="${1:-}"
shift || true
case "${_cmd}" in
    up)       cmd_up "$@" ;;
    status)   cmd_status "$@" ;;
    agent)    cmd_agent "$@" ;;
    sessions) cmd_sessions "$@" ;;
    import)   cmd_import "$@" ;;
    logs)     cmd_logs "$@" ;;
    down)     cmd_down "$@" ;;
    nuke)     cmd_nuke "$@" ;;
    ""|-h|--help|help)
        sed -n '2,40p' "$0"
        ;;
    *)
        echo "unknown command: ${_cmd}" >&2
        echo "usage: $0 {up|status|agent <msg>|sessions|import [<id>]|logs|down|nuke}" >&2
        exit 64
        ;;
esac
