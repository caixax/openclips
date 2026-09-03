//! The clip library: an index of clip files with the metadata the gallery
//! shows, persisted as JSON next to the other application data. The files
//! themselves stay ordinary MP4s; the index can always be rebuilt from them.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::clip::sanitize_file_name;
use crate::error::{CoreError, Result};

pub const LIBRARY_FILE_NAME: &str = "library.json";
pub const LIBRARY_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipKind {
    Replay,
    Recording,
}

impl ClipKind {
    pub const fn label(self) -> &'static str {
        match self {
            ClipKind::Replay => "Clip",
            ClipKind::Recording => "Recording",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipRecord {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub game: Option<String>,
    pub kind: ClipKind,
    pub created: SystemTime,
    pub duration_ms: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub thumbnail: Option<PathBuf>,
    /// Duration and dimensions were read from the file.
    pub probed: bool,
}

impl ClipRecord {
    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.duration_ms)
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Stable name for the cached thumbnail of this file.
    pub fn thumbnail_file_name(&self) -> String {
        let mut hasher = DefaultHasher::new();
        self.path.hash(&mut hasher);
        self.created
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .hash(&mut hasher);
        format!("{:016x}.png", hasher.finish())
    }
}

/// A clip file found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: SystemTime,
    pub kind: ClipKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub version: u32,
    pub clips: Vec<ClipRecord>,
}

impl Default for Library {
    fn default() -> Self {
        Self {
            version: LIBRARY_VERSION,
            clips: Vec::new(),
        }
    }
}

fn make_id(path: &Path, created: SystemTime) -> String {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    created
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn title_from_path(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Clip".to_owned())
}

impl Library {
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(CoreError::ReadFile {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        serde_json::from_str(&text).map_err(|source| CoreError::ParseLibrary {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|source| CoreError::CreateDir {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        let text = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text).map_err(|source| CoreError::WriteFile {
            path: tmp.clone(),
            source,
        })?;
        fs::rename(&tmp, path).map_err(|source| CoreError::WriteFile {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn get(&self, id: &str) -> Option<&ClipRecord> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut ClipRecord> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// Newest first.
    pub fn sorted(&self) -> Vec<&ClipRecord> {
        let mut clips: Vec<&ClipRecord> = self.clips.iter().collect();
        clips.sort_by_key(|c| std::cmp::Reverse(c.created));
        clips
    }

    /// Distinct game names, sorted.
    pub fn games(&self) -> Vec<String> {
        let mut games: Vec<String> = self.clips.iter().filter_map(|c| c.game.clone()).collect();
        games.sort();
        games.dedup();
        games
    }

    /// Brings the index in line with the files on disk: records whose file
    /// vanished are dropped, new files get a record, and edits made in the
    /// library (titles, games) survive. Returns the ids that still need a
    /// probe or a thumbnail.
    pub fn reconcile(&mut self, files: &[ScannedFile]) -> Vec<String> {
        self.clips
            .retain(|c| files.iter().any(|f| f.path == c.path));
        for file in files {
            match self.clips.iter_mut().find(|c| c.path == file.path) {
                Some(record) => {
                    if record.bytes != file.bytes {
                        record.bytes = file.bytes;
                        record.probed = false;
                    }
                }
                None => self.clips.push(ClipRecord {
                    id: make_id(&file.path, file.modified),
                    path: file.path.clone(),
                    title: title_from_path(&file.path),
                    game: None,
                    kind: file.kind,
                    created: file.modified,
                    duration_ms: 0,
                    bytes: file.bytes,
                    width: 0,
                    height: 0,
                    thumbnail: None,
                    probed: false,
                }),
            }
        }
        self.pending()
    }

    /// Ids of records that still need a probe or a thumbnail.
    pub fn pending(&self) -> Vec<String> {
        self.clips
            .iter()
            .filter(|c| !c.probed || c.thumbnail.is_none())
            .map(|c| c.id.clone())
            .collect()
    }

    pub fn set_probe(&mut self, id: &str, duration: Duration, width: u32, height: u32) {
        if let Some(record) = self.get_mut(id) {
            record.duration_ms = duration.as_millis() as u64;
            record.width = width;
            record.height = height;
            record.probed = true;
        }
    }

    pub fn set_thumbnail(&mut self, id: &str, thumbnail: Option<PathBuf>) {
        if let Some(record) = self.get_mut(id) {
            record.thumbnail = thumbnail;
        }
    }

    pub fn set_game(&mut self, id: &str, game: Option<String>) {
        if let Some(record) = self.get_mut(id) {
            record.game = game;
        }
    }

    /// Computes where the file should live for a new title. The caller
    /// renames the file, then confirms with [`Library::apply_rename`].
    pub fn rename_target(&self, id: &str, title: &str) -> Option<(PathBuf, PathBuf)> {
        let record = self.get(id)?;
        let clean = sanitize_file_name(title);
        if clean.is_empty() {
            return None;
        }
        let dir = record.path.parent()?;
        let ext = record
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mp4");
        let mut target = dir.join(format!("{clean}.{ext}"));
        let mut n = 2;
        while target != record.path
            && (target.exists() || self.clips.iter().any(|c| c.path == target))
        {
            target = dir.join(format!("{clean} ({n}).{ext}"));
            n += 1;
        }
        Some((record.path.clone(), target))
    }

    pub fn apply_rename(&mut self, id: &str, title: &str, path: PathBuf) {
        if let Some(record) = self.get_mut(id) {
            record.title = title.trim().to_owned();
            record.path = path;
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<ClipRecord> {
        let index = self.clips.iter().position(|c| c.id == id)?;
        Some(self.clips.remove(index))
    }
}

/// Lists the `.mp4` files directly inside `dir`. Partial files being
/// written are skipped.
pub fn scan_dir(dir: &Path, kind: ClipKind) -> Vec<ScannedFile> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_mp4 = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("mp4"));
        if !is_mp4 {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        files.push(ScannedFile {
            path,
            bytes: meta.len(),
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            kind,
        });
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, name: &str, secs: u64) -> ScannedFile {
        ScannedFile {
            path: dir.join(name),
            bytes: 100,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            kind: ClipKind::Replay,
        }
    }

    #[test]
    fn reconcile_adds_removes_and_keeps_edits() {
        let dir = Path::new("/clips");
        let mut library = Library::default();
        let pending = library.reconcile(&[file(dir, "a.mp4", 10), file(dir, "b.mp4", 20)]);
        assert_eq!(library.clips.len(), 2);
        assert_eq!(pending.len(), 2);

        let id = library.sorted()[0].id.clone();
        assert_eq!(library.get(&id).map(|c| c.title.as_str()), Some("b"));
        library.set_game(&id, Some("Half-Life".to_owned()));
        library.set_probe(&id, Duration::from_secs(30), 1920, 1080);
        library.set_thumbnail(&id, Some(PathBuf::from("/cache/b.png")));

        let pending = library.reconcile(&[file(dir, "b.mp4", 20), file(dir, "c.mp4", 30)]);
        assert_eq!(library.clips.len(), 2);
        assert!(library.get(&id).is_some(), "record b survives");
        assert_eq!(
            library.get(&id).and_then(|c| c.game.clone()).as_deref(),
            Some("Half-Life")
        );
        assert_eq!(pending.len(), 1, "only c needs work");
        assert_eq!(library.games(), vec!["Half-Life".to_owned()]);
    }

    #[test]
    fn changed_size_triggers_reprobe() {
        let dir = Path::new("/clips");
        let mut library = Library::default();
        library.reconcile(&[file(dir, "a.mp4", 10)]);
        let id = library.clips[0].id.clone();
        library.set_probe(&id, Duration::from_secs(5), 1280, 720);
        library.set_thumbnail(&id, Some(PathBuf::from("/cache/a.png")));
        assert!(library.pending().is_empty());
        let mut bigger = file(dir, "a.mp4", 10);
        bigger.bytes = 200;
        let pending = library.reconcile(&[bigger]);
        assert_eq!(pending, vec![id]);
    }

    #[test]
    fn rename_target_sanitizes_and_avoids_collisions() {
        let dir = Path::new("/clips");
        let mut library = Library::default();
        library.reconcile(&[file(dir, "a.mp4", 10), file(dir, "taken.mp4", 20)]);
        let id = library
            .clips
            .iter()
            .find(|c| c.title == "a")
            .map(|c| c.id.clone())
            .expect("a");
        let (old, new) = library.rename_target(&id, "Best: play?").expect("target");
        assert_eq!(old, dir.join("a.mp4"));
        assert_eq!(new, dir.join("Best_ play_.mp4"));
        let (_, collision) = library.rename_target(&id, "taken").expect("target");
        assert_eq!(collision, dir.join("taken (2).mp4"));
        assert!(library.rename_target(&id, "   ").is_none());

        library.apply_rename(&id, "Best play", new.clone());
        assert_eq!(library.get(&id).map(|c| c.path.clone()), Some(new));
        assert_eq!(
            library.get(&id).map(|c| c.title.as_str()),
            Some("Best play")
        );
    }

    #[test]
    fn json_round_trip_and_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("library.json");
        assert_eq!(Library::load(&path).expect("load"), Library::default());

        let mut library = Library::default();
        library.reconcile(&[file(dir.path(), "a.mp4", 10)]);
        library.save(&path).expect("save");
        let back = Library::load(&path).expect("load");
        assert_eq!(back, library);
    }

    #[test]
    fn scan_dir_lists_only_mp4_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("a.mp4"), b"x").expect("write");
        fs::write(dir.path().join("b.MP4"), b"xx").expect("write");
        fs::write(dir.path().join("c.mp4.part"), b"x").expect("write");
        fs::write(dir.path().join("notes.txt"), b"x").expect("write");
        fs::create_dir(dir.path().join("Recordings")).expect("mkdir");
        let mut files = scan_dir(dir.path(), ClipKind::Replay);
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let names: Vec<String> = files
            .iter()
            .map(|f| {
                f.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(names, vec!["a.mp4".to_owned(), "b.MP4".to_owned()]);
        assert!(scan_dir(&dir.path().join("missing"), ClipKind::Replay).is_empty());
    }
}
