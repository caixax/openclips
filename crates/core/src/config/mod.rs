//! User configuration: a single TOML file in the platform config directory.
//!
//! Every field has a default so that a partial or missing file always loads.
//! Unknown top level keys are rejected so that a typo in a hand edited file is
//! reported instead of silently ignored.

mod hotkey;
mod paths;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

pub use hotkey::{Hotkey, Key, Modifiers};
pub use paths::{AppPaths, CONFIG_FILE_NAME};

pub const CONFIG_VERSION: u32 = 1;

pub const MIN_REPLAY_SECONDS: u32 = 5;
pub const MAX_REPLAY_SECONDS: u32 = 20 * 60;
pub const MIN_MEMORY_CAP_MB: u32 = 64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub general: GeneralConfig,
    pub capture: CaptureConfig,
    pub replay: ReplayConfig,
    pub recording: RecordingConfig,
    pub output: OutputConfig,
    pub hotkeys: HotkeyConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            general: GeneralConfig::default(),
            capture: CaptureConfig::default(),
            replay: ReplayConfig::default(),
            recording: RecordingConfig::default(),
            output: OutputConfig::default(),
            hotkeys: HotkeyConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub launch_on_startup: bool,
    pub start_minimized: bool,
}

/// Which display to capture. `Primary` follows the OS primary monitor even
/// when the physical monitor set changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum DisplaySelection {
    Primary,
    Monitor(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderPreference {
    Auto,
    Nvenc,
    QuickSync,
    Amf,
    Software,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    pub display: DisplaySelection,
    pub encoder: EncoderPreference,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub show_cursor: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            display: DisplaySelection::Primary,
            encoder: EncoderPreference::Auto,
            fps: 60,
            bitrate_kbps: 20_000,
            show_cursor: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplayConfig {
    pub start_on_launch: bool,
    pub length_seconds: u32,
    pub memory_cap_mb: u32,
    /// When set, buffer segments that must touch disk go here (for example a
    /// RAM disk). Empty means pure in memory buffering.
    pub temp_dir: Option<PathBuf>,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            start_on_launch: true,
            length_seconds: 30,
            memory_cap_mb: 1024,
            temp_dir: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingConfig {
    pub subfolder: String,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            subfolder: "Recordings".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    /// Root folder for clips. `None` resolves to the platform videos folder.
    pub clips_dir: Option<PathBuf>,
    pub file_name_pattern: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            clips_dir: None,
            file_name_pattern: "{game} {date} {time}".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub save_replay: Hotkey,
    pub toggle_replay_buffer: Hotkey,
    pub toggle_recording: Hotkey,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            save_replay: Hotkey::new(Modifiers::ALT, Key::Char('8')),
            toggle_replay_buffer: Hotkey::new(Modifiers::ALT, Key::Char('9')),
            toggle_recording: Hotkey::new(Modifiers::ALT, Key::Char('0')),
        }
    }
}

impl HotkeyConfig {
    pub fn all(&self) -> [(&'static str, Hotkey); 3] {
        [
            ("save_replay", self.save_replay),
            ("toggle_replay_buffer", self.toggle_replay_buffer),
            ("toggle_recording", self.toggle_recording),
        ]
    }
}

impl Config {
    /// Loads the config file, returning defaults when it does not exist.
    /// A malformed file is an error rather than silently replaced, so the
    /// user never loses hand edited settings.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(CoreError::ReadFile {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let config: Self = toml::from_str(&text).map_err(|source| CoreError::ParseConfig {
            path: path.to_path_buf(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Loads the config or, when the file is missing, writes the defaults so
    /// the user has something to edit.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let config = Self::load(path)?;
        if !path.exists() {
            config.save(path)?;
        }
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|source| CoreError::CreateDir {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text).map_err(|source| CoreError::WriteFile {
            path: tmp.clone(),
            source,
        })?;
        fs::rename(&tmp, path).map_err(|source| CoreError::WriteFile {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn validate(&self) -> Result<()> {
        let secs = self.replay.length_seconds;
        if !(MIN_REPLAY_SECONDS..=MAX_REPLAY_SECONDS).contains(&secs) {
            return Err(CoreError::InvalidConfig {
                field: "replay.length_seconds",
                reason: format!(
                    "must be between {MIN_REPLAY_SECONDS} and {MAX_REPLAY_SECONDS}, got {secs}"
                ),
            });
        }
        if self.replay.memory_cap_mb < MIN_MEMORY_CAP_MB {
            return Err(CoreError::InvalidConfig {
                field: "replay.memory_cap_mb",
                reason: format!("must be at least {MIN_MEMORY_CAP_MB}"),
            });
        }
        if self.capture.fps == 0 || self.capture.fps > 240 {
            return Err(CoreError::InvalidConfig {
                field: "capture.fps",
                reason: "must be between 1 and 240".to_owned(),
            });
        }
        if self.capture.bitrate_kbps == 0 {
            return Err(CoreError::InvalidConfig {
                field: "capture.bitrate_kbps",
                reason: "must be greater than zero".to_owned(),
            });
        }
        let bindings = self.hotkeys.all();
        for (i, (name_a, a)) in bindings.iter().enumerate() {
            for (name_b, b) in &bindings[i + 1..] {
                if a == b {
                    return Err(CoreError::InvalidConfig {
                        field: "hotkeys",
                        reason: format!("{name_a} and {name_b} are both bound to {a}"),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn clips_dir(&self, paths: &AppPaths) -> PathBuf {
        self.output
            .clips_dir
            .clone()
            .unwrap_or_else(|| paths.default_clips_dir.clone())
    }

    pub fn replay_length(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.replay.length_seconds))
    }

    pub fn replay_memory_cap_bytes(&self) -> usize {
        self.replay.memory_cap_mb as usize * 1024 * 1024
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_round_trip() {
        let config = Config::default();
        config.validate().expect("defaults validate");
        let text = toml::to_string_pretty(&config).expect("serialize");
        let back: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(back, config);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        assert_eq!(Config::load(&path).expect("load"), Config::default());
        assert!(!path.exists());
    }

    #[test]
    fn load_or_create_writes_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("config.toml");
        let config = Config::load_or_create(&path).expect("load");
        assert_eq!(config, Config::default());
        assert!(path.exists());
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn partial_file_fills_in_defaults() {
        let text = "[replay]\nlength_seconds = 90\n\n[hotkeys]\nsave_replay = \"Ctrl+F10\"\n";
        let config: Config = toml::from_str(text).expect("parse");
        assert_eq!(config.replay.length_seconds, 90);
        assert_eq!(
            config.replay.memory_cap_mb,
            ReplayConfig::default().memory_cap_mb
        );
        assert_eq!(config.hotkeys.save_replay.to_string(), "Ctrl+F10");
        assert_eq!(
            config.hotkeys.toggle_recording,
            HotkeyConfig::default().toggle_recording
        );
    }

    #[test]
    fn malformed_file_is_an_error_not_a_reset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, "this is = not [ toml").expect("write");
        assert!(matches!(
            Config::load(&path),
            Err(CoreError::ParseConfig { .. })
        ));
    }

    #[test]
    fn rejects_out_of_range_and_conflicting_values() {
        let mut config = Config::default();
        config.replay.length_seconds = 1;
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.hotkeys.toggle_recording = config.hotkeys.save_replay;
        assert!(matches!(
            config.validate(),
            Err(CoreError::InvalidConfig {
                field: "hotkeys",
                ..
            })
        ));
    }

    #[test]
    fn display_selection_serializes_readably() {
        let mut config = Config::default();
        config.capture.display = DisplaySelection::Monitor(r"\\.\DISPLAY2".to_owned());
        let text = toml::to_string(&config).expect("serialize");
        assert!(text.contains("[capture.display]"), "{text}");
        assert!(text.contains("kind = \"monitor\""), "{text}");
        let back: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(back.capture.display, config.capture.display);
    }

    #[test]
    fn clips_dir_falls_back_to_platform_default() {
        let paths = AppPaths::rooted_at("/tmp/openclips-test");
        let config = Config::default();
        assert_eq!(config.clips_dir(&paths), paths.default_clips_dir);
    }
}
