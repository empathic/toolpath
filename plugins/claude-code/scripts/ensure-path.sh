#!/usr/bin/env bash
# Locate the Toolpath `path` binary, installing it globally if needed.
#
# Usage:
#   ensure-path.sh                 print the absolute binary path on stdout
#   ensure-path.sh exec <args...>  resolve, then run `path <args...>`
#   ensure-path.sh sessions        list Claude Code sessions for the cwd (TSV)
#   ensure-path.sh current-session print $CLAUDE_CODE_SESSION_ID, or "unknown"
#
# `sessions` and `current-session` exist so slash-command context blocks can
# reach `$PWD` / `$CLAUDE_CODE_SESSION_ID` without variables on the command
# line — Claude Code's permission checker rejects inline commands it cannot
# statically analyze.
#
# Everything except the resolved path / exec'd command output goes to stderr.
#
# Resolution order:
#   1. `path` on PATH that identifies as the Toolpath CLI
#   2. $TOOLPATH_INSTALL_DIR/path (default ~/.local/bin/path)
#   3. $TOOLPATH_CONFIG_DIR/bin/path (default ~/.toolpath/bin/path)
#   4. Download the latest GitHub release, verify its checksum, and install
#      to ~/.local/bin — or to ~/.toolpath/bin when an unrelated binary
#      named `path` would shadow the ~/.local/bin name.
#
# Environment variables:
#   TOOLPATH_INSTALL_DIR  Override the global install directory
#   TOOLPATH_CONFIG_DIR   Override ~/.toolpath (fallback bin lives under it)

set -euo pipefail

REPO="empathic/toolpath"
INSTALL_DIR="${TOOLPATH_INSTALL_DIR:-$HOME/.local/bin}"
FALLBACK_DIR="${TOOLPATH_CONFIG_DIR:-$HOME/.toolpath}/bin"
# Oldest release whose CLI surface the plugin's commands are written against.
MIN_VERSION="0.15.0"
TMPDIR_CLEANUP=""

log() { echo "$@" >&2; }

is_toolpath() {
    "$1" --help 2>/dev/null | head -1 | grep -qi "toolpath"
}

warn_if_old() {
    local bin="$1" version
    version="$("$bin" --version 2>/dev/null | awk '{print $2}')" || return 0
    [ -n "$version" ] || return 0
    if [ "$(printf '%s\n%s\n' "$MIN_VERSION" "$version" | sort -V | head -1)" != "$MIN_VERSION" ]; then
        log "warning: path ${version} is older than ${MIN_VERSION}; some plugin commands may not work."
        log "Upgrade: curl --proto '=https' --tlsv1.2 -fsS https://toolpath.net/install.sh | bash"
    fi
}

resolve_existing() {
    local candidate
    if candidate="$(command -v path 2>/dev/null)" && is_toolpath "$candidate"; then
        echo "$candidate"
        return 0
    fi
    for candidate in "$INSTALL_DIR/path" "$FALLBACK_DIR/path"; do
        if [ -x "$candidate" ] && is_toolpath "$candidate"; then
            echo "$candidate"
            return 0
        fi
    done
    return 1
}

choose_install_dir() {
    # A `path` binary that isn't the Toolpath CLI already claims the name
    # (resolve_existing ran first, so anything found here is foreign) —
    # install under ~/.toolpath/bin instead of shadow-fighting it.
    if command -v path >/dev/null 2>&1 || [ -e "$INSTALL_DIR/path" ]; then
        echo "$FALLBACK_DIR"
    else
        echo "$INSTALL_DIR"
    fi
}

check_dependencies() {
    local missing=()
    for cmd in curl tar; do
        if ! command -v "$cmd" &>/dev/null; then
            missing+=("$cmd")
        fi
    done
    if ! command -v sha256sum &>/dev/null && ! command -v shasum &>/dev/null; then
        missing+=("sha256sum or shasum")
    fi
    if [ ${#missing[@]} -gt 0 ]; then
        log "Error: required commands not found: ${missing[*]}"
        exit 1
    fi
}

resolve_target() {
    local os arch
    case "$(uname -s)" in
        Linux)  os="linux" ;;
        Darwin) os="macos" ;;
        *)      cargo_fallback "unsupported OS '$(uname -s)'" ;;
    esac
    case "$(uname -m)" in
        x86_64|amd64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *)             cargo_fallback "unsupported architecture '$(uname -m)'" ;;
    esac
    case "${os}-${arch}" in
        macos-aarch64) echo "aarch64-apple-darwin" ;;
        linux-x86_64)  echo "x86_64-unknown-linux-musl" ;;
        linux-aarch64) echo "aarch64-unknown-linux-gnu" ;;
        *)             cargo_fallback "no prebuilt binary for ${os}-${arch}" ;;
    esac
}

cargo_fallback() {
    log "Error: $1."
    log "Install the Toolpath CLI manually instead: cargo install path-cli"
    exit 1
}

fetch_latest_version() {
    local url version
    url="$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest")"
    version="${url##*/}"
    if [ -z "$version" ] || [ "$version" = "latest" ]; then
        log "Error: could not determine the latest release."
        log "Check https://github.com/${REPO}/releases"
        exit 1
    fi
    echo "$version"
}

download_and_verify() {
    local version="$1" target="$2" tmpdir="$3"
    local base_url="https://github.com/${REPO}/releases/download/${version}"
    local tarball="path-${target}.tar.gz"

    log "Downloading ${tarball}..."
    curl -fsSL "${base_url}/${tarball}" -o "${tmpdir}/${tarball}"
    curl -fsSL "${base_url}/${tarball}.sha256" -o "${tmpdir}/${tarball}.sha256"

    (
        cd "$tmpdir"
        log "Verifying checksum..."
        if command -v sha256sum &>/dev/null; then
            sha256sum -c "${tarball}.sha256" >&2
        else
            shasum -a 256 -c "${tarball}.sha256" >&2
        fi
        tar xzf "$tarball"
    )
}

path_hint() {
    local dir="$1"
    case ":$PATH:" in
        *":${dir}:"*) ;;
        *)
            log ""
            log "Note: ${dir} is not in your PATH. To use \`path\` from your own shell, add:"
            log "  export PATH=\"${dir}:\$PATH\""
            ;;
    esac
}

# Sets $RESOLVED_BIN. Deliberately not invoked via command substitution:
# $(...) subshells don't inherit `set -e` (without bash 4.4's inherit_errexit,
# which macOS's bash 3.2 lacks), and this chain must abort on a failed
# download or checksum.
install_path() {
    check_dependencies
    local target version dest_dir
    target="$(resolve_target)"
    version="$(fetch_latest_version)"
    dest_dir="$(choose_install_dir)"

    log "Installing path ${version} (${target}) to ${dest_dir}..."
    TMPDIR_CLEANUP="$(mktemp -d)"
    trap 'rm -rf "$TMPDIR_CLEANUP"' EXIT

    download_and_verify "$version" "$target" "$TMPDIR_CLEANUP"
    mkdir -p "$dest_dir"
    mv "${TMPDIR_CLEANUP}/path" "${dest_dir}/path"
    chmod +x "${dest_dir}/path"

    log "Installed path to ${dest_dir}/path"
    path_hint "$dest_dir"
    RESOLVED_BIN="${dest_dir}/path"
}

main() {
    # Needs no binary — answer before resolution so it can never block or
    # trigger a download.
    if [ "${1:-}" = "current-session" ]; then
        echo "${CLAUDE_CODE_SESSION_ID:-unknown}"
        return 0
    fi

    local bin
    if ! bin="$(resolve_existing)"; then
        install_path
        bin="$RESOLVED_BIN"
    fi
    warn_if_old "$bin"

    case "${1:-}" in
        exec)
            shift
            exec "$bin" "$@"
            ;;
        sessions)
            exec "$bin" p list claude --project "$PWD" --format tsv
            ;;
        "")
            echo "$bin"
            ;;
        *)
            log "usage: ensure-path.sh [exec <args...> | sessions | current-session]"
            exit 2
            ;;
    esac
}

main "$@"
