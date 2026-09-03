//! Connects the capture backend to the replay buffer and the session
//! recorder, and turns hotkey presses into clip files. Owned by the UI
//! thread; backend threads only touch the shared sink state.

use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use openclips_capture::{
    CaptureBackend, CaptureError, ClipWriter, FrameSink, IconExtractor, MediaTools, Player,
    PlayerSink, ProcessWatcher, Recorder, RecordingSession,
};
use openclips_core::capture::{
    AudioDeviceInfo, CaptureSettings, EncoderInfo, MonitorInfo, audio_source_key, choose_encoder,
};
use openclips_core::clip::{ClipFile, LocalDateTime, clip_file_name, unique_path};
use openclips_core::config::{AppPaths, Config, DisplaySelection};
use openclips_core::games::{AutoCapture, DetectedGame};
use openclips_core::media::{AudioPacket, AudioTrackInfo, EncodedFrame, StreamInfo, Timestamp};
use openclips_core::replay::{ReplayBuffer, ReplayLimits, ReplayStats};
use tracing::{error, info, warn};

use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferState {
    Stopped,
    Running,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    /// Waiting for the next keyframe to open the file.
    Starting,
    Active {
        path: PathBuf,
        duration: Duration,
    },
    Finishing,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct EngineStatus {
    pub buffer: BufferState,
    pub recording: RecordingState,
    pub stats: ReplayStats,
    pub stream: Option<StreamInfo>,
    pub audio_tracks: usize,
    pub encoder: EncoderInfo,
    pub backend: &'static str,
    pub replay_length: Duration,
    /// Non fatal information such as an encoder fallback.
    pub notice: Option<String>,
    /// The capture is producing black or empty frames.
    pub blank: bool,
}

pub type SaveCallback = Box<dyn FnOnce(Result<ClipFile, String>) + Send + 'static>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum SinkRecording {
    Idle,
    Pending(PathBuf),
    Active(Box<dyn RecordingSession>, Timestamp, Timestamp),
    Failed(String),
}

/// Everything the capture threads write into. The ring buffer and the
/// recording are fed from the same encoded streams.
struct CaptureSink {
    buffer: Arc<Mutex<ReplayBuffer>>,
    buffer_enabled: Mutex<bool>,
    recording: Mutex<SinkRecording>,
    recorder: Arc<dyn Recorder>,
    failure: Mutex<Option<CaptureError>>,
}

impl CaptureSink {
    fn feed_recording(&self, frame: &EncodedFrame) {
        let mut slot = lock(&self.recording);
        match &mut *slot {
            SinkRecording::Pending(path) => {
                if !frame.keyframe {
                    return;
                }
                let (stream, audio) = {
                    let buffer = lock(&self.buffer);
                    (buffer.stream().cloned(), buffer.audio_tracks())
                };
                let Some(stream) = stream else {
                    return;
                };
                let path = path.clone();
                match self.recorder.start(&stream, &audio, &path) {
                    Ok(mut session) => {
                        if let Err(err) = session.push(frame) {
                            *slot = SinkRecording::Failed(err.to_string());
                            return;
                        }
                        *slot = SinkRecording::Active(session, frame.pts, frame.pts);
                    }
                    Err(err) => *slot = SinkRecording::Failed(err.to_string()),
                }
            }
            SinkRecording::Active(session, _, last) => {
                if let Err(err) = session.push(frame) {
                    error!("{err}");
                    *slot = SinkRecording::Failed(err.to_string());
                    return;
                }
                *last = frame.pts;
            }
            SinkRecording::Idle | SinkRecording::Failed(_) => {}
        }
    }

    fn feed_recording_audio(&self, packet: &AudioPacket) {
        let mut slot = lock(&self.recording);
        if let SinkRecording::Active(session, _, _) = &mut *slot
            && let Err(err) = session.push_audio(packet)
        {
            error!("{err}");
            *slot = SinkRecording::Failed(err.to_string());
        }
    }
}

impl FrameSink for CaptureSink {
    fn on_stream(&self, info: StreamInfo) {
        lock(&self.buffer).set_stream(info);
    }

    fn on_frame(&self, frame: EncodedFrame) {
        self.feed_recording(&frame);
        if *lock(&self.buffer_enabled) {
            lock(&self.buffer).push(frame);
        }
    }

    fn on_audio_track(&self, info: AudioTrackInfo) {
        lock(&self.buffer).set_audio_track(info);
    }

    fn on_audio(&self, packet: AudioPacket) {
        self.feed_recording_audio(&packet);
        if *lock(&self.buffer_enabled) {
            lock(&self.buffer).push_audio(packet);
        }
    }

    fn on_error(&self, error: CaptureError) {
        *lock(&self.failure) = Some(error);
    }
}

pub struct Engine {
    backend: Box<dyn CaptureBackend>,
    writer: Arc<dyn ClipWriter>,
    buffer: Arc<Mutex<ReplayBuffer>>,
    sink: Arc<CaptureSink>,
    config: Config,
    paths: AppPaths,
    /// The encoder the user asked for (or the best registered one).
    preferred: EncoderInfo,
    /// The encoder actually driving the running or last capture.
    active: EncoderInfo,
    /// Whether the user wants the replay buffer running.
    buffer_wanted: bool,
    /// Whether the game watcher wants the buffer running.
    auto_buffer: bool,
    recording_wanted: bool,
    /// The recording was started by the game watcher and ends with the game.
    auto_recording: bool,
    /// The game that is currently driving naming and overrides.
    active_game: Option<DetectedGame>,
    finishing: bool,
    /// A config change that needs a pipeline rebuild, deferred while a
    /// recording is active.
    restart_pending: bool,
    /// Audio sources that failed this session, skipped until settings change.
    unavailable_audio: HashSet<String>,
    notice: Option<String>,
    last_failure: Option<String>,
    monitors: Vec<MonitorInfo>,
    /// Timestamps of automatic restarts after capture errors, to cap them.
    restarts: VecDeque<Instant>,
    blank_warned: bool,
}

impl Engine {
    pub fn new(config: Config, paths: AppPaths) -> Result<Self, AppError> {
        let backend = openclips_capture::create_backend()?;
        let encoder = choose_encoder(backend.available_encoders(), config.capture.encoder)
            .cloned()
            .ok_or(CaptureError::NoEncoder)?;
        info!(
            "using encoder {} ({})",
            encoder.kind.label(),
            encoder.element
        );

        let buffer = Arc::new(Mutex::new(ReplayBuffer::new(Self::limits(&config))));
        let sink = Arc::new(CaptureSink {
            buffer: buffer.clone(),
            buffer_enabled: Mutex::new(false),
            recording: Mutex::new(SinkRecording::Idle),
            recorder: backend.recorder(),
            failure: Mutex::new(None),
        });
        let writer = backend.clip_writer();
        let monitors = backend.list_monitors().unwrap_or_default();
        Ok(Self {
            backend,
            writer,
            buffer,
            sink,
            config,
            paths,
            active: encoder.clone(),
            preferred: encoder,
            buffer_wanted: false,
            auto_buffer: false,
            recording_wanted: false,
            auto_recording: false,
            active_game: None,
            finishing: false,
            restart_pending: false,
            unavailable_audio: HashSet::new(),
            notice: None,
            last_failure: None,
            monitors,
            restarts: VecDeque::new(),
            blank_warned: false,
        })
    }

    fn limits(config: &Config) -> ReplayLimits {
        ReplayLimits {
            max_duration: config.replay_length(),
            max_bytes: config.replay_memory_cap_bytes(),
        }
    }

    pub fn monitors(&self) -> &[MonitorInfo] {
        &self.monitors
    }

    pub fn list_audio_devices(&self) -> Vec<AudioDeviceInfo> {
        match self.backend.list_audio_devices() {
            Ok(devices) => devices,
            Err(err) => {
                warn!("could not list audio devices: {err}");
                Vec::new()
            }
        }
    }

    pub fn clips_dir(&self) -> PathBuf {
        self.config.clips_dir(&self.paths)
    }

    pub fn media_tools(&self) -> Arc<dyn MediaTools> {
        self.backend.media_tools()
    }

    pub fn create_player(&self, sink: Arc<dyn PlayerSink>) -> Result<Box<dyn Player>, AppError> {
        Ok(self.backend.create_player(sink)?)
    }

    pub fn process_watcher(&self) -> Arc<dyn ProcessWatcher> {
        self.backend.process_watcher()
    }

    pub fn icon_extractor(&self) -> Arc<dyn IconExtractor> {
        self.backend.icon_extractor()
    }

    fn game_name(&self) -> String {
        self.active_game
            .as_ref()
            .map(|g| g.name.clone())
            .unwrap_or_default()
    }

    /// Replay length with the active game's override applied.
    fn effective_replay_length(&self) -> Duration {
        self.active_game
            .as_ref()
            .and_then(|g| g.profile.as_ref())
            .and_then(|p| p.replay_length_seconds)
            .map(|s| Duration::from_secs(u64::from(s)))
            .unwrap_or_else(|| self.config.replay_length())
    }

    fn effective_display(&self) -> DisplaySelection {
        self.active_game
            .as_ref()
            .and_then(|g| g.profile.as_ref())
            .and_then(|p| p.display.clone())
            .unwrap_or_else(|| self.config.capture.display.clone())
    }

    fn output_dir(&self, base: PathBuf) -> PathBuf {
        match self
            .active_game
            .as_ref()
            .and_then(|g| g.profile.as_ref())
            .and_then(|p| p.subfolder.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(sub) => base.join(sub),
            None => base,
        }
    }

    /// Applies the watcher's view: the active game (for naming and
    /// overrides) and what capture should be doing in per game scope.
    pub fn set_game_state(
        &mut self,
        active: Option<DetectedGame>,
        auto: AutoCapture,
    ) -> Result<(), AppError> {
        let previous_display = self.effective_display();
        let game_changed =
            active.as_ref().map(|g| &g.exe) != self.active_game.as_ref().map(|g| &g.exe);
        self.active_game = active;
        if game_changed {
            if let Some(game) = &self.active_game {
                info!("active game: {} ({})", game.name, game.exe);
            }
            lock(&self.buffer).set_limits(ReplayLimits {
                max_duration: self.effective_replay_length(),
                max_bytes: self.config.replay_memory_cap_bytes(),
            });
            if self.effective_display() != previous_display
                && self.backend.is_running()
                && !self.recording_wanted
            {
                self.restart_capture()?;
            }
        }

        let want_buffer = auto == AutoCapture::Buffer;
        let want_recording = auto == AutoCapture::Recording;
        if want_buffer != self.auto_buffer {
            self.auto_buffer = want_buffer;
            *lock(&self.sink.buffer_enabled) = self.buffer_wanted || self.auto_buffer;
            if self.auto_buffer {
                self.ensure_capture()?;
            } else if !self.buffer_wanted {
                lock(&self.buffer).clear();
                self.release_capture_if_unused();
            }
        }
        if want_recording && !self.recording_wanted {
            self.auto_recording = true;
            self.start_recording()?;
        } else if !want_recording && self.auto_recording && self.recording_wanted {
            self.auto_recording = false;
            self.stop_recording(Box::new(|result| match result {
                Ok(clip) => info!("game recording saved: {}", clip.path.display()),
                Err(err) => warn!("game recording failed: {err}"),
            }));
        }
        Ok(())
    }

    pub fn start_buffer(&mut self) -> Result<(), AppError> {
        self.buffer_wanted = true;
        *lock(&self.sink.buffer_enabled) = true;
        self.ensure_capture()
    }

    pub fn stop_buffer(&mut self) {
        self.buffer_wanted = false;
        self.auto_buffer = false;
        *lock(&self.sink.buffer_enabled) = false;
        lock(&self.buffer).clear();
        self.release_capture_if_unused();
    }

    pub fn toggle_buffer(&mut self) -> Result<(), AppError> {
        if self.is_buffering() {
            self.stop_buffer();
            Ok(())
        } else {
            self.start_buffer()
        }
    }

    pub fn is_capturing(&self) -> bool {
        self.backend.is_running() && lock(&self.sink.failure).is_none()
    }

    pub fn is_buffering(&self) -> bool {
        (self.buffer_wanted || self.auto_buffer) && self.is_capturing()
    }

    /// Starts a session recording, starting capture first when needed. The
    /// file opens at the next keyframe.
    pub fn start_recording(&mut self) -> Result<(), AppError> {
        if self.recording_wanted {
            return Ok(());
        }
        self.ensure_capture()?;
        let dir = self.output_dir(self.config.recordings_dir(&self.paths));
        let name = clip_file_name(
            &self.config.output.file_name_pattern,
            &self.game_name(),
            &now_local(),
        );
        let path = unique_path(&dir, &name);
        *lock(&self.sink.recording) = SinkRecording::Pending(path);
        self.recording_wanted = true;
        Ok(())
    }

    /// Stops the recording and finalises the file on a worker thread. `done`
    /// runs on that thread; it must not touch the engine directly.
    pub fn stop_recording(&mut self, done: SaveCallback) {
        if !self.recording_wanted {
            return;
        }
        self.recording_wanted = false;
        self.auto_recording = false;
        let state = std::mem::replace(&mut *lock(&self.sink.recording), SinkRecording::Idle);
        match state {
            SinkRecording::Active(session, _, _) => {
                self.finishing = true;
                spawn_named("recording-finish", move || {
                    done(session.finish().map_err(|e| e.to_string()));
                });
            }
            SinkRecording::Pending(_) => done(Err("no frame was recorded".to_owned())),
            SinkRecording::Failed(reason) => done(Err(reason)),
            SinkRecording::Idle => {}
        }
        self.release_capture_if_unused();
        if self.restart_pending {
            self.restart_pending = false;
            if let Err(err) = self.restart_capture() {
                self.last_failure = Some(err.to_string());
            }
        }
    }

    pub fn toggle_recording(&mut self, done: SaveCallback) -> Result<(), AppError> {
        if self.recording_wanted {
            self.stop_recording(done);
            Ok(())
        } else {
            self.start_recording()
        }
    }

    /// Called by the UI when the worker thread reports the file is closed.
    pub fn recording_finished(&mut self) {
        self.finishing = false;
    }

    fn ensure_capture(&mut self) -> Result<(), AppError> {
        if self.is_capturing() {
            return Ok(());
        }
        self.backend.stop();
        self.start_capture()
    }

    fn release_capture_if_unused(&mut self) {
        if !self.buffer_wanted && !self.auto_buffer && !self.recording_wanted {
            self.backend.stop();
        }
    }

    fn restart_capture(&mut self) -> Result<(), AppError> {
        self.backend.stop();
        lock(&self.buffer).clear();
        if !self.buffer_wanted && !self.auto_buffer && !self.recording_wanted {
            return Ok(());
        }
        self.start_capture()
    }

    fn capture_settings(&self, encoder: EncoderInfo, display: DisplaySelection) -> CaptureSettings {
        let mut settings = CaptureSettings::from_config(
            &self.config.capture,
            &self.config.audio,
            encoder,
            self.config.replay.temp_dir.clone(),
        );
        settings.display = display;
        for track in &mut settings.audio_tracks {
            track
                .sources
                .retain(|s| !self.unavailable_audio.contains(&s.key()));
        }
        settings.audio_tracks.retain(|t| !t.sources.is_empty());
        settings
    }

    /// Starts capture with the preferred encoder and falls back through the
    /// remaining registered encoders when one refuses to start. An audio
    /// source that fails is dropped and the start is retried without it.
    /// Both cases are reported through the status notice.
    fn start_capture(&mut self) -> Result<(), AppError> {
        *lock(&self.sink.failure) = None;
        self.last_failure = None;
        self.notice = None;
        {
            let mut buffer = lock(&self.buffer);
            buffer.clear();
            buffer.clear_audio_tracks();
        }

        let mut display = self.effective_display();
        if let DisplaySelection::Monitor(id) = &display
            && !self.monitors.iter().any(|m| &m.id == id)
        {
            self.notice = Some(format!(
                "Display {id} is not connected, capturing the primary display instead."
            ));
            display = DisplaySelection::Primary;
        }

        let mut candidates = vec![self.preferred.clone()];
        candidates.extend(
            self.backend
                .available_encoders()
                .iter()
                .filter(|e| **e != self.preferred)
                .cloned(),
        );

        let mut failures = Vec::new();
        let mut index = 0;
        while index < candidates.len() {
            let candidate = candidates[index].clone();
            let settings = self.capture_settings(candidate.clone(), display.clone());
            let sink: Arc<dyn FrameSink> = self.sink.clone();
            match self.backend.start(&settings, sink) {
                Ok(()) => {
                    if candidate != self.preferred {
                        self.add_notice(format!(
                            "{} could not start, using {} instead.",
                            self.preferred.kind.label(),
                            candidate.kind.label()
                        ));
                    }
                    self.active = candidate;
                    return Ok(());
                }
                Err(CaptureError::AudioSource { key, message }) => {
                    warn!("audio source {key} failed to start: {message}");
                    self.unavailable_audio.insert(key.clone());
                    self.add_notice(format!(
                        "Audio device {} is unavailable, capturing without it.",
                        self.audio_source_name(&key)
                    ));
                }
                Err(CaptureError::EncoderStart { encoder, reason }) => {
                    warn!("encoder {encoder} failed to start: {reason}");
                    failures.push(format!("{} ({reason})", candidate.kind.label()));
                    index += 1;
                }
                Err(other) => return Err(other.into()),
            }
        }
        Err(CaptureError::AllEncodersFailed(failures.join("; ")).into())
    }

    fn add_notice(&mut self, text: String) {
        self.notice = Some(match self.notice.take() {
            Some(existing) => format!("{existing} {text}"),
            None => text,
        });
    }

    fn audio_source_name(&self, key: &str) -> String {
        self.config
            .audio
            .sources
            .iter()
            .find(|s| audio_source_key(s.kind, &s.id) == key)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| key.to_owned())
    }

    /// Applies a new configuration: live limits and levels immediately,
    /// pipeline settings through a restart (deferred while recording).
    /// Returns whether the hotkeys must be re-registered.
    pub fn apply_config(&mut self, next: Config) -> Result<bool, AppError> {
        let previous = std::mem::replace(&mut self.config, next);
        let hotkeys = previous.hotkeys_changed(&self.config);

        if previous.replay_limits_changed(&self.config) {
            lock(&self.buffer).set_limits(ReplayLimits {
                max_duration: self.effective_replay_length(),
                max_bytes: self.config.replay_memory_cap_bytes(),
            });
        }
        if previous.capture.encoder != self.config.capture.encoder
            && let Some(encoder) = choose_encoder(
                self.backend.available_encoders(),
                self.config.capture.encoder,
            )
        {
            self.preferred = encoder.clone();
        }
        if previous.capture_restart_needed(&self.config) {
            self.unavailable_audio.clear();
            if self.backend.is_running() {
                if self.recording_wanted {
                    self.restart_pending = true;
                    self.notice =
                        Some("Capture settings apply when the current recording stops.".to_owned());
                } else {
                    self.restart_capture()?;
                }
            }
        } else if previous.audio_levels_changed(&self.config) {
            self.apply_audio_levels();
        }
        Ok(hotkeys)
    }

    fn apply_audio_levels(&self) {
        for source in &self.config.audio.sources {
            let key = audio_source_key(source.kind, &source.id);
            self.backend
                .set_audio_level(&key, source.volume, source.muted);
        }
    }

    /// Re-enumerates displays. Returns true when the set changed. A running
    /// capture of a display that disappeared is restarted on the primary.
    pub fn refresh_monitors(&mut self) -> bool {
        let Ok(current) = self.backend.list_monitors() else {
            return false;
        };
        if current == self.monitors {
            return false;
        }
        info!("display set changed: {} display(s)", current.len());
        self.monitors = current;
        if let DisplaySelection::Monitor(id) = &self.config.capture.display
            && self.backend.is_running()
            && !self.monitors.iter().any(|m| &m.id == id)
            && !self.recording_wanted
            && let Err(err) = self.restart_capture()
        {
            self.last_failure = Some(err.to_string());
        }
        true
    }

    /// Reports the current state, retiring a failed capture so the UI can
    /// show the failure and the user can retry. A failed audio device is
    /// dropped and capture restarts without it.
    pub fn status(&mut self) -> EngineStatus {
        let failure = lock(&self.sink.failure).take();
        if let Some(failure) = failure {
            warn!("capture failed: {failure}");
            self.backend.stop();
            match failure {
                CaptureError::AudioSource { key, .. } if !self.recording_wanted => {
                    self.unavailable_audio.insert(key.clone());
                    let name = self.audio_source_name(&key);
                    match self.restart_capture() {
                        Ok(()) => self.add_notice(format!(
                            "Audio device {name} stopped working, capturing without it."
                        )),
                        Err(err) => self.last_failure = Some(err.to_string()),
                    }
                }
                CaptureError::Pipeline { message, .. }
                    if self.wants_capture() && self.allow_restart() =>
                {
                    match self.restart_capture() {
                        Ok(()) => self
                            .add_notice(format!("Capture restarted after an error ({message}).")),
                        Err(err) => self.last_failure = Some(err.to_string()),
                    }
                }
                other => self.last_failure = Some(other.to_string()),
            }
        }
        let buffer_state = match (&self.last_failure, self.is_buffering()) {
            (Some(failure), _) => BufferState::Failed(failure.clone()),
            (None, true) => BufferState::Running,
            (None, false) => BufferState::Stopped,
        };
        let recording = if self.finishing {
            RecordingState::Finishing
        } else {
            match &*lock(&self.sink.recording) {
                SinkRecording::Idle => RecordingState::Idle,
                SinkRecording::Pending(_) => RecordingState::Starting,
                SinkRecording::Active(session, first, last) => RecordingState::Active {
                    path: session.path().to_path_buf(),
                    duration: last.saturating_sub(*first),
                },
                SinkRecording::Failed(reason) => RecordingState::Failed(reason.clone()),
            }
        };
        let buffer = lock(&self.buffer);
        let stats = buffer.stats();
        if stats.looks_blank && !self.blank_warned && self.is_capturing() {
            warn!("capture looks blank");
            self.blank_warned = true;
        } else if !stats.looks_blank {
            self.blank_warned = false;
        }
        EngineStatus {
            buffer: buffer_state,
            recording,
            stats,
            stream: buffer.stream().cloned(),
            audio_tracks: buffer.audio_tracks().len(),
            encoder: self.active.clone(),
            backend: self.backend.name(),
            replay_length: self.effective_replay_length(),
            notice: self.notice.clone(),
            blank: self.blank_warned,
        }
    }

    fn wants_capture(&self) -> bool {
        self.buffer_wanted || self.auto_buffer || self.recording_wanted
    }

    /// At most three automatic restarts per minute; after that the failure
    /// is surfaced and the user decides.
    fn allow_restart(&mut self) -> bool {
        let now = Instant::now();
        while self
            .restarts
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
        {
            self.restarts.pop_front();
        }
        if self.restarts.len() >= 3 {
            return false;
        }
        self.restarts.push_back(now);
        true
    }

    /// Snapshots the buffer immediately and writes the clip on a worker
    /// thread so the hotkey never blocks the UI or the capture.
    pub fn save_clip(&self, done: SaveCallback) {
        let wanted = self.effective_replay_length();
        let snapshot = lock(&self.buffer).snapshot_last(wanted);
        let Some(snapshot) = snapshot else {
            done(Err(CaptureError::EmptyBuffer.to_string()));
            return;
        };
        if snapshot.truncated {
            info!(
                "buffer holds {:.1} s, less than the requested {:.1} s",
                snapshot.duration.as_secs_f64(),
                wanted.as_secs_f64()
            );
        }

        let game = self.game_name();
        let file_name = clip_file_name(&self.config.output.file_name_pattern, &game, &now_local());
        let path = unique_path(&self.output_dir(self.clips_dir()), &file_name);
        let writer = self.writer.clone();
        let game = (!game.is_empty()).then_some(game);
        spawn_named("clip-writer", move || {
            let result = writer
                .write_clip(&snapshot, &path)
                .map(|mut clip| {
                    clip.game = game;
                    clip
                })
                .map_err(|e| e.to_string());
            if let Err(err) = &result {
                error!("{err}");
            }
            done(result);
        });
    }
}

fn spawn_named(name: &str, work: impl FnOnce() + Send + 'static) {
    if let Err(err) = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
    {
        error!("could not spawn the {name} thread: {err}");
    }
}

pub fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn now_local() -> LocalDateTime {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    LocalDateTime {
        year: now.year(),
        month: now.month(),
        day: now.day(),
        hour: now.hour(),
        minute: now.minute(),
        second: now.second(),
    }
}
