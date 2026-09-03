//! Windows backend built on GStreamer: DXGI desktop duplication through
//! `d3d11screencapturesrc`, hardware encoding on the D3D11 device, and MP4
//! muxing for clips.

mod encoders;
mod monitors;
mod mux;
mod pipeline;
mod props;

use std::sync::Arc;

use gstreamer as gst;
use openclips_core::capture::{CaptureSettings, EncoderInfo, MonitorInfo};
use tracing::info;

use crate::backend::{CaptureBackend, ClipWriter, FrameSink};
use crate::error::CaptureError;

pub struct WindowsBackend {
    encoders: Vec<EncoderInfo>,
    capture: Option<pipeline::CapturePipeline>,
    writer: Arc<mux::Mp4Writer>,
}

impl WindowsBackend {
    pub fn new() -> Result<Self, CaptureError> {
        gst::init().map_err(|e| CaptureError::FrameworkInit(e.to_string()))?;
        info!("GStreamer {} initialized", gst::version_string());

        for element in [
            "d3d11screencapturesrc",
            "d3d11convert",
            "h264parse",
            "mp4mux",
            "appsink",
            "appsrc",
        ] {
            if gst::ElementFactory::find(element).is_none() {
                return Err(CaptureError::MissingElement(element.to_owned()));
            }
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

    fn start(
        &mut self,
        settings: &CaptureSettings,
        sink: Arc<dyn FrameSink>,
    ) -> Result<(), CaptureError> {
        if self.capture.is_some() {
            return Err(CaptureError::AlreadyRunning);
        }
        let capture = pipeline::CapturePipeline::start(settings, sink)?;
        self.capture = Some(capture);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(capture) = self.capture.take() {
            capture.stop();
        }
    }

    fn is_running(&self) -> bool {
        self.capture.is_some()
    }

    fn clip_writer(&self) -> Arc<dyn ClipWriter> {
        self.writer.clone()
    }
}

impl Drop for WindowsBackend {
    fn drop(&mut self) {
        self.stop();
    }
}
