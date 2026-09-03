use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("could not determine a home or config directory for this user")]
    NoProjectDirs,

    #[error("failed to read {path}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write {path}: {source}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid config file {path}: {source}")]
    ParseConfig {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("failed to serialize config: {0}")]
    SerializeConfig(#[from] toml::ser::Error),

    #[error("invalid library file {path}: {source}")]
    ParseLibrary {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to serialize the library: {0}")]
    SerializeLibrary(#[from] serde_json::Error),

    #[error("invalid hotkey binding \"{input}\": {reason}")]
    InvalidHotkey { input: String, reason: String },

    #[error("invalid configuration value for {field}: {reason}")]
    InvalidConfig { field: &'static str, reason: String },

    #[error("logging is already initialized")]
    LoggingAlreadyInitialized,
}
