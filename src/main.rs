//! Automatic game save backup for the Heroic Launcher (and extra Wine
//! prefixes under ~/Games) into a Git repository on GitHub.
//!
//! Usage:
//!   heroic-save-backup             # backup + commit + push
//!   heroic-save-backup --dry-run   # simulate; no writes, commits or pushes
//!   heroic-save-backup --no-push   # backup + commit, without pushing
//!   heroic-save-backup --quiet     # no desktop notifications

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use chrono::Local;
use filetime::{FileTime, set_file_times};
use fs2::FileExt;
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;

/// Folders/files that never go into the backup (caches, logs, temporary data).
const EXCLUDED: &[&str] = &[
    "cache",
    "code cache",
    "gpucache",
    "shadercache",
    "crashpad",
    "crashes",
    "crash reports",
    "logs",
    "log",
    "temp",
    "tmp",
    "webcache",
    "local storage",
    "session storage",
    "blob storage",
    "blob_storage",
];

const EXCLUDED_SUFFIXES: &[&str] = &[".log", ".tmp", ".temp", ".dmp", ".lock"];

const STOPWORDS: &[&str] = &[
    "the",
    "a",
    "an",
    "of",
    "and",
    "or",
    "edition",
    "definitive",
    "deluxe",
    "goty",
    "complete",
    "remastered",
    "repack",
    "fitgirl",
    "pc",
    "game",
    "digital",
    "ultimate",
    "gold",
    "premium",
    "collection",
];

// ------------------------------------------------------------------ environment

struct Env {
    home: PathBuf,
    repo: PathBuf,
    heroic: PathBuf,
    secondary_roots: Vec<PathBuf>,
    state_dir: PathBuf,
    log_file: PathBuf,
    lock_file: PathBuf,
    machine: String,
    dry_run: bool,
    no_push: bool,
    quiet: bool,
}

impl Env {
    fn load() -> Self {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));

        let mut repo_arg = None;
        let mut heroic_arg = None;
        let mut machine_arg = None;
        let mut dry_run = false;
        let mut no_push = false;
        let mut quiet = false;

        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--dry-run" => dry_run = true,
                "--no-push" => no_push = true,
                "--quiet" => quiet = true,
                "--repo" => repo_arg = args.next().map(PathBuf::from),
                "--heroic-dir" => heroic_arg = args.next().map(PathBuf::from),
                "--machine" => machine_arg = args.next(),
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                "--version" | "-V" => {
                    println!("heroic-save-backup {}", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                other => {
                    eprintln!("unknown argument: {other}\n");
                    print_help();
                    std::process::exit(2);
                }
            }
        }

        let config_dir = config_home.join("heroic-save-backup");

        // Repo: --repo > GAME_SAVES_REPO > ~/.config/heroic-save-backup/repo
        //       > ~/heroic-save-backup
        let repo = repo_arg
            .or_else(|| env::var_os("GAME_SAVES_REPO").map(PathBuf::from))
            .or_else(|| read_config_value(&config_dir.join("repo")))
            .unwrap_or_else(|| home.join("heroic-save-backup"));

        let heroic = heroic_arg
            .or_else(|| env::var_os("HEROIC_DIR").map(PathBuf::from))
            .unwrap_or_else(|| detect_heroic(&home));

        let secondary_roots = env::var("SECONDARY_ROOTS")
            .unwrap_or_else(|_| home.join("Games").to_string_lossy().into_owned())
            .split(':')
            .filter(|p| !p.trim().is_empty())
            .map(|p| expand_tilde(p.trim(), &home))
            .collect();

        let state_dir = env::var_os("GAME_SAVES_STATE")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("XDG_STATE_HOME").map(|p| PathBuf::from(p).join("heroic-save-backup"))
            })
            .unwrap_or_else(|| home.join(".local/state/heroic-save-backup"));

        // Machine: --machine > GAME_SAVES_MACHINE > config file > hostname.
        // Each machine gets its own folder inside the backup repo.
        let machine = machine_arg
            .or_else(|| env::var("GAME_SAVES_MACHINE").ok())
            .or_else(|| {
                read_config_value(&config_dir.join("machine"))
                    .map(|p: PathBuf| p.to_string_lossy().into_owned())
            })
            .unwrap_or_else(hostname);
        let machine = safe_name(machine.trim());

        Self {
            log_file: state_dir.join("backup.log"),
            lock_file: state_dir.join("backup.lock"),
            dry_run,
            no_push,
            quiet,
            home,
            repo,
            heroic,
            secondary_roots,
            state_dir,
            machine,
        }
    }
}

fn read_config_value(path: &Path) -> Option<PathBuf> {
    let value = fs::read_to_string(path).ok()?;
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

fn hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| env::var("HOSTNAME").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "linux".to_string())
}

/// Finds the Heroic config dir for Flatpak, native/AppImage or Snap installs.
fn detect_heroic(home: &Path) -> PathBuf {
    let candidates = [
        home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic"),
        home.join(".config/heroic"),
        home.join("snap/heroic/current/.config/heroic"),
    ];
    candidates
        .iter()
        .find(|path| path.is_dir())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

fn print_help() {
    println!(
        "heroic-save-backup {}\n\
         Back up game saves managed by Heroic Launcher (and extra Wine prefixes) to Git.\n\n\
         USAGE:\n    heroic-save-backup [OPTIONS]\n\n\
         OPTIONS:\n\
         \x20   --repo <PATH>        Git repository that stores the saves (default: ~/heroic-save-backup)\n\
         \x20   --heroic-dir <PATH>  Heroic config dir (auto-detected: Flatpak, native or Snap)\n\
         \x20   --machine <NAME>     machine folder inside the repo (default: hostname)\n\
         \x20   --dry-run            show what would be copied; no writes, commits or pushes\n\
         \x20   --no-push            commit locally but do not push to the remote\n\
         \x20   --quiet              do not show desktop notifications\n\
         \x20   -h, --help           show this help\n\
         \x20   -V, --version        show the version\n\n\
         ENVIRONMENT:\n\
         \x20   GAME_SAVES_REPO      repository path (overridden by --repo)\n\
         \x20   GAME_SAVES_MACHINE   machine folder name (overridden by --machine)\n\
         \x20   HEROIC_DIR           Heroic config dir (overridden by --heroic-dir)\n\
         \x20   SECONDARY_ROOTS      colon-separated dirs with extra Wine prefixes (default: ~/Games)\n\
         \x20   GAME_SAVES_STATE     state/log dir (default: ~/.local/state/heroic-save-backup)\n\n\
         EXIT CODES: 0 ok, 1 setup/runtime error, 2 commit/push failed",
        env!("CARGO_PKG_VERSION")
    );
}

// ---------------------------------------------------------------------- log

struct Log<'a> {
    env: &'a Env,
    file: Option<File>,
}

impl<'a> Log<'a> {
    fn new(env: &'a Env) -> Self {
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&env.log_file)
            .ok();
        Self { env, file }
    }

    fn info(&mut self, msg: &str) {
        let line = format!("[{}] {msg}", Local::now().format("%Y-%m-%d %H:%M:%S"));
        println!("{line}");
        if let Some(file) = self.file.as_mut() {
            let _ = writeln!(file, "{line}");
        }
    }

    fn notify(&self, title: &str, body: &str) {
        if self.env.quiet {
            return;
        }
        if env::var_os("DISPLAY").is_none() && env::var_os("WAYLAND_DISPLAY").is_none() {
            return;
        }
        let _ = Command::new("notify-send")
            .args(["-a", "Game Saves", "-i", "document-save", title, body])
            .status();
    }
}

// ------------------------------------------------------------------- util

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn is_combining(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F
    )
}

fn norm(text: &str) -> String {
    text.nfkd()
        .filter(|c| !is_combining(*c) && c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' => '-',
            ':' => '-',
            c => c,
        })
        .collect();
    let trimmed = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == '.' || c == ' ')
        .to_string();
    if trimmed.is_empty() {
        "Unnamed".to_string()
    } else {
        trimmed
    }
}

fn title_keys(title: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let full = norm(title);
    if full.len() >= 4 {
        keys.insert(full);
    }
    let reduced: String = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !STOPWORDS.contains(&w.to_lowercase().as_str()))
        .collect();
    let reduced = norm(&reduced);
    if reduced.len() >= 4 {
        keys.insert(reduced);
    }
    keys
}

fn matches(folder: &str, titles: &[String]) -> bool {
    let nf = norm(folder);
    if nf.len() < 4 {
        return false;
    }
    titles
        .iter()
        .flat_map(|t| title_keys(t))
        .any(|key| nf.contains(&key) || key.contains(&nf))
}

fn is_excluded(name: &str) -> bool {
    let lower = name.to_lowercase();
    if EXCLUDED.contains(&lower.as_str()) {
        return true;
    }
    EXCLUDED_SUFFIXES.iter().any(|s| lower.ends_with(s))
}

fn expand_tilde(path: &str, home: &Path) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else if path == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(path)
    }
}

fn is_dir_not_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false)
}

fn dir_has_entries(path: &Path) -> bool {
    fs::read_dir(path)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

fn is_prefix(path: &Path) -> bool {
    path.join("drive_c").is_dir() || path.join("pfx/drive_c").is_dir()
}

fn resolve_prefix(path: &Path) -> PathBuf {
    let pfx = path.join("pfx");
    if pfx.join("drive_c").is_dir() {
        pfx
    } else {
        path.to_path_buf()
    }
}

// ------------------------------------------------------------------- Heroic

#[derive(Default, Clone)]
struct GameInfo {
    title: String,
    platform: String,
    executable: String,
}

impl GameInfo {
    fn is_native(&self) -> bool {
        let platform = self.platform.to_lowercase();
        let exe = self.executable.to_lowercase();
        if matches!(platform.as_str(), "linux" | "mac" | "osx") {
            return true;
        }
        if matches!(platform.as_str(), "windows" | "win32" | "win64") {
            return false;
        }
        !exe.is_empty()
            && !exe.ends_with(".exe")
            && !exe.ends_with(".bat")
            && !exe.ends_with(".msi")
            && !exe.ends_with(".lnk")
    }
}

fn iter_game_entries(value: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(obj) = value.as_object() else {
        if let Some(arr) = value.as_array() {
            out.extend(arr.iter().filter(|v| v.is_object()).cloned());
        }
        return out;
    };
    for key in ["games", "installed", "library"] {
        if let Some(arr) = obj.get(key).and_then(Value::as_array) {
            out.extend(arr.iter().filter(|v| v.is_object()).cloned());
        }
    }
    for (key, val) in obj {
        let Some(inner) = val.as_object() else {
            continue;
        };
        if inner.contains_key("title") || inner.contains_key("app_name") {
            let mut entry = inner.clone();
            entry
                .entry("app_name")
                .or_insert_with(|| Value::String(key.clone()));
            out.push(Value::Object(entry));
        }
    }
    out
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn load_library(heroic: &Path) -> HashMap<String, GameInfo> {
    let candidates = [
        heroic.join("legendaryConfig/legendary/installed.json"),
        heroic.join("gog_store/installed.json"),
        heroic.join("nile_store/installed.json"),
        heroic.join("nile_store/library.json"),
        // cache used by Heroic while nile_store/library.json does not exist
        heroic.join("store_cache/nile_library.json"),
        heroic.join("sideload_apps/library.json"),
    ];
    let mut games = HashMap::new();
    for path in candidates {
        let Some(data) = read_json(&path) else {
            continue;
        };
        for entry in iter_game_entries(&data) {
            let app = {
                let a = string_field(&entry, "app_name");
                if a.is_empty() {
                    string_field(&entry, "id")
                } else {
                    a
                }
            };
            let title = {
                let t = string_field(&entry, "title");
                if t.is_empty() {
                    string_field(&entry, "name")
                } else {
                    t
                }
            };
            if app.is_empty() || title.is_empty() {
                continue;
            }
            games.insert(
                app,
                GameInfo {
                    title,
                    platform: string_field(&entry, "platform"),
                    executable: string_field(&entry, "executable"),
                },
            );
        }
    }
    games
}

#[derive(Default)]
struct PrefixInfo {
    titles: BTreeSet<String>,
    exists: bool,
}

fn default_prefix_root(heroic: &Path) -> Option<PathBuf> {
    let config = read_json(&heroic.join("config.json"))?;
    let root = config
        .get("defaultSettings")?
        .get("defaultWinePrefix")?
        .as_str()?;
    PathBuf::from(root).canonicalize().ok()
}

type HeroicGames = (
    BTreeMap<PathBuf, PrefixInfo>,
    Vec<(String, PathBuf)>,
    BTreeMap<String, GameInfo>,
);

fn heroic_games(env: &Env) -> HeroicGames {
    let library = load_library(&env.heroic);
    let config = read_json(&env.heroic.join("config.json")).unwrap_or(Value::Null);
    let default_prefix = config
        .get("defaultSettings")
        .and_then(|s| s.get("defaultWinePrefix"))
        .and_then(Value::as_str)
        .map(PathBuf::from);

    let mut prefixes: BTreeMap<PathBuf, PrefixInfo> = BTreeMap::new();
    let mut overrides: Vec<(String, PathBuf)> = Vec::new();
    let mut natives: BTreeMap<String, GameInfo> = BTreeMap::new();

    let configs_dir = env.heroic.join("GamesConfig");
    let Ok(read_dir) = fs::read_dir(&configs_dir) else {
        return (prefixes, overrides, natives);
    };
    let mut cfg_paths: Vec<PathBuf> = read_dir
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension() == Some(OsStr::new("json")))
        .collect();
    cfg_paths.sort();

    for path in cfg_paths {
        let Some(data) = read_json(&path) else {
            continue;
        };
        let Some(obj) = data.as_object() else {
            continue;
        };
        for (app_name, cfg) in obj {
            if !cfg.is_object() {
                continue;
            }
            let Some(info) = library.get(app_name) else {
                // game not installed / unknown
                continue;
            };
            if info.is_native() {
                natives.insert(info.title.clone(), info.clone());
            } else {
                let prefix = cfg
                    .get("winePrefix")
                    .and_then(Value::as_str)
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
                    .or_else(|| {
                        default_prefix
                            .as_ref()
                            .map(|p| p.join(safe_name(&info.title)))
                    });
                if let Some(prefix) = prefix {
                    let entry = prefixes.entry(prefix).or_default();
                    entry.titles.insert(info.title.clone());
                    entry.exists = false;
                }
            }
            if let Some(save_folder) = cfg
                .get("saveFolder")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
            {
                overrides.push((info.title.clone(), expand_tilde(save_folder, &env.home)));
            }
        }
    }

    for (prefix, entry) in prefixes.iter_mut() {
        entry.exists = resolve_prefix(prefix).is_dir();
    }

    (prefixes, overrides, natives)
}

fn secondary_prefixes(
    env: &Env,
    known: &BTreeMap<PathBuf, PrefixInfo>,
) -> BTreeMap<PathBuf, PrefixInfo> {
    let mut result: BTreeMap<PathBuf, PrefixInfo> = BTreeMap::new();
    let known_resolved: HashSet<PathBuf> = known
        .keys()
        .filter_map(|p| resolve_prefix(p).canonicalize().ok())
        .collect();
    let heroic_root = default_prefix_root(&env.heroic);
    let mut seen: HashSet<PathBuf> = known_resolved;

    for root in &env.secondary_roots {
        if !root.is_dir() {
            continue;
        }
        let mut stack = vec![(root.clone(), 0usize)];
        while let Some((current, depth)) = stack.pop() {
            let Ok(read_dir) = fs::read_dir(&current) else {
                continue;
            };
            for entry in read_dir.filter_map(|e| e.ok()) {
                let child = entry.path();
                if !is_dir_not_symlink(&child) {
                    continue;
                }
                if is_prefix(&child) {
                    let resolved = resolve_prefix(&child)
                        .canonicalize()
                        .unwrap_or_else(|_| resolve_prefix(&child));
                    if let Some(root) = &heroic_root
                        && (resolved == *root || resolved.starts_with(root))
                    {
                        continue;
                    }
                    if !seen.insert(resolved.clone()) {
                        continue;
                    }
                    let title = child
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Unnamed".to_string());
                    result.insert(
                        child,
                        PrefixInfo {
                            titles: BTreeSet::from([title]),
                            exists: true,
                        },
                    );
                } else if depth < 2 {
                    stack.push((child, depth + 1));
                }
            }
        }
    }
    result
}

// ------------------------------------------------------------------ discovery

fn user_dirs(prefix: &Path) -> Vec<PathBuf> {
    let users = prefix.join("drive_c/users");
    let Ok(read_dir) = fs::read_dir(&users) else {
        return Vec::new();
    };
    read_dir
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_dir_not_symlink(p))
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_lowercase() != "public")
                .unwrap_or(false)
        })
        .collect()
}

fn collect_sources(prefix: &Path, titles: &[String]) -> Vec<(PathBuf, String)> {
    let prefix = resolve_prefix(prefix);
    let mut sources = Vec::new();
    for user in user_dirs(&prefix) {
        let mut add = |rel: &str| {
            let src = user.join(rel);
            if src.is_dir() && dir_has_entries(&src) {
                sources.push((src, rel.to_string()));
            }
        };
        add("Saved Games");
        add("Documents/My Games");
        add("AppData/LocalLow");

        for base in ["Documents", "AppData/Local", "AppData/Roaming"] {
            let base_dir = user.join(base);
            if !base_dir.is_dir() {
                continue;
            }
            let Ok(read_dir) = fs::read_dir(&base_dir) else {
                continue;
            };
            let mut subdirs: Vec<PathBuf> = read_dir
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| is_dir_not_symlink(p))
                .collect();
            subdirs.sort();
            for sub in subdirs {
                let Some(name) = sub.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                    continue;
                };
                if matches!(
                    name.to_lowercase().as_str(),
                    "my games" | "temp" | "microsoft" | "packages"
                ) {
                    continue;
                }
                if matches(&name, titles) {
                    sources.push((sub, format!("{base}/{name}")));
                }
            }
        }
    }
    sources
}

fn native_roots(home: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("local-share", home.join(".local/share")),
        ("config", home.join(".config")),
        ("documents", home.join("Documents")),
        ("gog", home.join(".local/share/GOG.com")),
    ]
}

fn collect_native_sources(home: &Path, title: &str) -> Vec<(PathBuf, String)> {
    let titles = [title.to_string()];
    let mut sources = Vec::new();
    for (label, base) in native_roots(home) {
        if !base.is_dir() {
            continue;
        }
        let Ok(read_dir) = fs::read_dir(&base) else {
            continue;
        };
        let mut children: Vec<PathBuf> = read_dir
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| is_dir_not_symlink(p))
            .collect();
        children.sort();
        for child in children {
            let Some(name) = child.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if matches(&name, &titles) {
                sources.push((child, format!("_native/{label}/{name}")));
            }
        }
    }
    sources
}

fn manual_sources(repo: &Path, home: &Path) -> Vec<(String, PathBuf)> {
    let Ok(text) = fs::read_to_string(repo.join("extra_paths.tsv")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|raw| {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (title, path) = line.split_once('\t')?;
            let src = expand_tilde(path.trim(), home);
            if title.trim().is_empty() || !src.is_dir() {
                return None;
            }
            Some((title.trim().to_string(), src))
        })
        .collect()
}

// --------------------------------------------------------------------- copy

fn copy_tree(src: &Path, dest: &Path, dry_run: bool, copied: &mut u64) -> io::Result<()> {
    if !dry_run {
        fs::create_dir_all(dest)?;
    }
    let mut entries: Vec<_> = fs::read_dir(src)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_excluded(&name) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let target = dest.join(&name);
        if metadata.is_dir() {
            copy_tree(&path, &target, dry_run, copied)?;
            continue;
        }
        if !metadata.is_file() || metadata.len() > MAX_FILE_SIZE {
            continue;
        }

        let same = fs::metadata(&target)
            .map(|d| d.len() == metadata.len() && d.modified().ok() == metadata.modified().ok())
            .unwrap_or(false);
        if same {
            continue;
        }
        *copied += 1;
        if dry_run {
            continue;
        }
        fs::copy(&path, &target)?;
        if let Ok(modified) = metadata.modified() {
            let time = FileTime::from_system_time(modified);
            let _ = set_file_times(&target, time, time);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------- git

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git: {e}"))
}

fn git_with_env(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap_or_else(|e| panic!("failed to run git: {e}"))
}

fn git_backup(env: &Env, log: &mut Log) -> bool {
    if env.dry_run {
        return true;
    }
    if !env.repo.join(".git").is_dir() {
        log.info(&format!(
            "ERROR: {} is not a git repository. Run install.sh first.",
            env.repo.display()
        ));
        return false;
    }

    git(&env.repo, &["add", "-A"]);
    let diff = git(&env.repo, &["diff", "--cached", "--quiet"]);
    if diff.status.success() {
        log.info("No save changes to commit.");
        return true;
    }

    let stamp = Local::now().format("%Y-%m-%d %H:%M");
    let message = format!("Automatic save backup [{}] — {stamp}", env.machine);
    let commit = git(&env.repo, &["commit", "-m", &message]);
    if !commit.status.success() {
        log.info(&format!(
            "ERROR: commit failed: {}",
            String::from_utf8_lossy(&commit.stderr).trim()
        ));
        return false;
    }
    log.info("Commit created.");

    if env.no_push {
        log.info("--no-push: skipping push.");
        return true;
    }

    // Another machine may have pushed in the meantime: rebase before pushing.
    let upstream = git(
        &env.repo,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    );
    if upstream.status.success() {
        let pull = git_with_env(&env.repo, &["pull", "--rebase", "--autostash"]);
        if pull.status.success() {
            log.info("Rebased onto the remote.");
        } else {
            let _ = git(&env.repo, &["rebase", "--abort"]);
            log.info(&format!(
                "WARNING: rebase failed, keeping local commit: {}",
                String::from_utf8_lossy(&pull.stderr).trim()
            ));
        }
    }

    let push = git_with_env(&env.repo, &["push", "origin", "HEAD"]);
    if !push.status.success() {
        log.info(&format!(
            "WARNING: push failed (commit kept locally): {}",
            String::from_utf8_lossy(&push.stderr).trim()
        ));
        log.notify(
            "Save backup",
            "Commit created locally, but the push to GitHub failed.",
        );
        return false;
    }
    log.info("Pushed to GitHub.");
    true
}

// --------------------------------------------------------------------- main

fn main() {
    let env = Env::load();
    if let Err(e) = fs::create_dir_all(&env.state_dir) {
        eprintln!("could not create {}: {e}", env.state_dir.display());
        std::process::exit(1);
    }

    let lock = match File::create(&env.lock_file) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("could not open the lock file: {e}");
            std::process::exit(1);
        }
    };
    if lock.try_lock_exclusive().is_err() {
        println!("Another backup is already running; exiting.");
        return;
    }

    let mut log = Log::new(&env);
    log.info(&format!(
        "=== Starting save backup (machine: {}) ===",
        env.machine
    ));

    let (mut prefixes, overrides, natives) = heroic_games(&env);
    prefixes.extend(secondary_prefixes(&env, &prefixes));

    let saves_root = env.repo.join("saves").join(&env.machine);
    let mut total_files = 0u64;
    let mut games_done = 0usize;
    let mut index_lines: Vec<String> = Vec::new();
    let mut manuals = manual_sources(&saves_root, &env.home);
    if manuals.is_empty() {
        manuals = manual_sources(&env.repo, &env.home);
    }

    for (prefix, info) in &prefixes {
        let titles: Vec<String> = info.titles.iter().cloned().collect();
        let game_name = if titles.len() == 1 {
            titles[0].clone()
        } else {
            let base = resolve_prefix(prefix);
            let name = base
                .parent()
                .and_then(|p| p.file_name())
                .or_else(|| prefix.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Unnamed".to_string());
            safe_name(&name)
        };
        let game_dir = saves_root.join(safe_name(&game_name));

        if !info.exists {
            log.info(&format!(
                "- {game_name}: prefix does not exist yet, skipping ({})",
                prefix.display()
            ));
            continue;
        }

        let mut sources = collect_sources(prefix, &titles);
        for (title, folder) in &overrides {
            if titles.contains(title) && folder.is_dir() {
                let name = folder.file_name().map(|n| n.to_string_lossy().into_owned());
                if let Some(name) = name {
                    sources.push((folder.clone(), format!("_saveFolder/{name}")));
                }
            }
        }
        for (title, folder) in &manuals {
            if *title == game_name || titles.contains(title) {
                let name = folder.file_name().map(|n| n.to_string_lossy().into_owned());
                if let Some(name) = name {
                    sources.push((folder.clone(), format!("_extra/{name}")));
                }
            }
        }

        if sources.is_empty() {
            log.info(&format!(
                "- {game_name}: no save folder found in {}",
                prefix.display()
            ));
            continue;
        }

        let mut seen: HashSet<PathBuf> = HashSet::new();
        for (src, rel) in sources {
            let src = src.canonicalize().unwrap_or(src);
            if !seen.insert(src.clone()) {
                continue;
            }
            let mut copied = 0u64;
            if let Err(e) = copy_tree(&src, &game_dir.join(&rel), env.dry_run, &mut copied) {
                log.info(&format!("ERROR in {game_name}: {e}"));
                continue;
            }
            total_files += copied;
            log.info(&format!("- {game_name}: {rel} ({copied} file(s))"));
        }

        games_done += 1;
        index_lines.push(format!(
            "{}\t{}\t{}",
            safe_name(&game_name),
            prefix.display(),
            titles.join(", ")
        ));
    }

    // Native Linux games: no prefix; saves live in ~/.local/share etc.
    for title in natives.keys() {
        let game_dir = saves_root.join(safe_name(title));
        let mut sources = collect_native_sources(&env.home, title);
        for (over_title, folder) in &overrides {
            if over_title == title && folder.is_dir() {
                let name = folder.file_name().map(|n| n.to_string_lossy().into_owned());
                if let Some(name) = name {
                    sources.push((folder.clone(), format!("_saveFolder/{name}")));
                }
            }
        }
        for (man_title, folder) in &manuals {
            if man_title == title {
                let name = folder.file_name().map(|n| n.to_string_lossy().into_owned());
                if let Some(name) = name {
                    sources.push((folder.clone(), format!("_extra/{name}")));
                }
            }
        }
        if sources.is_empty() {
            log.info(&format!(
                "- {title}: native Linux game, no save folder found"
            ));
            continue;
        }

        let mut seen: HashSet<PathBuf> = HashSet::new();
        for (src, rel) in sources {
            let src = src.canonicalize().unwrap_or(src);
            if !seen.insert(src.clone()) {
                continue;
            }
            let mut copied = 0u64;
            if let Err(e) = copy_tree(&src, &game_dir.join(&rel), env.dry_run, &mut copied) {
                log.info(&format!("ERROR in {title}: {e}"));
                continue;
            }
            total_files += copied;
            log.info(&format!("- {title} (native): {rel} ({copied} file(s))"));
        }

        games_done += 1;
        index_lines.push(format!("{}\tnative-linux\t{title}", safe_name(title)));
    }

    let index = format!(
        "# Generated automatically by heroic-save-backup\n# folder\tsource\tgame(s)\n{}\n",
        index_lines.join("\n")
    );
    if !env.dry_run {
        let _ = fs::create_dir_all(&saves_root);
        let _ = fs::write(saves_root.join(".index.tsv"), index);
    }

    log.info(&format!(
        "Games processed: {games_done} | files created/updated: {total_files}"
    ));

    let ok = git_backup(&env, &mut log);
    if ok {
        log.notify(
            "Backup de saves",
            &format!("{games_done} game(s), {total_files} file(s) updated."),
        );
    } else {
        log.notify("Backup de saves", "Backup failed — check the log.");
    }
    log.info("=== Backup finished ===");
    if !ok {
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(titles: &[&str]) -> Vec<String> {
        titles.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn normalization_ignores_accents_and_punctuation() {
        assert_eq!(norm("Ação! 2"), "acao2");
        assert_eq!(norm("A Plague Tale: Innocence"), "aplaguetaleinnocence");
    }

    #[test]
    fn safe_names_remove_path_separators() {
        let name = safe_name("A/B: C");
        assert!(!name.contains('/'));
        assert!(!name.contains(':'));
        assert_eq!(safe_name("   "), "Unnamed");
    }

    #[test]
    fn matches_game_folders() {
        let titles = g(&["A Plague Tale: Innocence"]);
        assert!(matches("APlagueTaleInnocence", &titles));
        assert!(matches("A Plague Tale Innocence", &titles));
        assert!(!matches("Microsoft", &titles));
        assert!(!matches("abc", &titles));
    }

    #[test]
    fn matches_titles_with_edition_suffix() {
        let titles = g(&["Disco Elysium - The Final Cut"]);
        assert!(matches("Disco Elysium", &titles));
        assert!(matches("DiscoElysium", &titles));
    }

    #[test]
    fn excludes_caches_logs_and_temp_files() {
        for name in [
            "Cache",
            "Code Cache",
            "webcache",
            "Temp",
            "game.log",
            "x.tmp",
            "y.lock",
        ] {
            assert!(is_excluded(name), "{name} should be excluded");
        }
        for name in ["Saved Games", "profile.sav", "settings.json"] {
            assert!(!is_excluded(name), "{name} should be kept");
        }
    }

    #[test]
    fn detects_native_games() {
        let linux = GameInfo {
            platform: "Linux".into(),
            executable: "game".into(),
            ..Default::default()
        };
        let windows = GameInfo {
            platform: "Windows".into(),
            executable: "game.exe".into(),
            ..Default::default()
        };
        let unknown_linux = GameInfo {
            executable: "bin/game".into(),
            ..Default::default()
        };
        assert!(linux.is_native());
        assert!(!windows.is_native());
        assert!(unknown_linux.is_native());
    }

    #[test]
    fn expands_tilde() {
        let home = Path::new("/home/player");
        assert_eq!(expand_tilde("~/Games", home), home.join("Games"));
        assert_eq!(
            expand_tilde("/opt/games", home),
            PathBuf::from("/opt/games")
        );
    }
}
