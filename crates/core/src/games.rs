//! Game detection data: the bundled executable to game name database, the
//! user's per game profiles, and the matching of running processes against
//! both.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{CaptureScope, GameAction, GameProfile, GamesConfig};

/// The bundled database, generated from the public seed by
/// `cargo run -p openclips-core --example build_games_db`.
pub const BUNDLED_GAMES_JSON: &str = include_str!("../assets/games.json");
pub const GAMES_DB_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameEntry {
    /// Lower case executable file name, for example `hl2.exe`.
    pub exe: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GamesFile {
    pub version: u32,
    pub source: String,
    pub games: Vec<GameEntry>,
}

/// Executable name to game name, lower case keys.
#[derive(Debug, Clone, Default)]
pub struct GamesDatabase {
    by_exe: HashMap<String, String>,
}

impl GamesDatabase {
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        let file: GamesFile = serde_json::from_str(text)?;
        Ok(Self::from_entries(file.games))
    }

    pub fn from_entries(entries: Vec<GameEntry>) -> Self {
        Self {
            by_exe: entries
                .into_iter()
                .map(|e| (e.exe.to_lowercase(), e.name))
                .collect(),
        }
    }

    pub fn bundled() -> Self {
        Self::from_json(BUNDLED_GAMES_JSON).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.by_exe.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_exe.is_empty()
    }

    pub fn lookup(&self, exe: &str) -> Option<&str> {
        self.by_exe.get(&exe.to_lowercase()).map(String::as_str)
    }
}

/// A process reported by the platform watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningProcess {
    pub pid: u32,
    /// Lower case executable file name.
    pub exe: String,
    pub path: Option<PathBuf>,
    pub foreground: bool,
}

/// A running process that is a known game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedGame {
    pub pid: u32,
    pub exe: String,
    pub name: String,
    pub path: Option<PathBuf>,
    pub foreground: bool,
    pub profile: Option<GameProfile>,
}

impl DetectedGame {
    pub fn action(&self) -> GameAction {
        self.profile
            .as_ref()
            .map(|p| p.action)
            .unwrap_or(GameAction::Buffer)
    }
}

/// Matches running processes against the user's profiles first, then the
/// bundled database. One entry per executable name.
pub fn detect(
    processes: &[RunningProcess],
    db: &GamesDatabase,
    config: &GamesConfig,
) -> Vec<DetectedGame> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for process in processes {
        let exe = process.exe.to_lowercase();
        if seen.contains(&exe) {
            continue;
        }
        let profile = config
            .profiles
            .iter()
            .find(|p| p.exe.eq_ignore_ascii_case(&exe));
        let name = match profile {
            Some(profile) if !profile.name.trim().is_empty() => profile.name.trim().to_owned(),
            Some(_) => db
                .lookup(&exe)
                .map(str::to_owned)
                .unwrap_or_else(|| display_name_from_exe(&exe)),
            None => match db.lookup(&exe) {
                Some(name) => name.to_owned(),
                None => continue,
            },
        };
        seen.insert(exe.clone());
        found.push(DetectedGame {
            pid: process.pid,
            exe,
            name,
            path: process.path.clone(),
            foreground: process.foreground,
            profile: profile.cloned(),
        });
    }
    found
}

/// The game that should drive capture: the foreground one when it is a
/// game, otherwise the first detected one that is not ignored.
pub fn active(detected: &[DetectedGame]) -> Option<&DetectedGame> {
    let candidates = || detected.iter().filter(|g| g.action() != GameAction::Ignore);
    candidates()
        .find(|g| g.foreground)
        .or_else(|| candidates().next())
}

/// What the watcher wants capture to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoCapture {
    /// Leave capture to the user (global scope, or no game running).
    None,
    Buffer,
    Recording,
}

pub fn auto_capture(scope: CaptureScope, active: Option<&DetectedGame>) -> AutoCapture {
    match (scope, active) {
        (CaptureScope::Global, _) | (_, None) => AutoCapture::None,
        (CaptureScope::PerGame, Some(game)) => match game.action() {
            GameAction::Buffer => AutoCapture::Buffer,
            GameAction::Recording => AutoCapture::Recording,
            GameAction::Ignore => AutoCapture::None,
        },
    }
}

/// `some_game-win64-shipping.exe` becomes `Some Game`.
pub fn display_name_from_exe(exe: &str) -> String {
    let stem = exe.rsplit_once('.').map(|(s, _)| s).unwrap_or(exe);
    let stem = stem
        .to_lowercase()
        .replace("-win64-shipping", "")
        .replace("-win32-shipping", "")
        .replace("_dx12", "")
        .replace("_dx11", "");
    let words: Vec<String> = stem
        .split(['_', '-', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        exe.to_owned()
    } else {
        words.join(" ")
    }
}

/// Cleaning of the public seed list into the bundled database.
pub mod seed {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct RawEntry {
        #[serde(rename = "processName")]
        process_name: Option<String>,
        #[serde(rename = "Name")]
        name: Option<String>,
    }

    /// Executables that are not games: launchers, installers, runtimes.
    const GENERIC_EXACT: &[&str] = &[
        "launcher.exe",
        "setup.exe",
        "install.exe",
        "installer.exe",
        "uninstall.exe",
        "javaw.exe",
        "java.exe",
        "dxsetup.exe",
        "game.exe",
        "start.exe",
        "play.exe",
        "run.exe",
        "client.exe",
        "steam.exe",
        "steamservice.exe",
        "uplay.exe",
        "upc.exe",
        "origin.exe",
        "epicgameslauncher.exe",
        "galaxyclient.exe",
        "bootstrapper.exe",
        "updater.exe",
        "update.exe",
        "config.exe",
        "settings.exe",
        "readme.exe",
        "eac.exe",
        "easyanticheat.exe",
        "battleye.exe",
        "beservice.exe",
        "crashreporter.exe",
        "crashreportclient.exe",
        "crashsender.exe",
        "wow64.exe",
        "cmd.exe",
        "explorer.exe",
    ];

    const GENERIC_PREFIXES: &[&str] = &[
        "vcredist",
        "vc_redist",
        "dotnet",
        "directx",
        "dxwebsetup",
        "unins",
        "setup_",
        "setup-",
        "install_",
        "redist",
        "physx",
        "oalinst",
        "ue4prereqsetup",
        "ue5prereqsetup",
        "ueprereqsetup",
        "crashreport",
        "unitycrashhandler",
    ];

    const GENERIC_CONTAINS: &[&str] = &[
        "launcher",
        "redist",
        "uninstall",
        "crashhandler",
        "anticheat",
    ];

    /// Binaries shared by several games, where the shortest name rule would
    /// pick a surprising title.
    const PREFERRED: &[(&str, &str)] = &[("hl2.exe", "Half-Life 2"), ("hl.exe", "Half-Life")];

    pub fn is_generic_executable(exe: &str) -> bool {
        let exe = exe.to_lowercase();
        GENERIC_EXACT.contains(&exe.as_str())
            || GENERIC_PREFIXES.iter().any(|p| exe.starts_with(p))
            || GENERIC_CONTAINS.iter().any(|c| exe.contains(c))
    }

    fn normalize_name(name: &str) -> String {
        name.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Parses the raw gist, drops malformed rows and generic executables,
    /// normalizes names, and resolves duplicates by keeping the shortest
    /// name (the base game rather than an expansion sharing the binary).
    pub fn clean(raw: &str) -> Result<Vec<GameEntry>, serde_json::Error> {
        let rows: Vec<RawEntry> = serde_json::from_str(raw)?;
        let mut best: BTreeMap<String, String> = BTreeMap::new();
        for row in rows {
            let (Some(exe), Some(name)) = (row.process_name, row.name) else {
                continue;
            };
            let exe = exe.trim().to_lowercase();
            let name = normalize_name(&name);
            if !exe.ends_with(".exe")
                || exe.len() < 5
                || name.is_empty()
                || is_generic_executable(&exe)
            {
                continue;
            }
            if exe.contains(['/', '\\']) {
                continue;
            }
            match best.get(&exe) {
                Some(existing) if existing.len() <= name.len() => {}
                _ => {
                    best.insert(exe, name);
                }
            }
        }
        for (exe, name) in PREFERRED {
            if best.contains_key(*exe) {
                best.insert((*exe).to_owned(), (*name).to_owned());
            }
        }
        Ok(best
            .into_iter()
            .map(|(exe, name)| GameEntry { exe, name })
            .collect())
    }

    pub fn to_json(entries: Vec<GameEntry>, source: &str) -> Result<String, serde_json::Error> {
        let file = GamesFile {
            version: GAMES_DB_VERSION,
            source: source.to_owned(),
            games: entries,
        };
        serde_json::to_string_pretty(&file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DisplaySelection;

    fn process(pid: u32, exe: &str, foreground: bool) -> RunningProcess {
        RunningProcess {
            pid,
            exe: exe.to_owned(),
            path: None,
            foreground,
        }
    }

    fn db() -> GamesDatabase {
        GamesDatabase::from_entries(vec![
            GameEntry {
                exe: "hl2.exe".to_owned(),
                name: "Half-Life 2".to_owned(),
            },
            GameEntry {
                exe: "cs2.exe".to_owned(),
                name: "Counter-Strike 2".to_owned(),
            },
        ])
    }

    #[test]
    fn bundled_database_loads() {
        let db = GamesDatabase::bundled();
        assert!(db.len() > 1000, "bundled database has {} entries", db.len());
        assert_eq!(db.lookup("HL2.EXE"), Some("Half-Life 2"));
        assert!(db.lookup("launcher.exe").is_none());
        assert!(db.lookup("vcredist_x86.exe").is_none());
    }

    #[test]
    fn seed_cleaning_drops_noise_and_resolves_duplicates() {
        let raw = r#"[
            {"processName": "HL2.exe", "Name": "Half-Life 2: Episode Two"},
            {"processName": "hl2.exe", "Name": "  Half-Life   2 "},
            {"processName": "launcher.exe", "Name": "Some Game"},
            {"processName": "vcredist_x86.exe", "Name": "Other"},
            {"processName": "game", "Name": "No extension"},
            {"Name": "Cabal 2"},
            {"processName": "path\\to\\thing.exe", "Name": "Pathy"},
            {"processName": "fine.exe", "Name": "Fine Game"}
        ]"#;
        let entries = seed::clean(raw).expect("parse");
        assert_eq!(
            entries,
            vec![
                GameEntry {
                    exe: "fine.exe".to_owned(),
                    name: "Fine Game".to_owned()
                },
                GameEntry {
                    exe: "hl2.exe".to_owned(),
                    name: "Half-Life 2".to_owned()
                },
            ]
        );
        assert!(seed::is_generic_executable("UnityCrashHandler64.exe"));
        assert!(seed::is_generic_executable("MyGameLauncher.exe"));
        assert!(!seed::is_generic_executable("hatintimegame.exe"));
    }

    #[test]
    fn detects_known_games_and_profiles() {
        let config = GamesConfig {
            scope: CaptureScope::PerGame,
            profiles: vec![GameProfile {
                exe: "mygame-win64-shipping.exe".to_owned(),
                name: String::new(),
                action: GameAction::Recording,
                replay_length_seconds: Some(120),
                subfolder: Some("MyGame".to_owned()),
                display: Some(DisplaySelection::Primary),
            }],
        };
        let processes = [
            process(1, "explorer.exe", false),
            process(2, "hl2.exe", false),
            process(3, "MyGame-Win64-Shipping.exe", true),
            process(4, "hl2.exe", false),
        ];
        let detected = detect(&processes, &db(), &config);
        assert_eq!(detected.len(), 2);
        assert_eq!(detected[0].name, "Half-Life 2");
        assert_eq!(detected[1].name, "Mygame");
        assert!(detected[1].profile.is_some());

        let active = active(&detected).expect("active");
        assert_eq!(active.exe, "mygame-win64-shipping.exe");
        assert_eq!(
            auto_capture(config.scope, Some(active)),
            AutoCapture::Recording
        );
        assert_eq!(
            auto_capture(CaptureScope::Global, Some(active)),
            AutoCapture::None
        );
        assert_eq!(auto_capture(CaptureScope::PerGame, None), AutoCapture::None);
    }

    #[test]
    fn ignored_games_do_not_drive_capture() {
        let config = GamesConfig {
            scope: CaptureScope::PerGame,
            profiles: vec![GameProfile {
                exe: "hl2.exe".to_owned(),
                name: "Half-Life 2".to_owned(),
                action: GameAction::Ignore,
                replay_length_seconds: None,
                subfolder: None,
                display: None,
            }],
        };
        let detected = detect(&[process(2, "hl2.exe", true)], &db(), &config);
        assert_eq!(detected.len(), 1);
        assert!(active(&detected).is_none());
    }

    #[test]
    fn names_from_executables_read_well() {
        assert_eq!(display_name_from_exe("rocketleague.exe"), "Rocketleague");
        assert_eq!(
            display_name_from_exe("dead_space-win64-shipping.exe"),
            "Dead Space"
        );
        assert_eq!(display_name_from_exe("weird"), "Weird");
    }
}
