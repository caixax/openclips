use std::path::Path;
use std::sync::Arc;

use openclips_core::capture::{CaptureSettings, EncoderInfo, MonitorInfo};
use openclips_core::clip::ClipFile;
use openclips_core::media::{EncodedFrame, StreamInfo};
use openclips_core::replay::ReplaySnapshot;

use crate::error::CaptureError;

/// Receives the output of a running capture. Called from backend threads, so
/// implementations must be cheap and must not block for long.
pub trait FrameSink: Send + Sync + 'static {
    /// Called once per capture start before the first frame, and again if
    /// the stream properties change mid capture.
    fn on_stream(&self, info: StreamInfo);
    fn on_frame(&self, frame: EncodedFrame);
    /// A fatal capture failure. The backend stops after reporting it.
    fn on_error(&self, error: CaptureError);
}

/// Writes encoded frames into a playable container file.
pub trait ClipWriter: Send + Sync {
    fn write_clip(&self, snapshot: &ReplaySnapshot, path: &Path) -> Result<ClipFile, CaptureError>;
}

/// Opens streaming recordings that receive frames as they are captured.
pub trait Recorder: Send + Sync {
    fn start(
        &self,
        stream: &StreamInfo,
        path: &Path,
    ) -> Result<Box<dyn RecordingSession>, CaptureError>;
}

/// One recording in progress. The first pushed frame must be a keyframe.
pub trait RecordingSession: Send {
    fn push(&mut self, frame: &EncodedFrame) -> Result<(), CaptureError>;
    fn finish(self: Box<Self>) -> Result<ClipFile, CaptureError>;
    fn path(&self) -> &Path;
}

/// The platform abstraction. One implementation per operating system.
pub trait CaptureBackend: Send {
    fn name(&self) -> &'static str;

    /// Encoders verified to work on this machine, best first.
    fn available_encoders(&self) -> &[EncoderInfo];

    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError>;

    fn start(
        &mut self,
        settings: &CaptureSettings,
        sink: Arc<dyn FrameSink>,
    ) -> Result<(), CaptureError>;

    fn stop(&mut self);

    fn is_running(&self) -> bool;

    /// A thread safe writer that can be used while capture is running.
    fn clip_writer(&self) -> Arc<dyn ClipWriter>;

    /// A thread safe factory for full session recordings.
    fn recorder(&self) -> Arc<dyn Recorder>;
}
