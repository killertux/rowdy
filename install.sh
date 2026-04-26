#!/bin/sh
# Installs the latest rowdy release from GitHub.
#
# Usage:
#   curl --proto '=https' --tlsv1.2 -sSf \
#     https://raw.githubusercontent.com/killertux/rowdy/main/install.sh | sh
#
# Environment overrides:
#   ROWDY_INSTALL_DIR  install location (default: $HOME/.local/bin)
#   ROWDY_VERSION      release tag to fetch (default: latest, e.g. v0.1.0)

set -eu

REPO="killertux/rowdy"
INSTALL_DIR="${ROWDY_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${ROWDY_VERSION:-latest}"

err() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

info() {
    printf '%s\n' "$1"
}

need() {
    command -v "$1" >/dev/null 2>&1 || err "missing required command: $1"
}

need uname
need tar
need mktemp

if command -v curl >/dev/null 2>&1; then
    DOWNLOADER="curl"
elif command -v wget >/dev/null 2>&1; then
    DOWNLOADER="wget"
else
    err "need either curl or wget on PATH"
fi

download() {
    _url="$1"
    _out="$2"
    case "$DOWNLOADER" in
        curl) curl --proto '=https' --tlsv1.2 -fsSL "$_url" -o "$_out" ;;
        wget) wget --https-only -q -O "$_out" "$_url" ;;
    esac
}

detect_target() {
    _os="$(uname -s)"
    _arch="$(uname -m)"
    case "$_os" in
        Linux)
            case "$_arch" in
                x86_64|amd64) echo "x86_64-unknown-linux-gnu" ;;
                aarch64|arm64) echo "aarch64-unknown-linux-gnu" ;;
                *) err "unsupported Linux architecture: $_arch" ;;
            esac
            ;;
        Darwin)
            case "$_arch" in
                arm64|aarch64) echo "aarch64-apple-darwin" ;;
                x86_64)
                    err "macOS x86_64 builds are not published — build from source: cargo install --git https://github.com/$REPO"
                    ;;
                *) err "unsupported macOS architecture: $_arch" ;;
            esac
            ;;
        *) err "unsupported OS: $_os (only Linux and macOS are supported)" ;;
    esac
}

resolve_version() {
    if [ "$VERSION" != "latest" ]; then
        echo "$VERSION"
        return
    fi
    _url="https://api.github.com/repos/$REPO/releases/latest"
    _tmp="$(mktemp)"
    download "$_url" "$_tmp"
    _tag="$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$_tmp" | head -n1)"
    rm -f "$_tmp"
    [ -n "$_tag" ] || err "could not resolve latest release tag from $_url"
    echo "$_tag"
}

main() {
    target="$(detect_target)"
    tag="$(resolve_version)"
    name="rowdy-${tag}-${target}"
    url="https://github.com/${REPO}/releases/download/${tag}/${name}.tar.gz"

    info "rowdy ${tag} (${target})"
    info "downloading ${url}"

    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT INT TERM HUP

    download "$url" "$tmp/rowdy.tar.gz"
    tar -C "$tmp" -xzf "$tmp/rowdy.tar.gz"

    src="$tmp/${name}/rowdy"
    [ -f "$src" ] || err "binary not found in archive at $src"

    mkdir -p "$INSTALL_DIR"
    mv "$src" "$INSTALL_DIR/rowdy"
    chmod +x "$INSTALL_DIR/rowdy"

    info ""
    info "installed: $INSTALL_DIR/rowdy"

    case ":${PATH:-}:" in
        *":$INSTALL_DIR:"*) ;;
        *)
            info ""
            info "warning: $INSTALL_DIR is not on your PATH"
            info "add this to your shell rc to fix:"
            info "  export PATH=\"$INSTALL_DIR:\$PATH\""
            ;;
    esac
}

main "$@"
