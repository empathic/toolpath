#!/usr/bin/env bash
# Install path — Toolpath CLI for artifact transformation provenance
# Usage: curl --proto '=https' --tlsv1.2 -fsS https://toolpath.net/install.sh | bash
#
# Environment variables:
#   TOOLPATH_INSTALL_DIR  Override install directory (default: ~/.local/bin)

set -euo pipefail

REPO="empathic/toolpath"
INSTALL_DIR="${TOOLPATH_INSTALL_DIR:-$HOME/.local/bin}"
TMPDIR_CLEANUP=""

main() {
    check_dependencies

    local os arch target version
    os="$(detect_os)"
    arch="$(detect_arch)"
    target="$(resolve_target "$os" "$arch")"
    version="$(fetch_latest_version)"

    echo "Installing path ${version} (${target}) to ${INSTALL_DIR}..."

    TMPDIR_CLEANUP="$(mktemp -d)"
    trap 'rm -rf "$TMPDIR_CLEANUP"' EXIT

    download_and_verify "$version" "$target" "$TMPDIR_CLEANUP"
    install_binary "$TMPDIR_CLEANUP"

    echo "Installed path to ${INSTALL_DIR}/path"
    check_path
    echo "Run 'path --help' to get started."
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
        echo "Error: required commands not found: ${missing[*]}" >&2
        exit 1
    fi
}

detect_os() {
    case "$(uname -s)" in
        Linux)  echo "linux" ;;
        Darwin) echo "macos" ;;
        *)
            echo "Error: unsupported OS '$(uname -s)'. Toolpath supports macOS and Linux." >&2
            exit 1
            ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)  echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *)
            echo "Error: unsupported architecture '$(uname -m)'." >&2
            exit 1
            ;;
    esac
}

resolve_target() {
    local os="$1" arch="$2"
    case "${os}-${arch}" in
        macos-aarch64) echo "aarch64-apple-darwin" ;;
        linux-x86_64)  echo "x86_64-unknown-linux-musl" ;;
        linux-aarch64) echo "aarch64-unknown-linux-gnu" ;;
        macos-x86_64)
            echo "Error: no prebuilt binary for Intel Mac." >&2
            echo "Install via Cargo instead: cargo install path-cli" >&2
            exit 1
            ;;
        *)
            echo "Error: unsupported platform '${os}-${arch}'." >&2
            exit 1
            ;;
    esac
}

fetch_latest_version() {
    local url version
    url="$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest")"
    version="${url##*/}"
    if [ -z "$version" ]; then
        echo "Error: could not determine latest release." >&2
        echo "Check https://github.com/${REPO}/releases" >&2
        exit 1
    fi
    echo "$version"
}

download_and_verify() {
    local version="$1" target="$2" tmpdir="$3"
    local base_url="https://github.com/${REPO}/releases/download/${version}"
    local tarball="path-${target}.tar.gz"

    echo "Downloading ${tarball}..."
    curl -fsSL "${base_url}/${tarball}" -o "${tmpdir}/${tarball}"
    curl -fsSL "${base_url}/${tarball}.sha256" -o "${tmpdir}/${tarball}.sha256"

    cd "$tmpdir"
    echo "Verifying checksum..."
    if command -v sha256sum &>/dev/null; then
        sha256sum -c "${tarball}.sha256"
    else
        shasum -a 256 -c "${tarball}.sha256"
    fi

    tar xzf "$tarball"
}

install_binary() {
    local tmpdir="$1"
    mkdir -p "$INSTALL_DIR"
    mv "${tmpdir}/path" "${INSTALL_DIR}/path"
    chmod +x "${INSTALL_DIR}/path"
}

check_path() {
    case ":$PATH:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            echo ""
            echo "Note: ${INSTALL_DIR} is not in your PATH. Add it with:"
            echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
            echo ""
            ;;
    esac
}

main "$@"
