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

    #[error("no AAC audio encoder was found")]
    NoAudioEncoder,

    #[error("display \"{0}\" was not found")]
    MonitorNotFound(String),

    #[error("failed to build the capture pipeline: {0}")]
    PipelineBuild(String),

    #[error("the capture pipeline failed: {message}")]
    Pipeline { message: String, element: String },

    /// An audio source failed. `key` identifies the source so the caller can
    /// continue without it.
    #[error("audio source {key} failed: {message}")]
    AudioSource { key: String, message: String },

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

    #[error("could not read {path}: {reason}")]
    Media { path: PathBuf, reason: String },

    #[error("playback failed: {0}")]
    Playback(String),

    #[error("game capture failed: {0}")]
    GameCapture(String),
}
