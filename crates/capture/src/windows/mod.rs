//! Windows backend built on GStreamer: DXGI desktop duplication through
//! `d3d11screencapturesrc`, hardware encoding on the D3D11 device, WASAPI
//! audio capture, and MP4 muxing for clips and recordings.

mod audio;
mod encoders;
mod media;
mod monitors;
mod mux;
mod pipeline;
mod player;
mod props;
mod recording;
mod trim;

use std::sync::Arc;

use gstreamer as gst;
use openclips_core::capture::{AudioDeviceInfo, CaptureSettings, EncoderInfo, MonitorInfo};
use tracing::{info, warn};

use crate::backend::{
    CaptureBackend, ClipWriter, FrameSink, MediaTools, Player, PlayerSink, Recorder,
};
use crate::error::CaptureError;

const START_ATTEMPTS: u32 = 3;
const START_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

pub struct WindowsBackend {
    encoders: Vec<EncoderInfo>,
    capture: Option<pipeline::CapturePipeline>,
    writer: Arc<mux::Mp4Writer>,
    recorder: Arc<recording::Mp4Recorder>,
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

        Ok(Self {
            encoders,
            capture: None,
            writer: Arc::new(mux::Mp4Writer),
            recorder: Arc::new(recording::Mp4Recorder),
        })
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
        if self.capture.is_some() {
            return Err(CaptureError::AlreadyRunning);
        }
        // NVENC occasionally refuses to open a session and succeeds moments
        // later, so a start that fails inside the encoder is retried before
        // the caller moves on to another encoder.
        let attempts = if settings.encoder.kind.is_hardware() {
            START_ATTEMPTS
        } else {
            1
        };
        let mut last = None;
        for attempt in 1..=attempts {
            match pipeline::CapturePipeline::start(settings, sink.clone()) {
                Ok(capture) => {
                    self.capture = Some(capture);
                    return Ok(());
                }
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

    fn stop(&mut self) {
        if let Some(capture) = self.capture.take() {
            capture.stop();
        }
    }

    fn is_running(&self) -> bool {
        self.capture.is_some()
    }

    fn set_audio_level(&self, source_key: &str, volume: f32, muted: bool) -> bool {
        self.capture
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
        Arc::new(media::GstMediaTools)
    }

    fn create_player(&self, sink: Arc<dyn PlayerSink>) -> Result<Box<dyn Player>, CaptureError> {
        Ok(Box::new(player::GstPlayer::new(sink)?))
    }
}

impl Drop for WindowsBackend {
    fn drop(&mut self) {
        self.stop();
    }
}
