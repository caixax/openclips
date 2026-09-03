//! Game detection service: polls running processes, matches them against
//! the bundled database and the user's profiles, and caches game icons
//! extracted from the executables.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use openclips_capture::{IconExtractor, ProcessWatcher};
use openclips_core::clip::sanitize_file_name;
use openclips_core::config::{AppPaths, GamesConfig};
use openclips_core::games::{DetectedGame, GamesDatabase, detect};
use tracing::{debug, info, warn};

pub struct GameService {
    db: GamesDatabase,
    watcher: Arc<dyn ProcessWatcher>,
    icons: Arc<dyn IconExtractor>,
    icons_dir: PathBuf,
    detected: Vec<DetectedGame>,
    /// Executable name to icon path, `None` when extraction failed once.
    icon_cache: HashMap<String, Option<PathBuf>>,
}

impl GameService {
    pub fn new(
        paths: &AppPaths,
        watcher: Arc<dyn ProcessWatcher>,
        icons: Arc<dyn IconExtractor>,
    ) -> Self {
        let db = GamesDatabase::bundled();
        info!("games database: {} executables", db.len());
        Self {
            db,
            watcher,
            icons,
            icons_dir: paths.cache_dir.join("icons"),
            detected: Vec::new(),
            icon_cache: HashMap::new(),
        }
    }

    pub fn database(&self) -> &GamesDatabase {
        &self.db
    }

    pub fn detected(&self) -> &[DetectedGame] {
        &self.detected
    }

    pub fn active(&self) -> Option<&DetectedGame> {
        openclips_core::games::active(&self.detected)
    }

    /// Re-enumerates processes. Returns true when the detected set changed.
    pub fn refresh(&mut self, config: &GamesConfig) -> bool {
        let processes = match self.watcher.running() {
            Ok(processes) => processes,
            Err(err) => {
                warn!("{err}");
                return false;
            }
        };
        let mut detected = detect(&processes, &self.db, config);
        for game in &mut detected {
            if game.path.is_none() {
                game.path = self.watcher.process_path(game.pid);
            }
        }
        let changed = detected
            .iter()
            .map(|g| (&g.exe, g.foreground))
            .ne(self.detected.iter().map(|g| (&g.exe, g.foreground)));
        if changed {
            let names: Vec<&str> = detected.iter().map(|g| g.name.as_str()).collect();
            info!("detected games: [{}]", names.join(", "));
        }
        self.detected = detected;
        for game in self.detected.clone() {
            if let Some(path) = &game.path {
                self.icon_for_exe(&game.exe, path);
            }
        }
        changed
    }

    fn icon_path_for(&self, exe: &str) -> PathBuf {
        self.icons_dir.join(format!(
            "{}.png",
            sanitize_file_name(exe).replace(".exe", "")
        ))
    }

    /// Extracts (once) and returns the icon for an executable.
    pub fn icon_for_exe(&mut self, exe: &str, exe_path: &Path) -> Option<PathBuf> {
        if let Some(cached) = self.icon_cache.get(exe) {
            return cached.clone();
        }
        let output = self.icon_path_for(exe);
        let result = if output.exists() {
            Some(output)
        } else {
            match self.icons.extract_png(exe_path, &output) {
                Ok(()) => Some(output),
                Err(err) => {
                    debug!("no icon for {exe}: {err}");
                    None
                }
            }
        };
        self.icon_cache.insert(exe.to_owned(), result.clone());
        result
    }

    /// The cached icon for an executable, without extracting.
    pub fn cached_icon(&self, exe: &str) -> Option<PathBuf> {
        match self.icon_cache.get(exe) {
            Some(cached) => cached.clone(),
            None => {
                let path = self.icon_path_for(exe);
                path.exists().then_some(path)
            }
        }
    }

    /// Icon for a game name, through the profiles and the database. Used
    /// by the gallery, which only stores names.
    pub fn icon_for_name(&self, name: &str, config: &GamesConfig) -> Option<PathBuf> {
        let exe = config
            .profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .map(|p| p.exe.clone())
            .or_else(|| {
                self.detected
                    .iter()
                    .find(|g| g.name.eq_ignore_ascii_case(name))
                    .map(|g| g.exe.clone())
            })
            .or_else(|| {
                self.icon_cache
                    .keys()
                    .find(|exe| {
                        self.db
                            .lookup(exe)
                            .is_some_and(|n| n.eq_ignore_ascii_case(name))
                    })
                    .cloned()
            })?;
        self.cached_icon(&exe)
    }
}
