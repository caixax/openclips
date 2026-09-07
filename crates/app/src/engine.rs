//! Connects the capture backend to the replay buffer and the session
//! recorder, and turns hotkey presses into clip files. Owned by the UI
//! thread; backend threads only touch the shared sink state.

use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
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
use openclips_core::config::{AppPaths, CaptureMethod, Config, DisplaySelection};
use openclips_core::games::{AutoCapture, DetectedGame};
use openclips_core::media::{AudioPacket, AudioTrackInfo, EncodedFrame, StreamInfo, Timestamp};
use openclips_core::replay::{ReplayBuffer, ReplayLimits, ReplayStats};
use tracing::{error, info, warn};

use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferState {
    Stopped,
    /// Capture is being brought up on a worker thread.
    Starting,
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
/// Told about every recording file closed on its own (see
/// [`CaptureSink::rotate_recording`]), from a worker thread.
pub type RecordingListener = Arc<dyn Fn(Result<ClipFile, String>) + Send + Sync>;
/// Outcome of one background start attempt, tagged with its generation.
type StartResult = (u64, Result<(), CaptureError>);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum SinkRecording {
    Idle,
    Pending(PathBuf),
    Active {
        session: Box<dyn RecordingSession>,
        first: Timestamp,
        last: Timestamp,
    },
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
    listener: Mutex<Option<RecordingListener>>,
}

impl CaptureSink {
    /// A new stream cannot continue in the open file: after a capture
    /// restart the timestamps start over, and after a mode change the muxer
    /// cannot switch caps. The file is closed as it is and the next keyframe
    /// opens a numbered sibling, so a display change mid recording costs a
    /// cut, not the recording.
    fn rotate_recording(&self) {
        let mut slot = lock(&self.recording);
        if !matches!(&*slot, SinkRecording::Active { .. }) {
            return;
        }
        let state = std::mem::replace(&mut *slot, SinkRecording::Idle);
        let SinkRecording::Active { session, .. } = state else {
            return;
        };
        let next = next_part_path(session.path());
        info!(
            "stream changed while recording, closing {} and continuing in {}",
            session.path().display(),
            next.display()
        );
        *slot = SinkRecording::Pending(next);
        let listener = lock(&self.listener).clone();
        spawn_named("recording-rotate", move || {
            let result = session.finish().map_err(|e| e.to_string());
            match listener {
                Some(listener) => listener(result),
                None => {
                    if let Err(err) = result {
                        error!("could not close the rotated recording: {err}");
                    }
                }
            }
        });
    }

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
                        *slot = SinkRecording::Active {
                            session,
                            first: frame.pts,
                            last: frame.pts,
                        };
                    }
                    Err(err) => *slot = SinkRecording::Failed(err.to_string()),
                }
            }
            SinkRecording::Active { session, last, .. } => {
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
        if let SinkRecording::Active { session, .. } = &mut *slot
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
        self.rotate_recording();
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

/// One capture start in progress: the encoders still to try, in order, and
/// what failed so far. Each attempt runs on the backend's worker thread and
/// reports back through `start_results`; the engine picks the next step from
/// the UI thread (see [`Engine::poll_start`]).
struct StartPlan {
    candidates: Vec<EncoderInfo>,
    index: usize,
    display: DisplaySelection,
    failures: Vec<String>,
    /// Tells a stale result (from an attempt that was cancelled and
    /// replaced) from the one this plan is waiting for.
    generation: u64,
}

impl StartPlan {
    fn candidate(&self) -> Option<&EncoderInfo> {
        self.candidates.get(self.index)
    }
}

pub struct Engine {
    backend: Box<dyn CaptureBackend>,
    writer: Arc<dyn ClipWriter>,
    buffer: Arc<Mutex<ReplayBuffer>>,
    sink: Arc<CaptureSink>,
    config: Config,
    paths: AppPaths,
    starting: Option<StartPlan>,
    start_results: (Sender<StartResult>, Receiver<StartResult>),
    start_generation: u64,
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
    /// Process ids of the application audio sources the capture started with.
    app_audio: Vec<(String, u32)>,
    notice: Option<String>,
    last_failure: Option<String>,
    monitors: Vec<MonitorInfo>,
    /// Timestamps of automatic restarts after capture errors, to cap them.
    restarts: VecDeque<Instant>,
    blank_warned: bool,
    /// Game capture failed this session (or for this game); fall back to
    /// display until the game or the settings change.
    game_capture_unavailable: bool,
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
            listener: Mutex::new(None),
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
            starting: None,
            start_results: channel(),
            start_generation: 0,
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
            app_audio: Vec::new(),
            notice: None,
            last_failure: None,
            monitors,
            restarts: VecDeque::new(),
            blank_warned: false,
            game_capture_unavailable: false,
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

    /// Receives every recording file that is closed without the user asking
    /// (a display change or a capture restart mid recording). Called from a
    /// worker thread; it must not touch the engine directly.
    pub fn set_recording_listener(
        &self,
        listener: impl Fn(Result<ClipFile, String>) + Send + Sync + 'static,
    ) {
        *lock(&self.sink.listener) = Some(Arc::new(listener));
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

    /// The capture method for the active game, its per game override taking
    /// precedence over the global default.
    fn effective_capture_method(&self) -> CaptureMethod {
        self.active_game
            .as_ref()
            .and_then(|g| g.profile.as_ref())
            .and_then(|p| p.capture_method)
            .unwrap_or(self.config.capture.method)
    }

    /// The process id to hook, when game capture is chosen and possible.
    /// `None` selects display capture (no active game, no hooks, or a prior
    /// failure this session).
    fn game_capture_pid(&self) -> Option<u32> {
        if self.game_capture_unavailable
            || self.effective_capture_method() != CaptureMethod::Game
            || !self.backend.game_capture_available()
        {
            return None;
        }
        self.active_game.as_ref().map(|g| g.pid)
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
        let previous_pid = self.game_capture_pid();
        let game_changed =
            active.as_ref().map(|g| &g.exe) != self.active_game.as_ref().map(|g| &g.exe);
        self.active_game = active;
        if game_changed {
            // A new game gets a fresh attempt at game capture.
            self.game_capture_unavailable = false;
            if let Some(game) = &self.active_game {
                info!("active game: {} ({})", game.name, game.exe);
            }
            lock(&self.buffer).set_limits(ReplayLimits {
                max_duration: self.effective_replay_length(),
                max_bytes: self.config.replay_memory_cap_bytes(),
            });
            // Restart when the source changes: a different display, or a
            // different game capture target (which display comparison misses).
            let source_changed = self.effective_display() != previous_display
                || self.game_capture_pid() != previous_pid;
            if source_changed && self.is_engaged() && !self.recording_wanted {
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
        let starting_for_buffer =
            self.starting.is_some() && (self.buffer_wanted || self.auto_buffer);
        if self.is_buffering() || starting_for_buffer {
            self.stop_buffer();
            Ok(())
        } else {
            self.start_buffer()
        }
    }

    pub fn is_capturing(&self) -> bool {
        self.backend.is_running() && lock(&self.sink.failure).is_none()
    }

    /// Capture is running or on its way up: a change of source or settings
    /// needs a restart either way.
    fn is_engaged(&self) -> bool {
        self.backend.is_running() || self.starting.is_some()
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
            SinkRecording::Active { session, .. } => {
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
        if self.is_capturing() || self.starting.is_some() {
            return Ok(());
        }
        self.stop_backend();
        self.start_capture()
    }

    /// Stops the capture, or cancels the start in flight, and forgets the
    /// plan behind it. A cancelled attempt still reports back; its
    /// generation no longer matches and the result is dropped.
    fn stop_backend(&mut self) {
        self.backend.stop();
        self.starting = None;
    }

    fn release_capture_if_unused(&mut self) {
        if !self.buffer_wanted && !self.auto_buffer && !self.recording_wanted {
            self.stop_backend();
        }
    }

    fn restart_capture(&mut self) -> Result<(), AppError> {
        self.stop_backend();
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
        settings.game_capture_pid = self.game_capture_pid();
        for track in &mut settings.audio_tracks {
            track
                .sources
                .retain(|s| !self.unavailable_audio.contains(&s.key()));
        }
        let pids = self.app_pids();
        let first_app = pids
            .iter()
            .map(|(_, pid)| *pid)
            .find(|pid| *pid != 0)
            .unwrap_or(0);
        for track in &mut settings.audio_tracks {
            for source in &mut track.sources {
                match source.kind {
                    openclips_core::capture::AudioDeviceKind::Application => {
                        source.process = pids
                            .iter()
                            .find(|(id, _)| *id == source.id)
                            .map(|(_, pid)| *pid)
                            .unwrap_or(0);
                    }
                    openclips_core::capture::AudioDeviceKind::Output
                        if source.id == openclips_core::capture::DEFAULT_AUDIO_DEVICE_ID =>
                    {
                        source.process = first_app;
                    }
                    _ => {}
                }
            }
            track.sources.retain(|s| {
                s.kind != openclips_core::capture::AudioDeviceKind::Application || s.process != 0
            });
        }
        settings.audio_tracks.retain(|t| !t.sources.is_empty());
        settings
    }

    /// Process ids of the enabled application audio sources, zero when the
    /// application is not running.
    fn app_pids(&self) -> Vec<(String, u32)> {
        let apps: Vec<&openclips_core::config::AudioSourceConfig> = self
            .config
            .audio
            .sources
            .iter()
            .filter(|s| {
                s.enabled && s.kind == openclips_core::capture::AudioDeviceKind::Application
            })
            .collect();
        if apps.is_empty() || !self.config.audio.enabled {
            return Vec::new();
        }
        let running = self.backend.process_watcher().running().unwrap_or_default();
        apps.iter()
            .map(|s| {
                let exe = s.id.to_lowercase();
                let pid = running
                    .iter()
                    .find(|p| p.exe == exe)
                    .map(|p| p.pid)
                    .unwrap_or(0);
                (s.id.clone(), pid)
            })
            .collect()
    }

    /// Restarts the capture when an application with its own audio track
    /// starts or stops, so the track appears or disappears. Deferred while
    /// recording.
    pub fn poll_app_audio(&mut self) -> Result<(), AppError> {
        if self.app_audio.is_empty()
            && !self.config.audio.sources.iter().any(|s| {
                s.enabled && s.kind == openclips_core::capture::AudioDeviceKind::Application
            })
        {
            return Ok(());
        }
        let now = self.app_pids();
        if now != self.app_audio && self.is_engaged() && !self.recording_wanted {
            info!("application audio changed, restarting capture");
            self.restart_capture()?;
        }
        Ok(())
    }

    /// Starts capture with the preferred encoder and falls back through the
    /// remaining registered encoders when one refuses to start. An audio
    /// source that fails is dropped and the start is retried without it.
    /// Both cases are reported through the status notice. The work happens
    /// on the backend's thread; this only sets the plan in motion, and a
    /// failure surfaces through [`Engine::status`] once every step is done.
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
        self.starting = Some(StartPlan {
            candidates,
            index: 0,
            display,
            failures: Vec::new(),
            generation: 0,
        });
        self.launch_attempt();
        Ok(())
    }

    /// Hands the current candidate of the plan to the backend.
    fn launch_attempt(&mut self) {
        self.start_generation += 1;
        let generation = self.start_generation;
        let Some(plan) = self.starting.as_mut() else {
            return;
        };
        plan.generation = generation;
        let Some(candidate) = plan.candidate().cloned() else {
            let failures = plan.failures.join("; ");
            self.starting = None;
            self.last_failure = Some(CaptureError::AllEncodersFailed(failures).to_string());
            return;
        };
        let display = plan.display.clone();
        let settings = self.capture_settings(candidate, display);
        let sink: Arc<dyn FrameSink> = self.sink.clone();
        let results = self.start_results.0.clone();
        self.backend.start_in_background(
            &settings,
            sink,
            Box::new(move |result| {
                let _ = results.send((generation, result));
            }),
        );
    }

    /// Applies the outcome of the attempts that finished since the last
    /// call and launches the next step of the plan when there is one.
    /// Called from [`Engine::status`], which the UI polls.
    pub fn poll_start(&mut self) {
        while let Ok((generation, result)) = self.start_results.1.try_recv() {
            let current = self.starting.as_ref().map(|p| p.generation);
            if current != Some(generation) {
                continue;
            }
            self.attempt_finished(result);
        }
    }

    fn attempt_finished(&mut self, result: Result<(), CaptureError>) {
        let Some(plan) = self.starting.as_mut() else {
            return;
        };
        let Some(candidate) = plan.candidate().cloned() else {
            self.starting = None;
            return;
        };
        match result {
            Ok(()) => {
                self.starting = None;
                self.app_audio = self.app_pids();
                if candidate != self.preferred {
                    self.add_notice(format!(
                        "{} could not start, using {} instead.",
                        self.preferred.kind.label(),
                        candidate.kind.label()
                    ));
                }
                self.active = candidate;
            }
            Err(CaptureError::AudioSource { key, message }) => {
                warn!("audio source {key} failed to start: {message}");
                self.unavailable_audio.insert(key.clone());
                self.add_notice(format!(
                    "Audio device {} is unavailable, capturing without it.",
                    self.audio_source_name(&key)
                ));
                self.launch_attempt();
            }
            Err(CaptureError::EncoderStart { encoder, reason }) => {
                warn!("encoder {encoder} failed to start: {reason}");
                plan.failures
                    .push(format!("{} ({reason})", candidate.kind.label()));
                plan.index += 1;
                self.launch_attempt();
            }
            // Game capture could not attach: fall back to display capture
            // for this session and retry the same encoder without the hook.
            Err(CaptureError::GameCapture(message)) if !self.game_capture_unavailable => {
                warn!("game capture failed to start: {message}");
                self.game_capture_unavailable = true;
                self.add_notice(format!(
                    "Game capture failed ({message}), using display capture."
                ));
                self.launch_attempt();
            }
            Err(CaptureError::Cancelled) => {
                self.starting = None;
            }
            Err(other) => {
                self.starting = None;
                self.last_failure = Some(other.to_string());
            }
        }
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
            // Changed capture settings get a fresh attempt at game capture.
            self.game_capture_unavailable = false;
            if self.is_engaged() {
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
            && self.is_engaged()
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
        self.poll_start();
        let failure = lock(&self.sink.failure).take();
        if let Some(failure) = failure {
            warn!("capture failed: {failure}");
            self.stop_backend();
            match failure {
                // While recording, the restart rotates the file (see
                // `CaptureSink::rotate_recording`), so the recording goes on
                // in a new one instead of stalling.
                CaptureError::AudioSource { key, .. } => {
                    self.unavailable_audio.insert(key.clone());
                    let name = self.audio_source_name(&key);
                    match self.restart_capture() {
                        Ok(()) => self.add_notice(format!(
                            "Audio device {name} stopped working, capturing without it."
                        )),
                        Err(err) => self.last_failure = Some(err.to_string()),
                    }
                }
                // Game capture could not start or the hook died: fall back to
                // display capture for the rest of this session.
                CaptureError::GameCapture(message) if self.wants_capture() => {
                    self.game_capture_unavailable = true;
                    match self.restart_capture() {
                        Ok(()) => self.add_notice(format!(
                            "Game capture failed ({message}), using display capture."
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
        let starting_for_buffer =
            self.starting.is_some() && (self.buffer_wanted || self.auto_buffer);
        let buffer_state = match (&self.last_failure, self.is_buffering()) {
            (Some(failure), _) => BufferState::Failed(failure.clone()),
            (None, true) => BufferState::Running,
            (None, false) if starting_for_buffer => BufferState::Starting,
            (None, false) => BufferState::Stopped,
        };
        let recording = if self.finishing {
            RecordingState::Finishing
        } else {
            match &*lock(&self.sink.recording) {
                SinkRecording::Idle => RecordingState::Idle,
                SinkRecording::Pending(_) => RecordingState::Starting,
                SinkRecording::Active {
                    session,
                    first,
                    last,
                } => RecordingState::Active {
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
    /// thread so the hotkey never blocks the UI or the capture. `length`
    /// limits how far back the clip reaches; `None` or zero saves the whole
    /// buffer.
    pub fn save_clip(&self, length: Option<Duration>, done: SaveCallback) {
        let full = self.effective_replay_length();
        let wanted = length.filter(|l| !l.is_zero()).unwrap_or(full).min(full);
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
        let path = unique_path(
            &self.output_dir(self.config.clips_out_dir(&self.paths)),
            &file_name,
        );
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

/// The next `<name> (n).mp4` next to `path` whose final and partial files
/// are both free. A previous ` (n)` on the name is replaced, not stacked.
fn next_part_path(path: &Path) -> PathBuf {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Recording");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("mp4");
    let base = stem
        .rsplit_once(" (")
        .filter(|(_, n)| {
            n.strip_suffix(')')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(|(base, _)| base)
        .unwrap_or(stem);
    (2..)
        .map(|n| dir.join(format!("{base} ({n}).{ext}")))
        .find(|p| !p.exists() && !p.with_extension(format!("{ext}.part")).exists())
        .unwrap_or_else(|| path.to_path_buf())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_part_path_skips_taken_names_and_replaces_the_counter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("Game 2026-09-07 21-05-09.mp4");
        assert_eq!(
            next_part_path(&first),
            dir.path().join("Game 2026-09-07 21-05-09 (2).mp4")
        );
        std::fs::write(
            dir.path().join("Game 2026-09-07 21-05-09 (2).mp4.part"),
            b"x",
        )
        .expect("write");
        assert_eq!(
            next_part_path(&first),
            dir.path().join("Game 2026-09-07 21-05-09 (3).mp4")
        );
        let second = dir.path().join("Game 2026-09-07 21-05-09 (2).mp4");
        assert_eq!(
            next_part_path(&second),
            dir.path().join("Game 2026-09-07 21-05-09 (3).mp4")
        );
        let odd = dir.path().join("Clip (final).mp4");
        assert_eq!(
            next_part_path(&odd),
            dir.path().join("Clip (final) (2).mp4")
        );
    }
}
