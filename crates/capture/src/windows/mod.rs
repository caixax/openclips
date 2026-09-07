//! Windows backend built on GStreamer: DXGI desktop duplication through
//! `d3d11screencapturesrc`, hardware encoding on the D3D11 device, WASAPI
//! audio capture, and MP4 muxing for clips and recordings.

mod audio;
mod encoders;
mod game_capture;
mod icons;
mod media;
mod monitors;
mod mux;
mod pipeline;
mod player;
mod processes;
mod props;
mod recording;
mod trim;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use gstreamer as gst;
use openclips_core::capture::{AudioDeviceInfo, CaptureSettings, EncoderInfo, MonitorInfo};
use tracing::{error, info, warn};

use crate::backend::{
    CaptureBackend, ClipWriter, FrameSink, IconExtractor, MediaTools, Player, PlayerSink,
    ProcessWatcher, Recorder,
};
use crate::error::CaptureError;

const START_ATTEMPTS: u32 = 3;
const START_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// A start running on its worker thread. Dropping the flag high and joining
/// ends it early.
struct Starting {
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

pub struct WindowsBackend {
    encoders: Vec<EncoderInfo>,
    /// Filled by the start thread once the pipeline delivers frames.
    capture: Arc<Mutex<Option<pipeline::CapturePipeline>>>,
    starting: Option<Starting>,
    /// The signed OBS hook binaries, located once. `None` means game
    /// capture is unavailable on this install.
    hooks: Option<game_capture::Hooks>,
    writer: Arc<mux::Mp4Writer>,
    recorder: Arc<recording::Mp4Recorder>,
    tools: Arc<media::GstMediaTools>,
    processes: Arc<processes::ToolHelpWatcher>,
    icons: Arc<icons::ShellIconExtractor>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// Starts the pipeline, retrying an encoder that refuses to open a session.
/// NVENC occasionally refuses and succeeds moments later, so a start that
/// fails inside the encoder is retried before the caller moves on to
/// another encoder.
fn start_pipeline(
    settings: &CaptureSettings,
    sink: Arc<dyn FrameSink>,
    hooks: Option<&game_capture::Hooks>,
    cancel: &AtomicBool,
) -> Result<pipeline::CapturePipeline, CaptureError> {
    let attempts = if settings.encoder.kind.is_hardware() {
        START_ATTEMPTS
    } else {
        1
    };
    let mut last = None;
    for attempt in 1..=attempts {
        if cancel.load(Ordering::SeqCst) {
            return Err(CaptureError::Cancelled);
        }
        match pipeline::CapturePipeline::start(settings, sink.clone(), hooks, cancel) {
            Ok(capture) => return Ok(capture),
            Err(err @ CaptureError::EncoderStart { .. }) if attempt < attempts => {
                warn!("{err}; retrying ({attempt}/{attempts})");
                std::thread::sleep(START_RETRY_DELAY);
                last = Some(err);
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or(CaptureError::NoEncoder))
}

impl WindowsBackend {
    pub fn new() -> Result<Self, CaptureError> {
        gst::init().map_err(|e| CaptureError::FrameworkInit(e.to_string()))?;
        info!("GStreamer {} initialized", gst::version_string());

        for element in [
            "d3d11screencapturesrc",
            "d3d11convert",
            "videorate",
            "h264parse",
            "mp4mux",
            "appsink",
            "appsrc",
            "wasapi2src",
            "audiomixer",
            "aacparse",
        ] {
            if gst::ElementFactory::find(element).is_none() {
                return Err(CaptureError::MissingElement(element.to_owned()));
            }
        }
        if audio::choose_encoder().is_none() {
            return Err(CaptureError::NoAudioEncoder);
        }

        let encoders = encoders::discover();
        if encoders.is_empty() {
            return Err(CaptureError::NoEncoder);
        }
        info!(
            "registered encoders: {}",
            encoders
                .iter()
                .map(|e| format!("{} ({})", e.kind.label(), e.element))
                .collect::<Vec<_>>()
                .join(", ")
        );

        let hooks = match game_capture::Hooks::locate() {
            Ok(hooks) => Some(hooks),
            Err(err) => {
                info!("game capture unavailable: {err}");
                None
            }
        };

        Ok(Self {
            encoders,
            capture: Arc::new(Mutex::new(None)),
            starting: None,
            hooks,
            writer: Arc::new(mux::Mp4Writer),
            recorder: Arc::new(recording::Mp4Recorder),
            tools: Arc::new(media::GstMediaTools),
            processes: Arc::new(processes::ToolHelpWatcher),
            icons: Arc::new(icons::ShellIconExtractor),
        })
    }

    /// Ends a start in flight, if any, and waits for its thread. The start
    /// polls the flag every 100 ms at most, so this is quick.
    fn cancel_start(&mut self) {
        if let Some(starting) = self.starting.take() {
            starting.cancel.store(true, Ordering::SeqCst);
            let _ = starting.thread.join();
        }
    }
}

impl CaptureBackend for WindowsBackend {
    fn name(&self) -> &'static str {
        "Windows (GStreamer, D3D11)"
    }

    fn available_encoders(&self) -> &[EncoderInfo] {
        &self.encoders
    }

    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        Ok(monitors::enumerate().into_iter().map(|m| m.info).collect())
    }

    fn list_audio_devices(&self) -> Result<Vec<AudioDeviceInfo>, CaptureError> {
        audio::list_devices()
    }

    fn start(
        &mut self,
        settings: &CaptureSettings,
        sink: Arc<dyn FrameSink>,
    ) -> Result<(), CaptureError> {
        if self.is_running() || self.is_starting() {
            return Err(CaptureError::AlreadyRunning);
        }
        let never = AtomicBool::new(false);
        let capture = start_pipeline(settings, sink, self.hooks.as_ref(), &never)?;
        *lock(&self.capture) = Some(capture);
        Ok(())
    }

    fn start_in_background(
        &mut self,
        settings: &CaptureSettings,
        sink: Arc<dyn FrameSink>,
        done: Box<dyn FnOnce(Result<(), CaptureError>) + Send + 'static>,
    ) {
        if self.is_running() || self.is_starting() {
            done(Err(CaptureError::AlreadyRunning));
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let settings = settings.clone();
        let hooks = self.hooks.clone();
        let slot = self.capture.clone();
        let flag = cancel.clone();
        // Shared so a failed spawn can still report; the closure owns it
        // otherwise and is dropped unrun in that case.
        let done = Arc::new(Mutex::new(Some(done)));
        let report = done.clone();
        let spawned = std::thread::Builder::new()
            .name("capture-start".to_owned())
            .spawn(move || {
                let result = start_pipeline(&settings, sink, hooks.as_ref(), &flag);
                let outcome = match result {
                    // Stopped while the first frame was on its way: the
                    // pipeline goes, the caller asked for nothing to run.
                    Ok(capture) if flag.load(Ordering::SeqCst) => {
                        capture.stop();
                        Err(CaptureError::Cancelled)
                    }
                    Ok(capture) => {
                        *lock(&slot) = Some(capture);
                        Ok(())
                    }
                    Err(err) => Err(err),
                };
                if let Some(done) = lock(&report).take() {
                    done(outcome);
                }
            });
        match spawned {
            Ok(thread) => self.starting = Some(Starting { cancel, thread }),
            Err(err) => {
                error!("could not spawn the capture start thread: {err}");
                if let Some(done) = lock(&done).take() {
                    done(Err(CaptureError::PipelineBuild(format!(
                        "could not spawn the capture start thread: {err}"
                    ))));
                }
            }
        }
    }

    fn stop(&mut self) {
        self.cancel_start();
        if let Some(capture) = lock(&self.capture).take() {
            capture.stop();
        }
    }

    fn is_running(&self) -> bool {
        lock(&self.capture).is_some()
    }

    fn is_starting(&self) -> bool {
        self.starting
            .as_ref()
            .is_some_and(|s| !s.thread.is_finished())
    }

    fn game_capture_available(&self) -> bool {
        self.hooks.is_some()
    }

    fn set_audio_level(&self, source_key: &str, volume: f32, muted: bool) -> bool {
        lock(&self.capture)
            .as_ref()
            .is_some_and(|c| c.set_audio_level(source_key, volume, muted))
    }

    fn clip_writer(&self) -> Arc<dyn ClipWriter> {
        self.writer.clone()
    }

    fn recorder(&self) -> Arc<dyn Recorder> {
        self.recorder.clone()
    }

    fn media_tools(&self) -> Arc<dyn MediaTools> {
        self.tools.clone()
    }

    fn create_player(&self, sink: Arc<dyn PlayerSink>) -> Result<Box<dyn Player>, CaptureError> {
        Ok(Box::new(player::GstPlayer::new(sink)?))
    }

    fn process_watcher(&self) -> Arc<dyn ProcessWatcher> {
        self.processes.clone()
    }

    fn icon_extractor(&self) -> Arc<dyn IconExtractor> {
        self.icons.clone()
    }
}

impl Drop for WindowsBackend {
    fn drop(&mut self) {
        self.stop();
    }
}
