//! Connects the capture backend to the replay buffer and turns hotkey
//! presses into clip files. Owned by the UI thread; backend threads only
//! touch the shared buffer.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use openclips_capture::{CaptureBackend, CaptureError, ClipWriter, FrameSink};
use openclips_core::capture::{CaptureSettings, EncoderInfo, choose_encoder};
use openclips_core::clip::{ClipFile, LocalDateTime, clip_file_name, unique_path};
use openclips_core::config::Config;
use openclips_core::media::{EncodedFrame, StreamInfo};
use openclips_core::replay::{ReplayBuffer, ReplayLimits, ReplayStats};
use tracing::{error, info, warn};

use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferState {
    Stopped,
    Running,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct EngineStatus {
    pub state: BufferState,
    pub stats: ReplayStats,
    pub stream: Option<StreamInfo>,
    pub encoder: EncoderInfo,
    pub backend: &'static str,
    pub replay_length: Duration,
    /// Non fatal information such as an encoder fallback.
    pub notice: Option<String>,
}

pub type SaveCallback = Box<dyn FnOnce(Result<ClipFile, String>) + Send + 'static>;

struct BufferSink {
    buffer: Arc<Mutex<ReplayBuffer>>,
    failure: Mutex<Option<String>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl FrameSink for BufferSink {
    fn on_stream(&self, info: StreamInfo) {
        lock(&self.buffer).set_stream(info);
    }

    fn on_frame(&self, frame: EncodedFrame) {
        lock(&self.buffer).push(frame);
    }

    fn on_error(&self, error: CaptureError) {
        *lock(&self.failure) = Some(error.to_string());
    }
}

pub struct Engine {
    backend: Box<dyn CaptureBackend>,
    writer: Arc<dyn ClipWriter>,
    buffer: Arc<Mutex<ReplayBuffer>>,
    sink: Arc<BufferSink>,
    config: Config,
    clips_dir: PathBuf,
    /// The encoder the user asked for (or the best registered one).
    preferred: EncoderInfo,
    /// The encoder actually driving the running or last capture.
    active: EncoderInfo,
    /// Encoders that failed to start this session, never retried first.
    broken: Vec<String>,
    notice: Option<String>,
    last_failure: Option<String>,
}

impl Engine {
    pub fn new(config: Config, clips_dir: PathBuf) -> Result<Self, AppError> {
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
        let sink = Arc::new(BufferSink {
            buffer: buffer.clone(),
            failure: Mutex::new(None),
        });
        let writer = backend.clip_writer();
        Ok(Self {
            backend,
            writer,
            buffer,
            sink,
            config,
            clips_dir,
            active: encoder.clone(),
            preferred: encoder,
            broken: Vec::new(),
            notice: None,
            last_failure: None,
        })
    }

    fn limits(config: &Config) -> ReplayLimits {
        ReplayLimits {
            max_duration: config.replay_length(),
            max_bytes: config.replay_memory_cap_bytes(),
        }
    }

    /// Starts capture with the preferred encoder and falls back through the
    /// remaining registered encoders when one refuses to start. A fallback is
    /// reported through the status notice rather than treated as a failure.
    pub fn start_buffer(&mut self) -> Result<(), AppError> {
        if self.backend.is_running() {
            return Ok(());
        }
        *lock(&self.sink.failure) = None;
        self.last_failure = None;
        lock(&self.buffer).clear();

        let mut candidates = vec![self.preferred.clone()];
        candidates.extend(
            self.backend
                .available_encoders()
                .iter()
                .filter(|e| **e != self.preferred)
                .cloned(),
        );

        let mut failures = Vec::new();
        for candidate in candidates {
            if self.broken.contains(&candidate.element) {
                continue;
            }
            let settings = CaptureSettings::from_config(
                &self.config.capture,
                candidate.clone(),
                self.config.replay.temp_dir.clone(),
            );
            let sink: Arc<dyn FrameSink> = self.sink.clone();
            match self.backend.start(&settings, sink) {
                Ok(()) => {
                    self.notice = (candidate != self.preferred).then(|| {
                        format!(
                            "{} could not start, using {} instead.",
                            self.preferred.kind.label(),
                            candidate.kind.label()
                        )
                    });
                    self.active = candidate;
                    return Ok(());
                }
                Err(CaptureError::EncoderStart { encoder, reason }) => {
                    warn!("encoder {encoder} failed to start: {reason}");
                    failures.push(format!("{} ({reason})", candidate.kind.label()));
                    self.broken.push(encoder);
                }
                Err(other) => return Err(other.into()),
            }
        }
        Err(CaptureError::AllEncodersFailed(failures.join("; ")).into())
    }

    pub fn stop_buffer(&mut self) {
        self.backend.stop();
        lock(&self.buffer).clear();
    }

    pub fn toggle_buffer(&mut self) -> Result<(), AppError> {
        if self.is_buffering() {
            self.stop_buffer();
            Ok(())
        } else {
            self.start_buffer()
        }
    }

    pub fn is_buffering(&self) -> bool {
        self.backend.is_running() && lock(&self.sink.failure).is_none()
    }

    /// Reports the current state, retiring a failed capture so the UI can
    /// show the failure and the user can retry.
    pub fn status(&mut self) -> EngineStatus {
        if let Some(failure) = lock(&self.sink.failure).take() {
            warn!("capture failed: {failure}");
            self.backend.stop();
            self.last_failure = Some(failure);
        }
        let state = match (&self.last_failure, self.backend.is_running()) {
            (Some(failure), _) => BufferState::Failed(failure.clone()),
            (None, true) => BufferState::Running,
            (None, false) => BufferState::Stopped,
        };
        let buffer = lock(&self.buffer);
        EngineStatus {
            state,
            stats: buffer.stats(),
            stream: buffer.stream().cloned(),
            encoder: self.active.clone(),
            backend: self.backend.name(),
            replay_length: self.config.replay_length(),
            notice: self.notice.clone(),
        }
    }

    /// Snapshots the buffer immediately and writes the clip on a worker
    /// thread so the hotkey never blocks the UI or the capture.
    pub fn save_clip(&self, done: SaveCallback) {
        let wanted = self.config.replay_length();
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

        let file_name = clip_file_name(&self.config.output.file_name_pattern, "", &now_local());
        let path = unique_path(&self.clips_dir, &file_name);
        let writer = self.writer.clone();
        let spawned = std::thread::Builder::new()
            .name("clip-writer".to_owned())
            .spawn(move || {
                let result = writer
                    .write_clip(&snapshot, &path)
                    .map_err(|e| e.to_string());
                if let Err(err) = &result {
                    error!("{err}");
                }
                done(result);
            });
        if let Err(err) = spawned {
            error!("could not spawn the clip writer thread: {err}");
        }
    }
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
