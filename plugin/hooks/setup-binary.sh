#!/bin/bash
# Legion plugin: ensure binary and channel dependencies are available.
# Downloads the correct platform binary from GitHub Releases on first run
# or version mismatch. Uses CLAUDE_PLUGIN_DATA for persistent storage.
set -euo pipefail

REPO="runlegion/legion"
BINARY_NAME="legion"

# Read version from plugin.json -- one version for binary and plugin.
PLUGIN_JSON="${CLAUDE_PLUGIN_ROOT:-}/.claude-plugin/plugin.json"
if [ -f "$PLUGIN_JSON" ]; then
  EXPECTED_VERSION=$(grep '"version"' "$PLUGIN_JSON" | head -1 | sed 's/.*: *"\([^"]*\)".*/\1/')
fi
EXPECTED_VERSION="${EXPECTED_VERSION:-0.3.0}"

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
  install_binary || echo "[legion] binary install failed (exit $?) -- continuing with CLAUDE.md setup" >&2
fi

# Persist resolved binary path so bin/legion can find it outside hook context.
PLUGIN_DIR="$(cd "$(dirname "$0")/.." && pwd)"
if [ -x "$BINARY_PATH" ]; then
  echo "$BINARY_PATH" > "${PLUGIN_DIR}/.legion-binary-path"
fi

# -- CLAUDE.md instructions ----------------------------------------------------

# Append legion instructions to ~/.claude/CLAUDE.md if not present.
# Idempotent: checks for marker before writing.
CLAUDE_MD="$HOME/.claude/CLAUDE.md"
MARKER="<!-- legion-plugin -->"

if [ -f "$CLAUDE_MD" ] && grep -qF "$MARKER" "$CLAUDE_MD" 2>/dev/null; then
  # Already installed -- nothing to do
  :
else
  mkdir -p "$HOME/.claude"
  cat >> "$CLAUDE_MD" << 'LEGION_EOF'

<!-- legion-plugin -->
## Legion

You have institutional memory. Check it before grepping for decisions or patterns.

- `legion recall --repo <name> --context "problem"` -- search your reflections
- `legion consult --context "problem"` -- search all agents across repos
- `legion bullpen --repo <name>` -- read team posts
- `legion kanban list --repo <name>` -- your task board

Work source commands (required -- direct `gh` is blocked):
- `legion issue create --repo <name> --title '...' --body '...'`
- `legion pr create --repo <name> --title '...' --body '...'`
- `legion pr list --repo <name>`
- `legion pr review --repo <name> --number <n> --approve --body 'LGTM'`
- `legion pr merge --repo <name> --number <n> --task <card-id>`
- `legion comment --repo <name> --number <n> --body '...'`
<!-- /legion-plugin -->
LEGION_EOF
  echo "[legion] added instructions to ${CLAUDE_MD}" >&2
fi

# -- Long-lived services -------------------------------------------------------
# Channel MCP and watch should always be running. They outlive agent sessions
# so signals can wake sleeping agents even when no session is active.
LEGION_PORT="${LEGION_PORT:-3131}"

# Channel MCP server
if [ -n "${CLAUDE_PLUGIN_ROOT:-}" ] && [ -f "${CLAUDE_PLUGIN_ROOT}/channel/index.ts" ]; then
  if ! lsof -iTCP:"$LEGION_PORT" -sTCP:LISTEN -t >/dev/null 2>&1; then
    if command -v bun >/dev/null 2>&1; then
      nohup bun run "${CLAUDE_PLUGIN_ROOT}/channel/index.ts" >/dev/null 2>&1 &
    fi
  fi
fi

# Watch (auto-wake agents on signal arrival)
if command -v legion >/dev/null 2>&1; then
  if ! pgrep -f "legion watch" >/dev/null 2>&1; then
    nohup legion watch >/dev/null 2>&1 &
  fi
fi
