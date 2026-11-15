#!/usr/bin/env bash
# Installs the Heroic Launcher save backup (Rust binary):
#   - builds the binary and installs it to ~/.local/bin
#   - prepares the private data repository (local Git, optional GitHub)
#   - installs the daily systemd timer
#   - wires Heroic's "after game closes" hook
#
# This script lives in the public code repository. The save data lives in a
# separate (usually private) repository, passed with --repo or GAME_SAVES_REPO.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

REPO="${GAME_SAVES_REPO:-$HOME/heroic-save-backup}"
BIN="${GAME_SAVES_BIN:-$HOME/.local/bin/heroic-save-backup}"
HOOK_BIN="${GAME_SAVES_HOOK:-}"
MACHINE="${GAME_SAVES_MACHINE:-$(hostname 2>/dev/null || echo linux)}"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
CONFIG_DIR="$CONFIG_HOME/heroic-save-backup"
UNIT_DIR="$CONFIG_HOME/systemd/user"
SKIP_GITHUB="${SKIP_GITHUB:-0}"
SKIP_TIMER="${SKIP_TIMER:-0}"
HEROIC_DIR="${HEROIC_DIR:-}"

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mWARNING:\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: ./install.sh [OPTIONS]

  --repo <PATH>        private data repository (default: ~/heroic-save-backup)
  --machine <NAME>     machine folder inside the repo (default: hostname)
  --heroic-dir <PATH>  Heroic config directory (default: auto-detect)
  --no-github          do not create/configure a GitHub remote
  --no-timer           do not install the systemd timer
  -h, --help           show this help

Environment variables: GAME_SAVES_REPO, GAME_SAVES_MACHINE,
GAME_SAVES_BIN, GAME_SAVES_HOOK, HEROIC_DIR, SKIP_GITHUB, SKIP_TIMER.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)       REPO="${2:?missing value for --repo}"; shift 2 ;;
    --machine)    MACHINE="${2:?missing value for --machine}"; shift 2 ;;
    --heroic-dir) HEROIC_DIR="${2:?missing value for --heroic-dir}"; shift 2 ;;
    --no-github)  SKIP_GITHUB=1; shift ;;
    --no-timer)   SKIP_TIMER=1; shift ;;
    -h|--help)    usage; exit 0 ;;
    *)            die "unknown argument: $1 (see --help)" ;;
  esac
done

[[ "$(uname -s)" == "Linux" ]] || die "this tool targets Linux."
command -v git >/dev/null || die "git not found. Install it with your package manager."
command -v cargo >/dev/null || die "cargo (Rust) not found. Install it from https://rustup.rs"

detect_heroic() {
  local candidates=(
    "$HOME/.var/app/com.heroicgameslauncher.hgl/config/heroic"
    "$HOME/.config/heroic"
    "$HOME/snap/heroic/current/.config/heroic"
  )
  local candidate
  for candidate in "${candidates[@]}"; do
    [[ -d "$candidate" ]] && { printf '%s\n' "$candidate"; return 0; }
  done
  return 1
}

if [[ -z "$HEROIC_DIR" ]]; then
  HEROIC_DIR="$(detect_heroic || true)"
fi

if [[ -z "$HOOK_BIN" && -n "$HEROIC_DIR" ]]; then
  HOOK_BIN="$HEROIC_DIR/heroic-save-backup-hook.sh"
fi
TRIGGER_FILE="${HEROIC_DIR:+$HEROIC_DIR/.heroic-save-backup-trigger}"

info "Building heroic-save-backup (Rust)"
cd "$SCRIPT_DIR"
cargo build --release
install -Dm755 "$SCRIPT_DIR/target/release/heroic-save-backup" "$BIN"
if [[ -n "$HOOK_BIN" ]]; then
  install -Dm755 "$SCRIPT_DIR/heroic-hook.sh" "$HOOK_BIN"
fi
info "Installed $BIN"

mkdir -p "$CONFIG_DIR"
printf '%s\n' "$REPO" > "$CONFIG_DIR/repo"
printf '%s\n' "$MACHINE" > "$CONFIG_DIR/machine"

info "Preparing the data repository at $REPO"
mkdir -p "$REPO"
git -C "$REPO" init -q 2>/dev/null || true
git -C "$REPO" branch -M main 2>/dev/null || true

if [[ "$SKIP_GITHUB" != "1" ]]; then
  if ! command -v gh >/dev/null; then
    warn "gh (GitHub CLI) not found; skipping GitHub setup. Add a remote manually:"
    warn "  git -C \"$REPO\" remote add origin <url>"
  elif ! gh auth status >/dev/null 2>&1; then
    warn "gh is not authenticated; skipping GitHub setup. Run 'gh auth login' and rerun."
  elif ! git -C "$REPO" remote get-url origin >/dev/null 2>&1; then
    GH_OWNER="${GH_OWNER:-$(gh api user --jq .login)}"
    GH_REPO="${GH_REPO:-$(basename "$REPO")}"
    if gh repo view "$GH_OWNER/$GH_REPO" >/dev/null 2>&1; then
      info "Repository $GH_OWNER/$GH_REPO already exists; adding remote"
      git -C "$REPO" remote add origin "https://github.com/$GH_OWNER/$GH_REPO.git"
    else
      info "Creating private repository $GH_OWNER/$GH_REPO"
      gh repo create "$GH_OWNER/$GH_REPO" --private --source "$REPO" --remote origin
    fi
  fi
fi

# Keep the data repository free of build output and temporary files.
if [[ ! -f "$REPO/.gitignore" ]]; then
  cat > "$REPO/.gitignore" <<'EOF'
*.tmp
*.temp
*.lock
*.dmp
*.log
!Cargo.lock
EOF
fi

git -C "$REPO" add -A
git -C "$REPO" diff --cached --quiet || git -C "$REPO" commit -q -m "Configure automatic game save backup"
if git -C "$REPO" remote get-url origin >/dev/null 2>&1; then
  git -C "$REPO" push -u origin HEAD || warn "Initial push failed; the next backups will retry"
fi

if [[ "$SKIP_TIMER" != "1" ]]; then
  info "Installing the daily systemd timer (22:00)"
  mkdir -p "$UNIT_DIR"
  cat > "$UNIT_DIR/heroic-save-backup.service" <<EOF
[Unit]
Description=Back up Heroic Launcher game saves to Git
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
Environment=GAME_SAVES_REPO=$REPO
Environment=GAME_SAVES_MACHINE=$MACHINE
ExecStart=$BIN --quiet
Nice=10
IOSchedulingClass=idle
TimeoutStartSec=15min
EOF

  cat > "$UNIT_DIR/heroic-save-backup.timer" <<EOF
[Unit]
Description=Daily Heroic game save backup

[Timer]
OnCalendar=*-*-* 22:00:00
RandomizedDelaySec=5min
Persistent=true

[Install]
WantedBy=timers.target
EOF

  systemctl --user daemon-reload
  systemctl --user enable --now heroic-save-backup.timer
  info "Timer state: $(systemctl --user is-active heroic-save-backup.timer)"
fi

if [[ "$SKIP_TIMER" != "1" && -n "$TRIGGER_FILE" ]]; then
  info "Installing the after-game systemd path unit"
  mkdir -p "$UNIT_DIR"
  : > "$TRIGGER_FILE"
  cat > "$UNIT_DIR/heroic-save-backup.path" <<EOF
[Unit]
Description=Watch Heroic's after-game trigger

[Path]
PathChanged=$TRIGGER_FILE

[Install]
WantedBy=default.target
EOF
  systemctl --user daemon-reload
  systemctl --user enable --now heroic-save-backup.path
  info "Path unit state: $(systemctl --user is-active heroic-save-backup.path)"
fi

if [[ -n "$HEROIC_DIR" && -d "$HEROIC_DIR" && -n "$HOOK_BIN" ]]; then
  info "Wiring Heroic's after-game hook"
  if pgrep -f "heroic" >/dev/null 2>&1; then
    warn "Heroic seems to be running. Close it and run this script again to apply the hook."
  else
    python3 - "$HEROIC_DIR" "$HOOK_BIN" <<'PY'
import json, shutil, sys
from pathlib import Path

heroic, hook = Path(sys.argv[1]), sys.argv[2]
targets = [heroic / "config.json", *sorted((heroic / "GamesConfig").glob("*.json"))]
changed = 0
for path in targets:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        continue
    if not isinstance(data, dict):
        continue
    touched = False
    if path.name == "config.json":
        settings = data.setdefault("defaultSettings", {})
        if settings.get("afterLaunchScriptPath") != hook:
            settings["afterLaunchScriptPath"] = hook
            touched = True
    else:
        for cfg in data.values():
            if isinstance(cfg, dict) and "afterLaunchScriptPath" in cfg:
                if not cfg.get("afterLaunchScriptPath"):
                    cfg["afterLaunchScriptPath"] = hook
                    touched = True
    if touched:
        shutil.copy2(path, path.with_suffix(path.suffix + ".bak-gamesaves"))
        path.write_text(json.dumps(data, indent=2, ensure_ascii=False), encoding="utf-8")
        changed += 1
print(f"Heroic config files updated: {changed}")
PY
  fi
else
  warn "Heroic config directory not found; skipping the after-game hook."
fi

info "Running the first backup"
"$BIN" || warn "First backup returned an error (check the log below)"

info "Done!"
echo "  Data repo:  $REPO"
echo "  Machine:    $MACHINE"
echo "  Binary:     $BIN"
echo "  Hook:       ${HOOK_BIN:-not installed}"
echo "  Log:        ${XDG_STATE_HOME:-$HOME/.local/state}/heroic-save-backup/backup.log"
echo "  Timer:      systemctl --user list-timers heroic-save-backup.timer"
echo "  Run now:    $BIN"
