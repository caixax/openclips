use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Core(#[from] openclips_core::CoreError),

    #[error(transparent)]
    Capture(#[from] openclips_capture::CaptureError),

    #[error("UI error: {0}")]
    Ui(#[from] slint::PlatformError),

    #[error("hotkey error: {0}")]
    Hotkey(String),
}
