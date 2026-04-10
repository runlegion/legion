#!/bin/bash
# Legion plugin: ensure binary is available.
# Downloads the correct platform binary from GitHub Releases on first run
# or version mismatch. Uses CLAUDE_PLUGIN_DATA for persistent storage.
#
# The channel MCP server is now built into the legion binary itself
# (legion daemon --mcp). No Bun or Node.js dependency required.
set -euo pipefail

REPO="runlegion/legion"
BINARY_NAME="legion"

# Read version from plugin.json -- one version for binary and plugin.
PLUGIN_JSON="${CLAUDE_PLUGIN_ROOT:-}/.claude-plugin/plugin.json"
if [ -f "$PLUGIN_JSON" ]; then
  EXPECTED_VERSION=$(grep '"version"' "$PLUGIN_JSON" | head -1 | sed 's/.*: *"\([^"]*\)".*/\1/')
fi
EXPECTED_VERSION="${EXPECTED_VERSION:-0.3.0}"

# -- Binary setup -------------------------------------------------------------

# CLAUDE_PLUGIN_DATA persists across plugin updates; fall back to plugin root
DATA_DIR="${CLAUDE_PLUGIN_DATA:-${CLAUDE_PLUGIN_ROOT:-.}}"
BINARY_PATH="${DATA_DIR}/${BINARY_NAME}"

# Check if we already have the right version
NEED_BINARY=true
if [ -x "$BINARY_PATH" ]; then
  INSTALLED=$("$BINARY_PATH" --version 2>/dev/null | awk '{print $2}' || echo "")
  if [ "$INSTALLED" = "$EXPECTED_VERSION" ]; then
    NEED_BINARY=false
  fi
fi

# Also check system PATH for an existing installation
if [ "$NEED_BINARY" = true ]; then
  SYSTEM_LEGION=$(command -v legion 2>/dev/null || true)
  if [ -n "$SYSTEM_LEGION" ] && [ -x "$SYSTEM_LEGION" ]; then
    SYSTEM_VER=$("$SYSTEM_LEGION" --version 2>/dev/null | awk '{print $2}' || echo "")
    if [ "$SYSTEM_VER" = "$EXPECTED_VERSION" ]; then
      NEED_BINARY=false
    fi
  fi
fi

# Download and install binary (failures are non-fatal -- CLAUDE.md setup still runs)
install_binary() {
  # Detect platform
  local platform arch
  case "$(uname -s)" in
    Linux)  platform="linux" ;;
    Darwin) platform="macos" ;;
    *)
      echo "[legion] unsupported platform: $(uname -s) $(uname -m)" >&2
      echo "[legion] install manually: cargo install --git https://github.com/${REPO}" >&2
      return 0
      ;;
  esac

  if [ "$platform" = "macos" ]; then
    local translated
    translated="$(sysctl -n sysctl.proc_translated 2>/dev/null || echo "0")"
    if [ "$translated" = "1" ]; then
      arch="arm64"
    fi
  fi
  if [ -z "${arch:-}" ]; then
    case "$(uname -m)" in
      x86_64|amd64)   arch="x64" ;;
      arm64|aarch64)   arch="arm64" ;;
      *)
        echo "[legion] unsupported arch: $(uname -m)" >&2
        return 0
        ;;
    esac
  fi

  local artifact="${BINARY_NAME}-${platform}-${arch}"
  local version_tag="v${EXPECTED_VERSION}"
  local base_url="https://github.com/${REPO}/releases/download/${version_tag}"
  local tmpdir
  tmpdir=$(mktemp -d)

  echo "[legion] downloading ${artifact} ${version_tag}..." >&2

  if ! curl -fsSL -o "${tmpdir}/${artifact}.tar.gz" "${base_url}/${artifact}.tar.gz"; then
    echo "[legion] download failed -- install manually: cargo install --git https://github.com/${REPO}" >&2
    rm -rf "$tmpdir"
    return 0
  fi

  if ! curl -fsSL -o "${tmpdir}/checksums.txt" "${base_url}/checksums.txt"; then
    echo "[legion] checksum download failed -- refusing to install unverified binary" >&2
    rm -rf "$tmpdir"
    return 1
  fi

  local expected_sum actual_sum
  expected_sum=$(grep -F "${artifact}.tar.gz" "${tmpdir}/checksums.txt" | awk '{print $1}')
  if [ -z "$expected_sum" ]; then
    echo "[legion] no checksum found for ${artifact}.tar.gz" >&2
    rm -rf "$tmpdir"
    return 1
  fi

  if command -v shasum >/dev/null 2>&1; then
    actual_sum=$(shasum -a 256 "${tmpdir}/${artifact}.tar.gz" | awk '{print $1}')
  else
    actual_sum=$(sha256sum "${tmpdir}/${artifact}.tar.gz" | awk '{print $1}')
  fi

  if [ "$expected_sum" != "$actual_sum" ]; then
    echo "[legion] checksum mismatch -- download may be corrupted" >&2
    rm -rf "$tmpdir"
    return 1
  fi

  mkdir -p "$DATA_DIR"
  tar xzf "${tmpdir}/${artifact}.tar.gz" -C "$tmpdir"
  mv "${tmpdir}/${BINARY_NAME}" "$BINARY_PATH"
  chmod +x "$BINARY_PATH"
  rm -rf "$tmpdir"

  echo "[legion] installed ${BINARY_NAME} ${version_tag} to ${BINARY_PATH}" >&2
}

if [ "$NEED_BINARY" = true ]; then
  install_binary || echo "[legion] binary install failed (exit $?)" >&2
fi
