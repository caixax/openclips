//! The clip library service: keeps the index in sync with the clip folders
//! and fills in metadata and thumbnails on a worker thread.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, SystemTime};

use openclips_capture::{MediaInfo, MediaTools};
use openclips_core::config::{AppPaths, Config};
use openclips_core::library::{ClipKind, ClipRecord, LIBRARY_FILE_NAME, Library, scan_dir};
use tracing::{error, info, warn};

const THUMBNAIL_WIDTH: u32 = 480;

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

pub struct LibraryService {
    library: Library,
    index_path: PathBuf,
    clips_dir: PathBuf,
    recordings_dir: PathBuf,
    thumbnails_dir: PathBuf,
    tools: Arc<dyn MediaTools>,
    sender: Sender<JobResult>,
    results: Receiver<JobResult>,
    in_flight: HashSet<String>,
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
            recordings_dir: config.recordings_dir(paths),
            thumbnails_dir: paths.cache_dir.join("thumbnails"),
            tools,
            sender,
            results,
            in_flight: HashSet::new(),
        };
        service.refresh();
        service
    }

    pub fn set_dirs(&mut self, paths: &AppPaths, config: &Config) {
        let clips = config.clips_dir(paths);
        let recordings = config.recordings_dir(paths);
        if clips != self.clips_dir || recordings != self.recordings_dir {
            self.clips_dir = clips;
            self.recordings_dir = recordings;
            self.refresh();
        }
    }

    /// Rescans the folders and queues work for anything new. Partial files
    /// left behind by a crash are renamed so they show up as clips.
    pub fn refresh(&mut self) {
        recover_partial_files(&self.clips_dir);
        if self.recordings_dir != self.clips_dir {
            recover_partial_files(&self.recordings_dir);
        }
        let mut files = scan_dir(&self.clips_dir, ClipKind::Replay);
        if self.recordings_dir != self.clips_dir {
            files.extend(scan_dir(&self.recordings_dir, ClipKind::Recording));
        }
        for record in &mut self.library.clips {
            if record.thumbnail.as_ref().is_some_and(|t| !t.exists()) {
                record.thumbnail = None;
            }
        }
        let pending = self.library.reconcile(&files);
        self.save();
        self.queue(pending);
    }

    fn save(&self) {
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

    /// Applies finished background work. Returns true when anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(result) = self.results.try_recv() {
            self.in_flight.remove(&result.id);
            if let Some(info) = result.info {
                self.library
                    .set_probe(&result.id, info.duration, info.width, info.height);
                changed = true;
            } else if let Some(record) = self.library.get_mut(&result.id)
                && !record.probed
            {
                // The file could not be read; stop retrying it every refresh.
                record.probed = true;
                changed = true;
            }
            if result.thumbnail.is_some() {
                self.library.set_thumbnail(&result.id, result.thumbnail);
                changed = true;
            }
        }
        if changed {
            self.save();
        }
        changed
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

    /// Records the game of a freshly written file, once it is indexed.
    pub fn tag_game(&mut self, path: &std::path::Path, game: &str) {
        let id = self
            .library
            .clips
            .iter()
            .find(|c| c.path == path)
            .map(|c| c.id.clone());
        if let Some(id) = id {
            self.library.set_game(&id, Some(game.to_owned()));
            self.save();
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
        self.save();
        info!("renamed {} to {}", old.display(), title);
        Ok(())
    }

    /// Moves the file to the recycle bin and forgets it.
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
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
        self.save();
        info!("deleted {}", record.path.display());
        Ok(())
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
