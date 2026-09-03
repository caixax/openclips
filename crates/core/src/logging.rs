use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

use crate::error::{CoreError, Result};

pub const LOG_ENV_VAR: &str = "OPENCLIPS_LOG";
pub const LOG_FILE_PREFIX: &str = "openclips.log";

/// Keeps the background log writer alive. Drop it only at process exit.
#[derive(Debug)]
pub struct LogGuard {
    _worker: WorkerGuard,
}

/// Logs to stderr and to a daily rotated file in `log_dir`.
/// The filter defaults to `info` and is overridable through `OPENCLIPS_LOG`.
pub fn init(log_dir: &Path) -> Result<LogGuard> {
    std::fs::create_dir_all(log_dir).map_err(|source| CoreError::CreateDir {
        path: log_dir.to_path_buf(),
        source,
    })?;

    let file_appender = tracing_appender::rolling::daily(log_dir, LOG_FILE_PREFIX);
    let (file_writer, worker) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_env(LOG_ENV_VAR).unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true).with_writer(std::io::stderr))
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_target(true)
                .with_writer(file_writer),
        )
        .try_init()
        .map_err(|_| CoreError::LoggingAlreadyInitialized)?;

    Ok(LogGuard { _worker: worker })
}
