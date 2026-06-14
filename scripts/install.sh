#!/usr/bin/env bash
#
# cust installer — downloads a release build of the `cust` driver and
# its matching clang plugin from GitHub Releases, verifies the
# checksum, and installs both into a single directory on your PATH.
#
# Quick start:
#
#   curl -fsSL https://raw.githubusercontent.com/youyuanwu/cust/main/scripts/install.sh | bash
#
# Or download and run with options:
#
#   ./install.sh [--version vX.Y.Z] [--dir DIR] [--llvm MAJOR]
#
# Environment overrides (flags win over env):
#
#   CUST_VERSION       release tag to install (default: latest)
#   CUST_INSTALL_DIR   install directory (default: $HOME/.local/bin)
#   CUST_LLVM_MAJOR    LLVM major to match (default: detected from clang)
#
# Why LLVM matters: `libcust_plugin.so` is a clang plugin, ABI-coupled
# to the LLVM major version it was built against. The installer picks
# the release artifact matching your local `clang` so the plugin loads.
# `cust` finds the plugin by looking next to its own binary, so the
# driver and the plugin are always installed together in one directory.

set -euo pipefail

REPO="youyuanwu/cust"

# ── defaults (env, overridable by flags) ────────────────────────────
VERSION="${CUST_VERSION:-latest}"
INSTALL_DIR="${CUST_INSTALL_DIR:-${HOME}/.local/bin}"
LLVM_MAJOR="${CUST_LLVM_MAJOR:-}"

err() { printf 'error: %s\n' "$*" >&2; exit 1; }
info() { printf '%s\n' "$*" >&2; }

usage() {
  sed -n '2,30p' "$0" | sed 's/^#\s\?//'
  exit "${1:-0}"
}

# ── parse flags ─────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
    --version=*) VERSION="${1#*=}"; shift ;;
    --dir) INSTALL_DIR="${2:?--dir needs a value}"; shift 2 ;;
    --dir=*) INSTALL_DIR="${1#*=}"; shift ;;
    --llvm) LLVM_MAJOR="${2:?--llvm needs a value}"; shift 2 ;;
    --llvm=*) LLVM_MAJOR="${1#*=}"; shift ;;
    -h|--help) usage 0 ;;
    *) err "unknown argument: $1 (try --help)" ;;
  esac
done

# ── tool discovery ──────────────────────────────────────────────────
have() { command -v "$1" >/dev/null 2>&1; }

if have curl; then
  dl() { curl -fSL --retry 3 -o "$2" "$1"; }
  dl_stdout() { curl -fsSL "$1"; }
elif have wget; then
  dl() { wget -q -O "$2" "$1"; }
  dl_stdout() { wget -qO- "$1"; }
else
  err "need either curl or wget on PATH"
fi

if have sha256sum; then
  sha256() { sha256sum "$1" | awk '{print $1}'; }
elif have shasum; then
  sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
else
  err "need either sha256sum or shasum on PATH"
fi

# ── detect platform target triple ───────────────────────────────────
os="$(uname -s)"
arch="$(uname -m)"
case "${os}-${arch}" in
  Linux-x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
  *) err "unsupported platform: ${os}/${arch} (only x86_64 Linux is published today)" ;;
esac

# ── detect LLVM major from clang (unless overridden) ────────────────
if [[ -z "${LLVM_MAJOR}" ]]; then
  have clang || err "clang not found on PATH; install clang or pass --llvm <major>"
  LLVM_MAJOR="$(clang --version | grep -oE 'clang version [0-9]+' | grep -oE '[0-9]+' | head -1)"
  [[ -n "${LLVM_MAJOR}" ]] || err "could not parse clang major version; pass --llvm <major>"
  info "detected clang major version: ${LLVM_MAJOR}"
fi

# ── resolve the version tag ─────────────────────────────────────────
if [[ "${VERSION}" == "latest" ]]; then
  info "resolving latest release tag…"
  VERSION="$(dl_stdout "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep -oE '"tag_name"\s*:\s*"[^"]+"' | head -1 | grep -oE 'v[0-9][^"]*')" \
    || err "could not resolve latest release tag from the GitHub API"
  [[ -n "${VERSION}" ]] || err "could not resolve latest release tag"
fi
# Normalise: accept both "0.4.6" and "v0.4.6".
[[ "${VERSION}" == v* ]] || VERSION="v${VERSION}"
info "installing cust ${VERSION} (${TARGET}, llvm${LLVM_MAJOR})"

# ── compose asset URLs ──────────────────────────────────────────────
name="cust-${VERSION}-${TARGET}-llvm${LLVM_MAJOR}"
base="https://github.com/${REPO}/releases/download/${VERSION}"
tarball_url="${base}/${name}.tar.gz"
checksum_url="${base}/${name}.tar.gz.sha256"

# ── download + verify into a temp dir ───────────────────────────────
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

info "downloading ${name}.tar.gz…"
dl "${tarball_url}" "${tmp}/${name}.tar.gz" \
  || err "download failed: ${tarball_url}
no artifact for llvm${LLVM_MAJOR}? check published versions at
https://github.com/${REPO}/releases/tag/${VERSION}"

info "downloading checksum…"
dl "${checksum_url}" "${tmp}/${name}.tar.gz.sha256" \
  || err "checksum download failed: ${checksum_url}"

expected="$(awk '{print $1}' "${tmp}/${name}.tar.gz.sha256")"
actual="$(sha256 "${tmp}/${name}.tar.gz")"
[[ "${expected}" == "${actual}" ]] \
  || err "checksum mismatch:
  expected ${expected}
  actual   ${actual}"
info "checksum ok"

# ── extract + install ───────────────────────────────────────────────
tar -C "${tmp}" -xzf "${tmp}/${name}.tar.gz"
src="${tmp}/${name}"
[[ -f "${src}/cust" && -f "${src}/libcust_plugin.so" ]] \
  || err "archive layout unexpected: cust / libcust_plugin.so missing"

mkdir -p "${INSTALL_DIR}"
# The driver discovers the plugin next to its own binary, so both must
# land in the same directory.
install -m 0755 "${src}/cust" "${INSTALL_DIR}/cust"
install -m 0644 "${src}/libcust_plugin.so" "${INSTALL_DIR}/libcust_plugin.so"

info ""
info "installed:"
info "  ${INSTALL_DIR}/cust"
info "  ${INSTALL_DIR}/libcust_plugin.so"

# ── PATH hint ───────────────────────────────────────────────────────
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    info ""
    info "note: ${INSTALL_DIR} is not on your PATH. Add it, e.g.:"
    info "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.bashrc"
    ;;
esac

info ""
"${INSTALL_DIR}/cust" --version
