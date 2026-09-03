use std::path::PathBuf;

use directories::{ProjectDirs, UserDirs};

use crate::error::{CoreError, Result};

pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Well known directories used by the application, resolved once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
    pub default_clips_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let project = ProjectDirs::from("", "", crate::APP_NAME).ok_or(CoreError::NoProjectDirs)?;
        let user = UserDirs::new().ok_or(CoreError::NoProjectDirs)?;

        let videos = user
            .video_dir()
            .map(PathBuf::from)
            .unwrap_or_else(|| user.home_dir().join("Videos"));

        let data_dir = project.data_local_dir().to_path_buf();
        Ok(Self {
            config_dir: project.config_dir().to_path_buf(),
            log_dir: data_dir.join("logs"),
            cache_dir: project.cache_dir().to_path_buf(),
            data_dir,
            default_clips_dir: videos.join(crate::APP_NAME),
        })
    }

    /// Builds paths rooted at a single directory. Used by tests and by a
    /// future portable mode where everything lives next to the executable.
    pub fn rooted_at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            log_dir: root.join("logs"),
            default_clips_dir: root.join("clips"),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE_NAME)
    }
}
