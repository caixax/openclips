//! Platform capture and encoding backends.
//!
//! This crate owns every piece of code that touches the screen, the audio
//! devices or a hardware encoder. The rest of the application only sees the
//! platform neutral [`CaptureBackend`] trait and the shared types from
//! `openclips-core`.
//!
//! A future Linux backend (PipeWire through the desktop portal, VAAPI
//! encode) is expected to be a sibling of the `windows` module and nothing
//! else.

mod backend;
mod error;
pub mod platform;

#[cfg(windows)]
mod windows;

pub use backend::{CaptureBackend, ClipWriter, FrameSink};
pub use error::CaptureError;

/// Creates the capture backend for the current platform. Initializes the
/// media framework and probes the available encoders, so call it once.
pub fn create_backend() -> Result<Box<dyn CaptureBackend>, CaptureError> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows::WindowsBackend::new()?))
    }
    #[cfg(not(windows))]
    {
        Err(CaptureError::Unsupported(
            platform::Platform::current().name(),
        ))
    }
}
