//! The clip library: an index of clip files with the metadata the gallery
//! shows, persisted as JSON next to the other application data. The files
//! themselves stay ordinary MP4s; the index can always be rebuilt from them.

use std::collections::HashMap;
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
    Edited,
}

impl ClipKind {
    pub const fn label(self) -> &'static str {
        match self {
            ClipKind::Replay => "Clip",
            ClipKind::Recording => "Recording",
            ClipKind::Edited => "Edited",
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
    /// Names of the audio tracks, when known (mixed, desktop, an app).
    #[serde(default)]
    pub audio_tracks: Vec<String>,
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

/// What the application knows about a file it just wrote, enough for a
/// record ahead of the probe (see [`Library::add_written`]).
#[derive(Debug, Clone, PartialEq)]
pub struct WrittenFile {
    pub path: PathBuf,
    pub kind: ClipKind,
    pub bytes: u64,
    pub created: SystemTime,
    pub duration: Duration,
    pub game: Option<String>,
    pub audio_tracks: Vec<String>,
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
        self.reconcile_changed(files).1
    }

    /// [`Library::reconcile`] that also says whether anything changed, so
    /// callers can skip a save when the folders match the index.
    pub fn reconcile_changed(&mut self, files: &[ScannedFile]) -> (bool, Vec<String>) {
        let on_disk: HashMap<&Path, &ScannedFile> =
            files.iter().map(|f| (f.path.as_path(), f)).collect();
        let before = self.clips.len();
        self.clips
            .retain(|c| on_disk.contains_key(c.path.as_path()));
        let mut changed = self.clips.len() != before;
        let mut known: HashMap<PathBuf, usize> = self
            .clips
            .iter()
            .enumerate()
            .map(|(i, c)| (c.path.clone(), i))
            .collect();
        for file in files {
            match known.get(&file.path) {
                Some(&index) => {
                    let record = &mut self.clips[index];
                    if record.bytes != file.bytes {
                        // The content changed under the same name (an edit
                        // that replaced the original): both the metadata
                        // and the picture must be read again.
                        record.bytes = file.bytes;
                        record.probed = false;
                        record.thumbnail = None;
                        changed = true;
                    }
                }
                None => {
                    self.clips.push(ClipRecord {
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
                        audio_tracks: Vec::new(),
                    });
                    known.insert(file.path.clone(), self.clips.len() - 1);
                    changed = true;
                }
            }
        }
        (changed, self.pending())
    }

    /// Adds a record for a file the application just wrote, with what is
    /// already known about it, so the gallery shows it without a rescan.
    /// Returns the id when the record is new.
    pub fn add_written(&mut self, file: WrittenFile) -> Option<String> {
        if self.clips.iter().any(|c| c.path == file.path) {
            return None;
        }
        let id = make_id(&file.path, file.created);
        self.clips.push(ClipRecord {
            id: id.clone(),
            title: title_from_path(&file.path),
            path: file.path,
            game: file.game,
            kind: file.kind,
            created: file.created,
            duration_ms: file.duration.as_millis() as u64,
            bytes: file.bytes,
            width: 0,
            height: 0,
            thumbnail: None,
            probed: false,
            audio_tracks: file.audio_tracks,
        });
        Some(id)
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

    pub fn set_audio_tracks(&mut self, id: &str, tracks: Vec<String>) {
        if let Some(record) = self.get_mut(id) {
            record.audio_tracks = tracks;
        }
    }

    /// Names tracks generically when the file was probed and nothing
    /// better is known.
    pub fn default_audio_tracks(&mut self, id: &str, count: u32) {
        if let Some(record) = self.get_mut(id)
            && record.audio_tracks.is_empty()
        {
            record.audio_tracks = (1..=count).map(|n| format!("Track {n}")).collect();
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
    let mut files = Vec::new();
    scan_into(dir, kind, &[], 0, &mut files);
    files
}

/// How deep [`scan_tree`] follows subfolders below the folder it is given.
pub const SCAN_DEPTH: usize = 3;

/// Lists the `.mp4` files inside `dir` and its subfolders (per game
/// subfolders put clips one level down), leaving out the folders in `skip`,
/// which are scanned on their own with another kind.
pub fn scan_tree(dir: &Path, kind: ClipKind, skip: &[PathBuf]) -> Vec<ScannedFile> {
    let mut files = Vec::new();
    scan_into(dir, kind, skip, SCAN_DEPTH, &mut files);
    files
}

fn scan_into(
    dir: &Path,
    kind: ClipKind,
    skip: &[PathBuf],
    depth: usize,
    files: &mut Vec<ScannedFile>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            if depth > 0 && !skip.iter().any(|s| s == &path) {
                scan_into(&path, kind, skip, depth - 1, files);
            }
            continue;
        }
        let is_mp4 = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("mp4"));
        if !is_mp4 || !meta.is_file() {
            continue;
        }
        files.push(ScannedFile {
            path,
            bytes: meta.len(),
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            kind,
        });
    }
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
        let (changed, pending) = library.reconcile_changed(&[bigger.clone()]);
        assert!(changed);
        assert_eq!(pending, vec![id.clone()]);
        assert!(
            library.get(&id).is_some_and(|c| c.thumbnail.is_none()),
            "a changed file gets a new thumbnail"
        );
        library.set_probe(&id, Duration::from_secs(5), 1280, 720);
        library.set_thumbnail(&id, Some(PathBuf::from("/cache/a.png")));
        let (changed, pending) = library.reconcile_changed(&[bigger]);
        assert!(!changed, "an unchanged folder is not a change");
        assert!(pending.is_empty());
    }

    #[test]
    fn written_files_are_added_once_with_what_is_known() {
        let dir = Path::new("/clips");
        let mut library = Library::default();
        let when = SystemTime::UNIX_EPOCH + Duration::from_secs(50);
        let written = WrittenFile {
            path: dir.join("new.mp4"),
            kind: ClipKind::Recording,
            bytes: 1234,
            created: when,
            duration: Duration::from_secs(9),
            game: Some("Game".to_owned()),
            audio_tracks: vec!["Audio".to_owned()],
        };
        let id = library.add_written(written.clone()).expect("added");
        let record = library.get(&id).expect("record");
        assert_eq!(record.kind, ClipKind::Recording);
        assert_eq!(record.duration_ms, 9000);
        assert_eq!(record.game.as_deref(), Some("Game"));
        assert!(!record.probed, "dimensions still need a probe");
        assert_eq!(library.pending(), vec![id.clone()]);
        assert!(
            library.add_written(written).is_none(),
            "the same path is not added twice"
        );
        let pending = library.reconcile(&[file(dir, "new.mp4", 50)]);
        assert_eq!(library.clips.len(), 1);
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

    #[test]
    fn scan_tree_follows_subfolders_but_not_the_skipped_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let game = dir.path().join("Half-Life");
        let recordings = dir.path().join("Recordings");
        fs::create_dir_all(game.join("deeper")).expect("mkdir");
        fs::create_dir(&recordings).expect("mkdir");
        fs::write(dir.path().join("root.mp4"), b"x").expect("write");
        fs::write(game.join("clip.mp4"), b"x").expect("write");
        fs::write(game.join("deeper").join("nested.mp4"), b"x").expect("write");
        fs::write(recordings.join("rec.mp4"), b"x").expect("write");
        let mut names: Vec<String> = scan_tree(dir.path(), ClipKind::Replay, &[recordings])
            .into_iter()
            .map(|f| {
                f.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "clip.mp4".to_owned(),
                "nested.mp4".to_owned(),
                "root.mp4".to_owned()
            ]
        );
    }
}
