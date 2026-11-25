---
description: Sync working directories into watch.toml for auto-wake
argument-hint: ""
allowed-tools: ["Bash"]
---

# /watch-sync -- Sync working directories to watch.toml

Reads the configured working directories from Claude Code's project settings and syncs them into legion's `watch.toml` config for auto-wake.

## Steps

Run this bash script:

```bash
DATA_DIR="${HOME}/Library/Application Support/legion"
CONFIG="${DATA_DIR}/watch.toml"

# Create config with defaults if missing
if [ ! -f "$CONFIG" ]; then
  mkdir -p "$DATA_DIR"
  printf 'poll_interval_secs = 30\ncooldown_secs = 300\n' > "$CONFIG"
  echo "Created $CONFIG"
fi

# Collect directories: PWD (primary) + additionalDirectories from project settings
DIRS="$PWD"

# Check project-local settings first, then project settings
for settings_file in ".claude/settings.local.json" ".claude/settings.json"; do
  if [ -f "$settings_file" ]; then
    EXTRA=$(jq -r '.permissions.additionalDirectories[]? // empty' "$settings_file" 2>/dev/null)
    if [ -n "$EXTRA" ]; then
      DIRS="$DIRS
$EXTRA"
    fi
  fi
done

ADDED=0
SKIPPED=0

while IFS= read -r dir; do
  [ -z "$dir" ] && continue
  name=$(basename "$dir")
  if grep -q "name = \"$name\"" "$CONFIG" 2>/dev/null; then
    echo "  already present: $name"
    SKIPPED=$((SKIPPED + 1))
  else
    printf '\n[[repos]]\nname = "%s"\nworkdir = "%s"\n' "$name" "$dir" >> "$CONFIG"
    echo "  added: $name -> $dir"
    ADDED=$((ADDED + 1))
  fi
done <<< "$DIRS"

TOTAL=$(grep -c '^\[\[repos\]\]' "$CONFIG" 2>/dev/null || echo 0)
echo ""
echo "watch.toml: $TOTAL repos ($ADDED added, $SKIPPED already present)"
```

Report the results to the user.
