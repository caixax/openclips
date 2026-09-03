use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum CaptureError {
    #[error("capture is not supported on {0}")]
    Unsupported(&'static str),

    #[error("the media framework failed to initialize: {0}")]
    FrameworkInit(String),

    #[error("required media component \"{0}\" is missing (check the GStreamer plugins)")]
    MissingElement(String),

    #[error("no usable video encoder was found")]
    NoEncoder,

    #[error("display \"{0}\" was not found")]
    MonitorNotFound(String),

    #[error("failed to build the capture pipeline: {0}")]
    PipelineBuild(String),

    #[error("the capture pipeline failed: {0}")]
    Pipeline(String),

    #[error("encoder {encoder} could not start: {reason}")]
    EncoderStart { encoder: String, reason: String },

    #[error("no video encoder could start: {0}")]
    AllEncodersFailed(String),

    #[error("capture is already running")]
    AlreadyRunning,

    #[error("failed to write clip {path}: {reason}")]
    ClipWrite { path: PathBuf, reason: String },

    #[error("the replay buffer is empty, nothing to save")]
    EmptyBuffer,
}
