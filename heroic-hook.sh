#!/bin/sh
# Heroic Launcher "after game closes" hook.
#
# Heroic runs this inside its Flatpak sandbox, where it cannot start processes
# on the host (flatpak-spawn --host is not allowed). Instead, it touches a
# trigger file inside Heroic's config directory; on the host, a systemd user
# path unit watches that file and starts heroic-save-backup.service.
set -u

HEROIC_DIR="${HEROIC_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/heroic}"
TRIGGER="$HEROIC_DIR/.heroic-save-backup-trigger"

[ -d "$HEROIC_DIR" ] || exit 0

date +%s > "$TRIGGER" 2>/dev/null || touch "$TRIGGER" 2>/dev/null || exit 0
