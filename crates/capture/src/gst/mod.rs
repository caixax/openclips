//! Everything the backends share: they are all GStreamer, and only the ends
//! of the pipelines differ by platform. The capture lifecycle, the encoder
//! tail, the audio tracks, the clip and recording muxers, the trimmer, the
//! prober and the player live here; a platform module (`windows`, `linux`)
//! implements [`Platform`] to supply the screen source, the audio sources
//! and the operating system services.

pub mod audio;
pub mod encoders;
pub mod media;
pub mod mux;
pub mod pipeline;
pub mod player;
pub mod props;
pub mod recording;
pub mod trim;

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use gstreamer as gst;
use openclips_core::capture::{
    AudioDeviceInfo, AudioSourceSettings, CaptureSettings, EncoderInfo, MonitorInfo,
};
use openclips_core::media::Timestamp;
use tracing::{error, info, warn};

use crate::backend::{
    CaptureBackend, ClipWriter, FrameSink, IconExtractor, MediaTools, Player, PlayerSink,
    ProcessWatcher, Recorder,
};
use crate::error::CaptureError;
use encoders::EncoderSpec;

/// The platform this build targets.
pub type Native = crate::native::Native;

const START_ATTEMPTS: u32 = 3;
const START_RETRY_DELAY: Duration = Duration::from_millis(400);

/// The video side of a capture pipeline up to the encoder: the source, the
/// frame rate grid and whatever conversion leaves frames in a format and
/// memory the chosen encoder takes.
pub struct VideoHead {
    /// In link order; the last element feeds the shared encoder tail.
    pub elements: Vec<gst::Element>,
    /// A context every element of the pipeline should share (a GPU device).
    pub context: Option<gst::Context>,
    /// Kept alive as long as the pipeline runs (a hook session, a portal
    /// session), dropped after it stops.
    pub keepalive: Option<Box<dyn Any + Send>>,
    /// Run the pipeline on the system clock instead of letting it pick one
    /// from its elements. For sources that offer a clock of their own which
    /// the rest of the pipeline cannot follow.
    pub system_clock: bool,
    /// How long the first encoded frame may take.
    pub first_frame_timeout: Duration,
    /// What is being captured, for the log.
    pub description: String,
}

/// What a platform supplies. Everything else is shared.
pub trait Platform: Clone + Send + Sync + Sized + 'static {
    /// Shown as the backend name.
    const NAME: &'static str;
    /// Elements without which nothing works; checked once at start.
    const REQUIRED_ELEMENTS: &'static [&'static str];
    /// H.264 encoders to look for, best first.
    const ENCODERS: &'static [EncoderSpec];
    /// AAC encoders to look for, best first.
    const AAC_ENCODERS: &'static [&'static str];
    /// Whether a start that produced no frame is tried again with a
    /// software encoder too. Hardware encoders always are (they refuse a
    /// session now and then); this is for platforms whose screen source can
    /// lose its first negotiation.
    const RETRY_SOFTWARE_STARTS: bool = false;

    /// Called after GStreamer is initialized.
    fn new() -> Result<Self, CaptureError>;
    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError>;
    fn list_audio_devices(&self) -> Result<Vec<AudioDeviceInfo>, CaptureError>;
    /// A configured source element for one audio source, named `name`.
    fn audio_source(
        &self,
        source: &AudioSourceSettings,
        name: &str,
    ) -> Result<gst::Element, CaptureError>;
    /// Builds the video head for `settings`. `cancel` is polled by heads
    /// that wait (for a game to present, for a permission dialog).
    fn video_head(
        &self,
        settings: &CaptureSettings,
        encoder: EncoderSpec,
        sink: Arc<dyn FrameSink>,
        cancel: &AtomicBool,
    ) -> Result<VideoHead, CaptureError>;
    /// Runs inside every streaming thread of a capture pipeline as it
    /// starts, to raise its scheduling class where the platform has one.
    fn streaming_thread_started() {}
    /// The player's conversion chain: takes decoded frames in whatever
    /// memory the decoder produced and ends in tightly packed RGBA in
    /// system memory, at most `max_width` wide, square pixels.
    fn player_video_chain(max_width: i32) -> Result<Vec<gst::Element>, CaptureError>;
    fn process_watcher(&self) -> Arc<dyn ProcessWatcher>;
    fn icon_extractor(&self) -> Arc<dyn IconExtractor>;
    fn game_capture_available(&self) -> bool {
        false
    }
}

pub(crate) fn make(element: &str) -> Result<gst::Element, CaptureError> {
    gst::ElementFactory::make(element)
        .build()
        .map_err(|_| CaptureError::MissingElement(element.to_owned()))
}

/// A diagnostic switch: `1` turns it on, `0` off, anything else keeps the
/// default.
pub(crate) fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name).as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => default,
    }
}

/// Timestamps from different branches only compare as running time, which
/// accounts for each pad's segment. Falls back to the raw timestamp when a
/// sample carries no segment.
pub(crate) fn running_time(sample: &gst::Sample, pts: Option<gst::ClockTime>) -> Timestamp {
    let pts = pts.unwrap_or(gst::ClockTime::ZERO);
    let running = sample
        .segment()
        .and_then(|segment| segment.downcast_ref::<gst::ClockTime>())
        .and_then(|segment| segment.to_running_time(pts))
        .unwrap_or(pts);
    Timestamp::from_nanos(running.nseconds())
}

/// A start running on its worker thread. Raising the flag and joining ends
/// it early.
struct Starting {
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

pub struct GstBackend {
    native: Native,
    encoders: Vec<EncoderInfo>,
    /// Filled by the start thread once the pipeline delivers frames.
    capture: Arc<Mutex<Option<pipeline::CapturePipeline>>>,
    starting: Option<Starting>,
    writer: Arc<mux::Mp4Writer>,
    recorder: Arc<recording::Mp4Recorder>,
    tools: Arc<media::GstMediaTools>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// Starts the pipeline, retrying an encoder that refuses to open a session.
/// NVENC occasionally refuses and succeeds moments later, so a start that
/// fails inside the encoder is retried before the caller moves on to
/// another encoder.
fn start_pipeline(
    native: &Native,
    settings: &CaptureSettings,
    sink: Arc<dyn FrameSink>,
    cancel: &AtomicBool,
) -> Result<pipeline::CapturePipeline, CaptureError> {
    let attempts = if settings.encoder.kind.is_hardware() || Native::RETRY_SOFTWARE_STARTS {
        START_ATTEMPTS
    } else {
        1
    };
    let mut last = None;
    for attempt in 1..=attempts {
        if cancel.load(Ordering::SeqCst) {
            return Err(CaptureError::Cancelled);
        }
        match pipeline::CapturePipeline::start(native, settings, sink.clone(), cancel) {
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

impl GstBackend {
    pub fn new() -> Result<Self, CaptureError> {
        gst::init().map_err(|e| CaptureError::FrameworkInit(e.to_string()))?;
        info!("GStreamer {} initialized", gst::version_string());

        const SHARED_ELEMENTS: [&str; 7] = [
            "videorate",
            "h264parse",
            "mp4mux",
            "appsink",
            "appsrc",
            "audiomixer",
            "aacparse",
        ];
        for element in Native::REQUIRED_ELEMENTS.iter().chain(&SHARED_ELEMENTS) {
            if gst::ElementFactory::find(element).is_none() {
                return Err(CaptureError::MissingElement((*element).to_owned()));
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

        Ok(Self {
            native: Native::new()?,
            encoders,
            capture: Arc::new(Mutex::new(None)),
            starting: None,
            writer: Arc::new(mux::Mp4Writer),
            recorder: Arc::new(recording::Mp4Recorder),
            tools: Arc::new(media::GstMediaTools),
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

impl CaptureBackend for GstBackend {
    fn name(&self) -> &'static str {
        Native::NAME
    }

    fn available_encoders(&self) -> &[EncoderInfo] {
        &self.encoders
    }

    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        self.native.list_monitors()
    }

    fn list_audio_devices(&self) -> Result<Vec<AudioDeviceInfo>, CaptureError> {
        self.native.list_audio_devices()
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
        let capture = start_pipeline(&self.native, settings, sink, &never)?;
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
        let native = self.native.clone();
        let slot = self.capture.clone();
        let flag = cancel.clone();
        // Shared so a failed spawn can still report; the closure owns it
        // otherwise and is dropped unrun in that case.
        let done = Arc::new(Mutex::new(Some(done)));
        let report = done.clone();
        let spawned = std::thread::Builder::new()
            .name("capture-start".to_owned())
            .spawn(move || {
                let result = start_pipeline(&native, &settings, sink, &flag);
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
        self.native.game_capture_available()
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
        self.native.process_watcher()
    }

    fn icon_extractor(&self) -> Arc<dyn IconExtractor> {
        self.native.icon_extractor()
    }
}

impl Drop for GstBackend {
    fn drop(&mut self) {
        self.stop();
    }
}
