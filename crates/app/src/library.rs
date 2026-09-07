//! The clip library service: keeps the index in sync with the clip folders
//! and fills in metadata and thumbnails on a worker thread.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

use openclips_capture::{MediaInfo, MediaTools};
use openclips_core::clip::ClipFile;
use openclips_core::config::{AppPaths, Config};
use openclips_core::library::{
    ClipKind, ClipRecord, LIBRARY_FILE_NAME, Library, WrittenFile, scan_tree,
};
use tracing::{error, info, warn};

const THUMBNAIL_WIDTH: u32 = 480;
/// Quiet time after the last change before the index is written. Edits
/// come in bursts (a save tags a file twice, a scan touches many), and the
/// index is rebuilt from the files anyway if the write never happens.
const SAVE_DELAY: Duration = Duration::from_secs(2);

/// What the gallery shows for one clip.
#[derive(Debug, Clone)]
pub struct CardData {
    pub id: String,
    pub title: String,
    pub game: String,
    pub date: String,
    pub duration: String,
    pub size: String,
    pub kind: String,
    pub thumbnail: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CardSort {
    #[default]
    Newest,
    Oldest,
    Longest,
    Largest,
}

impl CardSort {
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => CardSort::Oldest,
            2 => CardSort::Longest,
            3 => CardSort::Largest,
            _ => CardSort::Newest,
        }
    }
}

/// What the gallery is narrowed down to.
#[derive(Debug, Clone, Copy, Default)]
pub struct CardFilter<'a> {
    pub game: Option<&'a str>,
    pub kind: Option<ClipKind>,
    pub search: &'a str,
    pub sort: CardSort,
}

pub fn format_size(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

struct JobResult {
    id: String,
    info: Option<MediaInfo>,
    thumbnail: Option<PathBuf>,
}

/// What [`LibraryService::poll`] applied.
#[derive(Debug, Default)]
pub struct Polled {
    pub changed: bool,
    /// Thumbnail files written or rewritten, so cached pictures of them
    /// can be dropped.
    pub thumbnails: Vec<PathBuf>,
}

pub struct LibraryService {
    library: Library,
    index_path: PathBuf,
    /// Root folder plus the three subfolders (which may equal the root).
    clips_dir: PathBuf,
    clips_out_dir: PathBuf,
    recordings_dir: PathBuf,
    edited_dir: PathBuf,
    thumbnails_dir: PathBuf,
    tools: Arc<dyn MediaTools>,
    sender: Sender<JobResult>,
    results: Receiver<JobResult>,
    in_flight: HashSet<String>,
    /// When the index first diverged from the file, `None` when in sync.
    dirty_since: Option<Instant>,
}

impl LibraryService {
    pub fn new(paths: &AppPaths, config: &Config, tools: Arc<dyn MediaTools>) -> Self {
        let index_path = paths.data_dir.join(LIBRARY_FILE_NAME);
        let library = match Library::load(&index_path) {
            Ok(library) => library,
            Err(err) => {
                warn!("{err}; starting with an empty library index");
                Library::default()
            }
        };
        let (sender, results) = channel();
        let mut service = Self {
            library,
            index_path,
            clips_dir: config.clips_dir(paths),
            clips_out_dir: config.clips_out_dir(paths),
            recordings_dir: config.recordings_dir(paths),
            edited_dir: config.edited_dir(paths),
            thumbnails_dir: paths.cache_dir.join("thumbnails"),
            tools,
            sender,
            results,
            in_flight: HashSet::new(),
            dirty_since: None,
        };
        service.refresh();
        service
    }

    /// The kind a file gets from the folder it lives in.
    fn kind_for(&self, path: &Path) -> ClipKind {
        if self.recordings_dir != self.clips_dir && path.starts_with(&self.recordings_dir) {
            ClipKind::Recording
        } else if self.edited_dir != self.clips_dir && path.starts_with(&self.edited_dir) {
            ClipKind::Edited
        } else {
            ClipKind::Replay
        }
    }

    /// Puts a file the application just wrote into the index with what is
    /// known about it (no rescan) and queues its probe and thumbnail. A
    /// file already indexed (an edit that replaced it) gets its game and
    /// track names. Returns whether the gallery changed.
    pub fn index_written(&mut self, clip: &ClipFile) -> bool {
        let written = WrittenFile {
            path: clip.path.clone(),
            kind: self.kind_for(&clip.path),
            bytes: clip.bytes,
            created: clip.created,
            duration: clip.duration,
            game: clip.game.clone(),
            audio_tracks: clip.audio_tracks.clone(),
        };
        match self.library.add_written(written) {
            Some(id) => {
                self.mark_dirty();
                self.queue(vec![id]);
                true
            }
            None => {
                let id = self
                    .library
                    .clips
                    .iter()
                    .find(|c| c.path == clip.path)
                    .map(|c| c.id.clone());
                let Some(id) = id else {
                    return false;
                };
                if !clip.audio_tracks.is_empty() {
                    self.library
                        .set_audio_tracks(&id, clip.audio_tracks.clone());
                }
                if let Some(game) = &clip.game {
                    self.library.set_game(&id, Some(game.clone()));
                }
                self.mark_dirty();
                true
            }
        }
    }

    pub fn set_dirs(&mut self, paths: &AppPaths, config: &Config) {
        let dirs = (
            config.clips_dir(paths),
            config.clips_out_dir(paths),
            config.recordings_dir(paths),
            config.edited_dir(paths),
        );
        let current = (
            self.clips_dir.clone(),
            self.clips_out_dir.clone(),
            self.recordings_dir.clone(),
            self.edited_dir.clone(),
        );
        if dirs != current {
            (
                self.clips_dir,
                self.clips_out_dir,
                self.recordings_dir,
                self.edited_dir,
            ) = dirs;
            self.refresh();
        }
    }

    /// Rescans the folders and queues work for anything new. Partial files
    /// left behind by a crash are renamed so they show up as clips.
    pub fn refresh(&mut self) {
        // Folders scanned in priority order; a folder that doubles as
        // another (empty subfolder setting) is only scanned once, and each
        // scan leaves the other folders to their own pass (the root holds
        // all three). Subfolders below each (per game folders) come along.
        let plan = [
            (self.clips_out_dir.clone(), ClipKind::Replay),
            (self.recordings_dir.clone(), ClipKind::Recording),
            (self.edited_dir.clone(), ClipKind::Edited),
            (self.clips_dir.clone(), ClipKind::Replay),
        ];
        let mut seen: Vec<PathBuf> = Vec::new();
        let mut files = Vec::new();
        for (dir, kind) in &plan {
            if seen.contains(dir) {
                continue;
            }
            let others: Vec<PathBuf> = plan
                .iter()
                .map(|(d, _)| d.clone())
                .filter(|d| d != dir)
                .collect();
            recover_partial_files(dir);
            files.extend(scan_tree(dir, *kind, &others));
            seen.push(dir.clone());
        }
        let mut changed = false;
        for record in &mut self.library.clips {
            if record.thumbnail.as_ref().is_some_and(|t| !t.exists()) {
                record.thumbnail = None;
                changed = true;
            }
        }
        let (reconciled, pending) = self.library.reconcile_changed(&files);
        if changed || reconciled {
            self.mark_dirty();
        }
        self.queue(pending);
    }

    fn mark_dirty(&mut self) {
        self.dirty_since.get_or_insert_with(Instant::now);
    }

    /// Writes the index if it has been dirty for a while.
    fn save_if_due(&mut self) {
        if self
            .dirty_since
            .is_some_and(|since| since.elapsed() >= SAVE_DELAY)
        {
            self.flush();
        }
    }

    /// Writes the index now when it changed. Called at exit.
    pub fn flush(&mut self) {
        if self.dirty_since.take().is_none() {
            return;
        }
        if let Err(err) = self.library.save(&self.index_path) {
            error!("could not save the library index: {err}");
        }
    }

    fn queue(&mut self, ids: Vec<String>) {
        let jobs: Vec<(String, PathBuf, bool, Option<PathBuf>, Duration)> = ids
            .into_iter()
            .filter(|id| !self.in_flight.contains(id))
            .filter_map(|id| {
                let record = self.library.get(&id)?;
                let thumbnail = record
                    .thumbnail
                    .is_none()
                    .then(|| self.thumbnails_dir.join(record.thumbnail_file_name()));
                Some((
                    id,
                    record.path.clone(),
                    !record.probed,
                    thumbnail,
                    record.duration(),
                ))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        for (id, ..) in &jobs {
            self.in_flight.insert(id.clone());
        }
        let tools = self.tools.clone();
        let sender = self.sender.clone();
        let spawned = std::thread::Builder::new()
            .name("library-worker".to_owned())
            .spawn(move || {
                for (id, path, probe, thumbnail, known_duration) in jobs {
                    let info = if probe {
                        match tools.probe(&path) {
                            Ok(info) => Some(info),
                            Err(err) => {
                                warn!("{err}");
                                None
                            }
                        }
                    } else {
                        None
                    };
                    let duration = info.as_ref().map(|i| i.duration).unwrap_or(known_duration);
                    let thumbnail = thumbnail.and_then(|output| {
                        let at = thumbnail_time(duration);
                        match tools.thumbnail(&path, &output, at, THUMBNAIL_WIDTH) {
                            Ok(()) => Some(output),
                            Err(err) => {
                                warn!("thumbnail for {} failed: {err}", path.display());
                                None
                            }
                        }
                    });
                    if sender
                        .send(JobResult {
                            id,
                            info,
                            thumbnail,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            });
        if let Err(err) = spawned {
            error!("could not spawn the library worker: {err}");
        }
    }

    /// Applies finished background work and writes the index when it has
    /// been dirty long enough.
    pub fn poll(&mut self) -> Polled {
        let mut polled = Polled::default();
        while let Ok(result) = self.results.try_recv() {
            self.in_flight.remove(&result.id);
            if let Some(info) = result.info {
                self.library
                    .set_probe(&result.id, info.duration, info.width, info.height);
                self.library
                    .default_audio_tracks(&result.id, info.audio_tracks);
                polled.changed = true;
            } else if let Some(record) = self.library.get_mut(&result.id)
                && !record.probed
            {
                // The file could not be read; stop retrying it every refresh.
                record.probed = true;
                polled.changed = true;
            }
            if let Some(thumbnail) = result.thumbnail {
                polled.thumbnails.push(thumbnail.clone());
                self.library.set_thumbnail(&result.id, Some(thumbnail));
                polled.changed = true;
            }
        }
        if polled.changed {
            self.mark_dirty();
        }
        self.save_if_due();
        polled
    }

    pub fn record(&self, id: &str) -> Option<&ClipRecord> {
        self.library.get(id)
    }

    pub fn games(&self) -> Vec<String> {
        self.library.games()
    }

    pub fn cards(&self, filter: &CardFilter<'_>) -> Vec<CardData> {
        let needle = filter.search.trim().to_lowercase();
        let mut clips: Vec<&ClipRecord> = self
            .library
            .sorted()
            .into_iter()
            .filter(|c| filter.game.is_none_or(|g| c.game.as_deref() == Some(g)))
            .filter(|c| filter.kind.is_none_or(|k| c.kind == k))
            .filter(|c| needle.is_empty() || c.title.to_lowercase().contains(&needle))
            .collect();
        match filter.sort {
            CardSort::Newest => {}
            CardSort::Oldest => clips.reverse(),
            CardSort::Longest => clips.sort_by_key(|c| std::cmp::Reverse(c.duration_ms)),
            CardSort::Largest => clips.sort_by_key(|c| std::cmp::Reverse(c.bytes)),
        }
        clips
            .into_iter()
            .map(|c| CardData {
                id: c.id.clone(),
                title: c.title.clone(),
                game: c.game.clone().unwrap_or_else(|| c.kind.label().to_owned()),
                date: format_date(c.created),
                duration: format_duration(c.duration()),
                size: format_size(c.bytes),
                kind: c.kind.label().to_owned(),
                thumbnail: c.thumbnail.clone(),
            })
            .collect()
    }

    /// Bytes used by every indexed file.
    pub fn total_bytes(&self) -> u64 {
        self.library.clips.iter().map(|c| c.bytes).sum()
    }

    /// Records the audio track names of a freshly written file.
    pub fn tag_tracks(&mut self, path: &std::path::Path, tracks: &[String]) {
        let id = self
            .library
            .clips
            .iter()
            .find(|c| c.path == path)
            .map(|c| c.id.clone());
        if let Some(id) = id
            && !tracks.is_empty()
        {
            self.library.set_audio_tracks(&id, tracks.to_vec());
            self.mark_dirty();
        }
    }

    pub fn rename(&mut self, id: &str, title: &str) -> Result<(), String> {
        let (old, new) = self
            .library
            .rename_target(id, title)
            .ok_or_else(|| "The title cannot be empty.".to_owned())?;
        if old != new {
            std::fs::rename(&old, &new).map_err(|e| format!("Could not rename the file: {e}"))?;
        }
        self.library.apply_rename(id, title, new);
        self.mark_dirty();
        info!("renamed {} to {}", old.display(), title);
        Ok(())
    }

    /// Moves the file to the recycle bin and forgets it. Returns the
    /// thumbnail file that went with it, for caches.
    pub fn delete(&mut self, id: &str) -> Result<Option<PathBuf>, String> {
        let record = self
            .library
            .get(id)
            .cloned()
            .ok_or_else(|| "Unknown clip.".to_owned())?;
        if record.path.exists() {
            trash::delete(&record.path).map_err(|e| format!("Could not delete the file: {e}"))?;
        }
        if let Some(thumbnail) = &record.thumbnail {
            let _ = std::fs::remove_file(thumbnail);
        }
        self.library.remove(id);
        self.mark_dirty();
        info!("deleted {}", record.path.display());
        Ok(record.thumbnail)
    }
}

impl Drop for LibraryService {
    fn drop(&mut self) {
        self.flush();
    }
}

fn thumbnail_time(duration: Duration) -> Duration {
    if duration.is_zero() {
        Duration::from_secs(1)
    } else {
        (duration / 10).min(Duration::from_secs(5))
    }
}

pub fn format_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn format_date(when: SystemTime) -> String {
    let local: chrono::DateTime<chrono::Local> = when.into();
    local.format("%Y-%m-%d %H:%M").to_string()
}

/// A `.mp4.part` that nobody has touched for a while is a recording cut
/// short by a crash. Fragmented output keeps it playable, so it is renamed
/// into a clip instead of being left to rot. Files still being written are
/// skipped by their recent modification time.
fn recover_partial_files(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_part = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_lowercase().ends_with(".mp4.part"));
        if !is_part {
            continue;
        }
        let recent = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(60));
        if recent {
            continue;
        }
        let stem = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n[..n.len() - ".mp4.part".len()].to_owned())
            .unwrap_or_else(|| "Recovered".to_owned());
        let target = openclips_core::clip::unique_path(dir, &format!("{stem} (recovered).mp4"));
        match std::fs::rename(&path, &target) {
            Ok(()) => info!("recovered {} as {}", path.display(), target.display()),
            Err(err) => warn!("could not recover {}: {err}", path.display()),
        }
    }
}
