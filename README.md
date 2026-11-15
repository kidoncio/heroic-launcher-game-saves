# heroic-launcher-game-saves

Automatically back up **Heroic Launcher** game saves to Git/GitHub. One Rust
binary, no Python or `rsync` needed. Works with Epic, GOG, Amazon Games
(the "Amazon Luna" tab in Heroic) and sideloaded games, plus extra Wine
prefixes under `~/Games`.

Save **data** lives in a separate repository (usually private) — not in this
code repository. That way you can share the tool without exposing your saves.

## Tested on

The tool was written and tested on this setup:

| Component | Version |
| --- | --- |
| OS | Bazzite 45 (Fedora 45 based) |
| Kernel | 7.3.3 |
| Heroic Launcher | Flatpak `v2.22.4` (Flathub, stable) |
| Rust | 1.99.1 (`cargo build --release`) |
| systemd | 262 |

What was exercised:

- Unit tests (`cargo test`): title/path normalization, game-folder matching,
  cache/log exclusion, native-vs-Windows detection, `~` expansion.
- Integration with fixture repositories: Epic, GOG, Amazon (Nile) and
  sideload libraries; Wine/Proton prefixes; native Linux save paths; extra
  prefixes in `~/Games`; `saveFolder` overrides; `extra_paths.tsv`; repeated
  runs (idempotency) and excluded files.
- Real-machine smoke test: dry-run and live runs against Heroic Flatpak,
  including a systemd timer and the after-game hook.

> Other distributions should work (it only uses `git`, the filesystem and
> systemd user units), but they were not tested by the author. Native
> (non-Flatpak) and Snap Heroic config paths are auto-detected too.

## Features

- Discovers installed games from Heroic's own files; no manual game list.
- Finds saves inside Wine/Proton prefixes:
  `Saved Games`, `Documents/My Games`, `AppData/LocalLow`, plus game-named
  folders in `Documents`, `AppData/Local` and `AppData/Roaming`.
- Handles native Linux games (`~/.local/share`, `~/.config`, `~/Documents`,
  `~/.local/share/GOG.com`).
- Supports Heroic's per-game `saveFolder` and a manual `extra_paths.tsv`.
- **Multi-machine**: each computer writes to `saves/<machine>/`, so several
  computers can share the same private data repository.
- Commits/pushes only when something changed; rebases automatically before
  pushing.
- Daily systemd timer *and* an optional "right after the game closes" hook.
- Skips caches, logs, temp files and files larger than 100 MB.
- Never deletes old saves from the backup when a game is uninstalled.

## How it works

1. Reads Heroic's installed-game libraries (`legendaryConfig/`, `gog_store/`,
   `nile_store/` and `sideload_apps/`).
2. Reads `GamesConfig/*.json` for each game's Wine prefix and optional
   `saveFolder`.
3. Scans extra prefixes under `SECONDARY_ROOTS` (default `~/Games`) — any
   folder containing `drive_c` or `pfx/drive_c`.
4. Copies matching save folders to the data repository.
5. Writes `saves/<machine>/.index.tsv` describing the mapping.
6. Runs `git add`, commits and pushes.

Resulting layout in the private repository:

```
saves/
└── <machine>/
    ├── .index.tsv
    ├── extra_paths.tsv          # optional, per machine
    └── Game Title/
        ├── Saved Games/...
        ├── Documents/My Games/...
        ├── AppData/LocalLow/...
        ├── AppData/Roaming/<Game>/...
        ├── _saveFolder/<name>/  # Heroic saveFolder override
        ├── _extra/<name>/       # extra_paths.tsv entries
        └── _native/...          # native Linux games
```

## Requirements

- Linux.
- [Heroic Games Launcher](https://heroicgameslauncher.com/) with at least one
  game installed (Flatpak, native package or AppImage).
- `git` and `cargo` (install Rust from <https://rustup.rs>).
- Optional: [GitHub CLI](https://cli.github.com/) authenticated, to create the
  private data repository automatically.

## Install

```bash
git clone https://github.com/<you>/heroic-launcher-game-saves.git
cd heroic-launcher-game-saves
./install.sh --repo ~/heroic-save-backup
```

`install.sh` will:

1. Build with `cargo build --release` and install
   `~/.local/bin/heroic-save-backup`.
2. Install the Heroic hook as `~/.local/bin/heroic-save-backup-hook`.
3. Create the data repository (`--repo`, default `~/heroic-save-backup`),
   create a **private** GitHub repo with `gh` and push the first commit
   (unless a remote already exists).
4. Save the repo path and machine name to
   `~/.config/heroic-save-backup/{repo,machine}`.
5. Enable the daily systemd user timer (22:00 + up to 5 minutes of jitter).
6. Point Heroic's `afterLaunchScriptPath` at the hook.

Options:

```
--repo <PATH>        private data repository (default: ~/heroic-save-backup)
--machine <NAME>     machine folder inside the repo (default: hostname)
--heroic-dir <PATH>  Heroic config dir (default: auto-detected)
--no-github          do not create/configure a GitHub remote
--no-timer           do not install the systemd timer
```

> Close Heroic before running the installer so the hook can be written.
> Files changed by the installer get a `.bak-gamesaves` backup.

### Adding a second computer

Install on the other machine, pointing `--repo` at a clone of the same private
data repository:

```bash
git clone git@github.com:<you>/game-save-backup.git ~/heroic-save-backup
cd heroic-launcher-game-saves
./install.sh --repo ~/heroic-save-backup --machine my-laptop
```

Use a different `--machine` per computer (the hostname is the default). Each
machine writes to its own folder, and the tool rebases before pushing, so
backups do not overwrite each other.

## Usage

```bash
heroic-save-backup              # back up, commit and push
heroic-save-backup --dry-run    # show what would be copied; change nothing
heroic-save-backup --no-push    # commit locally without pushing
heroic-save-backup --quiet      # no desktop notifications
heroic-save-backup --help
```

| Option / variable | Meaning |
| --- | --- |
| `--repo <PATH>` | Git repository that stores the saves |
| `--machine <NAME>` | machine folder inside the repo |
| `--heroic-dir <PATH>` | Heroic config directory |
| `GAME_SAVES_REPO` | repository path (same as `--repo`) |
| `GAME_SAVES_MACHINE` | machine name (same as `--machine`) |
| `HEROIC_DIR` | Heroic config directory |
| `SECONDARY_ROOTS` | colon-separated dirs with extra Wine prefixes |
| `GAME_SAVES_STATE` | log/lock dir (default `~/.local/state/heroic-save-backup`) |

Heroic's config directory is auto-detected in this order:

1. Flatpak: `~/.var/app/com.heroicgameslauncher.hgl/config/heroic`
2. Native/AppImage: `~/.config/heroic`
3. Snap: `~/snap/heroic/current/.config/heroic`

### Logs and timer

```bash
tail -f ~/.local/state/heroic-save-backup/backup.log
systemctl --user list-timers heroic-save-backup.timer
systemctl --user start heroic-save-backup.service   # run now
journalctl --user -u heroic-save-backup.service -n 50
```

Change the schedule in `~/.config/systemd/user/heroic-save-backup.timer`, then:

```bash
systemctl --user daemon-reload
systemctl --user restart heroic-save-backup.timer
```

## What gets backed up

Per game, when present:

- `Saved Games`
- `Documents/My Games`
- `AppData/LocalLow`
- subfolders of `Documents`, `AppData/Local` and `AppData/Roaming` whose name
  matches the game title (e.g. `APlagueTaleInnocence`, `Overcooked2`)
- Heroic's `saveFolder` override, if configured
- paths listed in `extra_paths.tsv`
- native Linux save dirs, for games without a Wine prefix

Caches, logs, temp folders and `*.tmp/*.log/*.dmp/*.lock` files are skipped,
as are files larger than 100 MB.

### Stores and Linux-native games

| Store | Build on Linux | Save location |
| --- | --- | --- |
| Epic Games | Windows (Wine/Proton) | Wine prefix |
| GOG | Native Linux when available, else Windows | Native dirs or Wine prefix |
| Amazon Games ("Amazon Luna" tab) | Windows (Wine/Proton) | Wine prefix |
| Sideload | Whatever you added | Wine prefix |

> Heroic's **Amazon Luna** tab is Amazon Games/Prime Gaming powered by Nile:
> games are downloaded and run locally. Cloud-only Luna streaming is not part
> of Heroic and has no local save files to back up.

## Adding custom save paths

Create `saves/<machine>/extra_paths.tsv` (or `extra_paths.tsv` at the repo
root for all machines), one path per line, separated by a TAB:

```tsv
Game Title	/home/you/path/to/the/save
```

Everything inside that folder is copied to `saves/<machine>/Game Title/_extra/<folder>`.

## Restoring a save

Saves mirror the prefix layout. Example for Overcooked! 2:

```bash
rsync -a --dry-run "saves/bazzite/Overcooked! 2/" \
  ~/Games/Heroic/Prefixes/Overcooked\ 2/pfx/drive_c/users/steamuser/
# remove --dry-run to restore
```

The exact prefix for each game is listed in
`saves/<machine>/.index.tsv` or in Heroic's `GamesConfig/<app>.json`.

## Troubleshooting

- **Nothing is backed up** — the prefix does not exist yet. Launch the game
  once so Heroic creates it, then run the backup again.
- **Wrong prefix** — check `GamesConfig/<app>.json`; the tool uses its
  `winePrefix`, falling back to Heroic's default prefix directory.
- **Hook does nothing** — close and reopen Heroic, or rerun `./install.sh`
  while Heroic is closed, and check the log.
- **Push fails** — the commit stays local. Fix credentials and run
  `git -C <repo> push origin HEAD`.
- **Two machines** — make sure each one uses a different `--machine` name.

## Uninstall

```bash
systemctl --user disable --now heroic-save-backup.timer
rm -f ~/.config/systemd/user/heroic-save-backup.{service,timer}
systemctl --user daemon-reload
rm -f ~/.local/bin/heroic-save-backup ~/.local/bin/heroic-save-backup-hook
rm -rf ~/.config/heroic-save-backup
```

Remove the `afterLaunchScriptPath` entries from Heroic's config (or restore
the `.bak-gamesaves` files) if you no longer want the hook.

## Privacy

Game saves may contain profile data, screenshots or account identifiers. Keep
the **data** repository private. This code repository contains no save data.
Cloud save sync from Epic/GOG is independent and remains handled by Heroic.

## License

MIT — see [LICENSE](LICENSE).
