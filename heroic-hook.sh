#!/usr/bin/env bash
# Heroic Launcher "after game closes" hook.
# Heroic runs Flatpak apps inside a sandbox, so delegate to the host binary.
set -u

BIN="${GAME_SAVES_BIN:-$HOME/.local/bin/heroic-save-backup}"

if [ -n "${FLATPAK_ID:-}" ] && command -v flatpak-spawn >/dev/null 2>&1; then
  exec flatpak-spawn --host --directory="$HOME" "$BIN" --quiet "$@"
fi

exec "$BIN" --quiet "$@"
