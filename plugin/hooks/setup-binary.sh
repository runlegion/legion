#!/bin/bash
# Legion plugin: ensure binary and channel dependencies are available.
# Downloads the correct platform binary from GitHub Releases on first run
# or version mismatch. Uses CLAUDE_PLUGIN_DATA for persistent storage.
set -euo pipefail

REPO="runlegion/legion"
BINARY_NAME="legion"
EXPECTED_VERSION="0.1.1"

# -- Channel dependencies ----------------------------------------------------

# Install channel MCP dependencies if missing (bun required for the channel)
CHANNEL_DIR="${CLAUDE_PLUGIN_ROOT:-}/channel"
if [ -d "$CHANNEL_DIR" ] && [ -f "$CHANNEL_DIR/package.json" ] && [ ! -d "$CHANNEL_DIR/node_modules" ]; then
  if command -v bun >/dev/null 2>&1; then
    echo "[legion] installing channel dependencies..." >&2
    (cd "$CHANNEL_DIR" && bun install) >&2 || true
  fi
fi

# -- Binary setup -------------------------------------------------------------

# CLAUDE_PLUGIN_DATA persists across plugin updates; fall back to plugin root
DATA_DIR="${CLAUDE_PLUGIN_DATA:-${CLAUDE_PLUGIN_ROOT:-.}}"
BINARY_PATH="${DATA_DIR}/${BINARY_NAME}"

# Check if we already have the right version
if [ -x "$BINARY_PATH" ]; then
  INSTALLED=$("$BINARY_PATH" --version 2>/dev/null | awk '{print $2}' || echo "")
  if [ "$INSTALLED" = "$EXPECTED_VERSION" ]; then
    exit 0
  fi
fi

# Also check system PATH for an existing installation
SYSTEM_LEGION=$(command -v legion 2>/dev/null || true)
if [ -n "$SYSTEM_LEGION" ] && [ -x "$SYSTEM_LEGION" ]; then
  SYSTEM_VER=$("$SYSTEM_LEGION" --version 2>/dev/null | awk '{print $2}' || echo "")
  if [ "$SYSTEM_VER" = "$EXPECTED_VERSION" ]; then
    exit 0
  fi
fi

# Detect platform
detect_platform() {
  case "$(uname -s)" in
    Linux)  echo "linux" ;;
    Darwin) echo "macos" ;;
    *)      echo "unsupported" ;;
  esac
}

# Detect architecture with Rosetta 2 awareness
detect_arch() {
  local platform="$1"
  if [ "$platform" = "macos" ]; then
    local translated
    translated="$(sysctl -n sysctl.proc_translated 2>/dev/null || echo "0")"
    if [ "$translated" = "1" ]; then
      echo "arm64"
      return
    fi
  fi
  case "$(uname -m)" in
    x86_64|amd64)  echo "x64" ;;
    arm64|aarch64)  echo "arm64" ;;
    *)              echo "unsupported" ;;
  esac
}

PLATFORM=$(detect_platform)
ARCH=$(detect_arch "$PLATFORM")

# Exit 0 on unsupported platform so the hook does not block the session.
# The user can still install manually via cargo.
if [ "$PLATFORM" = "unsupported" ] || [ "$ARCH" = "unsupported" ]; then
  echo "[legion] unsupported platform: $(uname -s) $(uname -m)" >&2
  echo "[legion] install manually: cargo install --git https://github.com/${REPO}" >&2
  exit 0
fi

ARTIFACT="${BINARY_NAME}-${PLATFORM}-${ARCH}"
VERSION_TAG="v${EXPECTED_VERSION}"
BASE_URL="https://github.com/${REPO}/releases/download/${VERSION_TAG}"
ARCHIVE_URL="${BASE_URL}/${ARTIFACT}.tar.gz"
CHECKSUM_URL="${BASE_URL}/checksums.txt"

echo "[legion] downloading ${ARTIFACT} ${VERSION_TAG}..." >&2

TMPDIR_SETUP=$(mktemp -d)
trap 'rm -rf "$TMPDIR_SETUP"' EXIT

# Download archive and checksums
if ! curl -fsSL -o "${TMPDIR_SETUP}/${ARTIFACT}.tar.gz" "$ARCHIVE_URL"; then
  echo "[legion] download failed -- release may not exist yet for ${VERSION_TAG}" >&2
  echo "[legion] install manually: cargo install --git https://github.com/${REPO}" >&2
  exit 0
fi

if ! curl -fsSL -o "${TMPDIR_SETUP}/checksums.txt" "$CHECKSUM_URL"; then
  echo "[legion] checksum download failed -- refusing to install unverified binary" >&2
  exit 1
fi

EXPECTED=$(grep -F "${ARTIFACT}.tar.gz" "${TMPDIR_SETUP}/checksums.txt" | awk '{print $1}')
if [ -z "$EXPECTED" ]; then
  echo "[legion] no checksum found for ${ARTIFACT}.tar.gz" >&2
  exit 1
fi

if command -v shasum >/dev/null 2>&1; then
  ACTUAL=$(shasum -a 256 "${TMPDIR_SETUP}/${ARTIFACT}.tar.gz" | awk '{print $1}')
else
  ACTUAL=$(sha256sum "${TMPDIR_SETUP}/${ARTIFACT}.tar.gz" | awk '{print $1}')
fi

if [ "$EXPECTED" != "$ACTUAL" ]; then
  echo "[legion] checksum mismatch -- download may be corrupted" >&2
  exit 1
fi

# Extract and install
mkdir -p "$DATA_DIR"
tar xzf "${TMPDIR_SETUP}/${ARTIFACT}.tar.gz" -C "$TMPDIR_SETUP"
mv "${TMPDIR_SETUP}/${BINARY_NAME}" "$BINARY_PATH"
chmod +x "$BINARY_PATH"

echo "[legion] installed ${BINARY_NAME} ${VERSION_TAG} to ${BINARY_PATH}" >&2
