//! Platform capture and encoding backends.
//!
//! This crate owns every piece of code that touches the screen, the audio
//! devices or a hardware encoder. The rest of the application only sees the
//! platform neutral [`CaptureBackend`] trait and the shared types from
//! `openclips-core`.
//!
//! Every backend is GStreamer. `gst` holds what they share (the capture
//! lifecycle, encoding, muxing, trimming, playback); a platform module
//! supplies the screen and audio sources and the operating system services.

mod backend;
mod error;
pub mod platform;

#[cfg(any(windows, target_os = "linux"))]
mod gst;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as native;
#[cfg(windows)]
use windows as native;

pub use backend::{
    CaptureBackend, ClipWriter, FrameSink, IconExtractor, MediaInfo, MediaTools, Player,
    PlayerSink, ProcessWatcher, Recorder, RecordingSession, TrimJob,
};
pub use error::CaptureError;

/// Creates the capture backend for the current platform. Initializes the
/// media framework and probes the available encoders, so call it once.
pub fn create_backend() -> Result<Box<dyn CaptureBackend>, CaptureError> {
    #[cfg(any(windows, target_os = "linux"))]
    {
        Ok(Box::new(gst::GstBackend::new()?))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(CaptureError::Unsupported(
            platform::Platform::current().name(),
        ))
    }
}
