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

use crate::capture::{AudioDeviceKind, DEFAULT_AUDIO_DEVICE_ID};
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
    pub audio: AudioConfig,
    pub games: GamesConfig,
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
            audio: AudioConfig::default(),
            games: GamesConfig::default(),
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

/// Which Windows screen capture API drives the source. Desktop duplication
/// is the fast default; Windows Graphics Capture is the fallback when a
/// game's presentation mode leaves duplication with black frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaptureApi {
    #[default]
    DesktopDuplication,
    GraphicsCapture,
}

impl CaptureApi {
    pub const fn label(self) -> &'static str {
        match self {
            CaptureApi::DesktopDuplication => "Desktop Duplication (default)",
            CaptureApi::GraphicsCapture => "Windows Graphics Capture",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    pub display: DisplaySelection,
    pub encoder: EncoderPreference,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub show_cursor: bool,
    pub api: CaptureApi,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            display: DisplaySelection::Primary,
            encoder: EncoderPreference::Auto,
            fps: 60,
            bitrate_kbps: 20_000,
            show_cursor: false,
            api: CaptureApi::default(),
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSourceConfig {
    pub id: String,
    /// Last known display name, shown when the device is not connected.
    pub name: String,
    pub kind: AudioDeviceKind,
    pub enabled: bool,
    /// Linear gain, 1.0 is unity, up to 2.0.
    pub volume: f32,
    pub muted: bool,
}

impl Default for AudioSourceConfig {
    fn default() -> Self {
        Self {
            id: DEFAULT_AUDIO_DEVICE_ID.to_owned(),
            name: "Default output".to_owned(),
            kind: AudioDeviceKind::Output,
            enabled: true,
            volume: 1.0,
            muted: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub enabled: bool,
    /// Keep microphones on a second track instead of mixing everything.
    pub separate_tracks: bool,
    pub bitrate_kbps: u32,
    pub sources: Vec<AudioSourceConfig>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            separate_tracks: false,
            bitrate_kbps: 160,
            sources: vec![AudioSourceConfig::default()],
        }
    }
}

impl AudioConfig {
    /// The parts of the audio setup that require rebuilding the pipeline.
    /// Volume and mute are applied live and are deliberately excluded.
    pub fn topology(&self) -> Vec<(bool, bool, u32, String, AudioDeviceKind)> {
        self.sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| {
                (
                    self.enabled,
                    self.separate_tracks,
                    self.bitrate_kbps,
                    s.id.clone(),
                    s.kind,
                )
            })
            .collect()
    }
}

/// Whether capture runs all the time or only while a known game runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaptureScope {
    #[default]
    Global,
    PerGame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GameAction {
    #[default]
    Buffer,
    Recording,
    Ignore,
}

impl GameAction {
    pub const fn label(self) -> &'static str {
        match self {
            GameAction::Buffer => "Replay buffer",
            GameAction::Recording => "Full recording",
            GameAction::Ignore => "Do nothing",
        }
    }
}

/// Per game overrides. Every optional field falls back to the global value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GameProfile {
    /// Lower case executable file name.
    pub exe: String,
    /// Empty means "use the database name".
    pub name: String,
    pub action: GameAction,
    pub replay_length_seconds: Option<u32>,
    pub subfolder: Option<String>,
    pub display: Option<DisplaySelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GamesConfig {
    pub scope: CaptureScope,
    pub profiles: Vec<GameProfile>,
}

/// A hotkey that saves the last `seconds` of the buffer. Zero means the
/// whole buffer length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SaveHotkey {
    pub binding: Hotkey,
    pub seconds: u32,
}

impl Default for SaveHotkey {
    fn default() -> Self {
        Self {
            binding: Hotkey::new(Modifiers::ALT, Key::Char('8')),
            seconds: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    /// Any number of save hotkeys, each with its own length.
    pub save: Vec<SaveHotkey>,
    pub toggle_replay_buffer: Hotkey,
    pub toggle_recording: Hotkey,
    /// Pre 0.2 single save binding, migrated into `save` on load.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_replay: Option<Hotkey>,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            save: vec![SaveHotkey::default()],
            toggle_replay_buffer: Hotkey::new(Modifiers::ALT, Key::Char('9')),
            toggle_recording: Hotkey::new(Modifiers::ALT, Key::Char('0')),
            save_replay: None,
        }
    }
}

impl HotkeyConfig {
    /// Every binding with a name, for conflict checks and display.
    pub fn all(&self) -> Vec<(String, Hotkey)> {
        let mut all: Vec<(String, Hotkey)> = self
            .save
            .iter()
            .enumerate()
            .map(|(i, s)| (format!("save #{}", i + 1), s.binding))
            .collect();
        all.push(("toggle_replay_buffer".to_owned(), self.toggle_replay_buffer));
        all.push(("toggle_recording".to_owned(), self.toggle_recording));
        all
    }

    /// The first save binding, for hints.
    pub fn primary_save(&self) -> Option<&SaveHotkey> {
        self.save.first()
    }

    fn migrate(&mut self) {
        if let Some(binding) = self.save_replay.take()
            && self.save.is_empty()
        {
            self.save.push(SaveHotkey {
                binding,
                seconds: 0,
            });
        }
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
        let mut config: Self = toml::from_str(&text).map_err(|source| CoreError::ParseConfig {
            path: path.to_path_buf(),
            source,
        })?;
        config.hotkeys.migrate();
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
        for source in &self.audio.sources {
            if !(0.0..=2.0).contains(&source.volume) {
                return Err(CoreError::InvalidConfig {
                    field: "audio.sources.volume",
                    reason: format!("{} must be between 0.0 and 2.0", source.name),
                });
            }
        }
        for profile in &self.games.profiles {
            if profile.exe.trim().is_empty() {
                return Err(CoreError::InvalidConfig {
                    field: "games.profiles.exe",
                    reason: "a game profile needs an executable name".to_owned(),
                });
            }
            if let Some(secs) = profile.replay_length_seconds
                && !(MIN_REPLAY_SECONDS..=MAX_REPLAY_SECONDS).contains(&secs)
            {
                return Err(CoreError::InvalidConfig {
                    field: "games.profiles.replay_length_seconds",
                    reason: format!(
                        "must be between {MIN_REPLAY_SECONDS} and {MAX_REPLAY_SECONDS}"
                    ),
                });
            }
        }
        if self.audio.bitrate_kbps == 0 || self.audio.bitrate_kbps > 1024 {
            return Err(CoreError::InvalidConfig {
                field: "audio.bitrate_kbps",
                reason: "must be between 1 and 1024".to_owned(),
            });
        }
        if self.hotkeys.save.is_empty() {
            return Err(CoreError::InvalidConfig {
                field: "hotkeys.save",
                reason: "at least one save hotkey is required".to_owned(),
            });
        }
        for save in &self.hotkeys.save {
            if save.seconds != 0
                && !(MIN_REPLAY_SECONDS..=MAX_REPLAY_SECONDS).contains(&save.seconds)
            {
                return Err(CoreError::InvalidConfig {
                    field: "hotkeys.save.seconds",
                    reason: format!(
                        "must be 0 or between {MIN_REPLAY_SECONDS} and {MAX_REPLAY_SECONDS}"
                    ),
                });
            }
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

    /// True when moving from `self` to `next` requires the capture pipeline
    /// to be rebuilt (anything the encoder or the source is configured with).
    pub fn capture_restart_needed(&self, next: &Config) -> bool {
        self.capture != next.capture
            || self.replay.temp_dir != next.replay.temp_dir
            || self.audio.topology() != next.audio.topology()
    }

    pub fn audio_levels_changed(&self, next: &Config) -> bool {
        self.audio.sources != next.audio.sources
    }

    pub fn hotkeys_changed(&self, next: &Config) -> bool {
        self.hotkeys != next.hotkeys
    }

    pub fn replay_limits_changed(&self, next: &Config) -> bool {
        self.replay.length_seconds != next.replay.length_seconds
            || self.replay.memory_cap_mb != next.replay.memory_cap_mb
    }

    pub fn recordings_dir(&self, paths: &AppPaths) -> PathBuf {
        let sub = self.recording.subfolder.trim();
        if sub.is_empty() {
            self.clips_dir(paths)
        } else {
            self.clips_dir(paths).join(sub)
        }
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
        let text = "[replay]\nlength_seconds = 90\n\n[[hotkeys.save]]\nbinding = \"Ctrl+F10\"\nseconds = 15\n";
        let config: Config = toml::from_str(text).expect("parse");
        assert_eq!(config.replay.length_seconds, 90);
        assert_eq!(
            config.replay.memory_cap_mb,
            ReplayConfig::default().memory_cap_mb
        );
        assert_eq!(config.hotkeys.save[0].binding.to_string(), "Ctrl+F10");
        assert_eq!(config.hotkeys.save[0].seconds, 15);
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
        config.hotkeys.toggle_recording = config.hotkeys.save[0].binding;
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
    fn change_detection_covers_capture_and_hotkeys() {
        let base = Config::default();
        let mut next = base.clone();
        assert!(!base.capture_restart_needed(&next));
        next.capture.fps = 30;
        assert!(base.capture_restart_needed(&next));

        let mut next = base.clone();
        next.replay.length_seconds = 120;
        assert!(!base.capture_restart_needed(&next));
        assert!(base.replay_limits_changed(&next));

        let mut next = base.clone();
        next.hotkeys.save[0].binding = "F9".parse().expect("valid");
        assert!(base.hotkeys_changed(&next));
    }

    #[test]
    fn audio_volume_changes_do_not_restart_capture() {
        let base = Config::default();
        let mut next = base.clone();
        next.audio.sources[0].volume = 0.5;
        next.audio.sources[0].muted = true;
        assert!(!base.capture_restart_needed(&next));
        assert!(base.audio_levels_changed(&next));

        let mut next = base.clone();
        next.audio.separate_tracks = true;
        assert!(base.capture_restart_needed(&next));

        let mut next = base.clone();
        next.audio.sources[0].enabled = false;
        assert!(base.capture_restart_needed(&next));
    }

    #[test]
    fn audio_config_round_trips() {
        let mut config = Config::default();
        config.audio.sources.push(AudioSourceConfig {
            id: "{0.0.1.00000000}.{abc}".to_owned(),
            name: "Microphone".to_owned(),
            kind: AudioDeviceKind::Input,
            enabled: true,
            volume: 1.5,
            muted: false,
        });
        let text = toml::to_string_pretty(&config).expect("serialize");
        assert!(text.contains("kind = \"input\""), "{text}");
        let back: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(back, config);
    }

    #[test]
    fn recordings_dir_uses_subfolder() {
        let paths = AppPaths::rooted_at("/tmp/openclips-test");
        let mut config = Config::default();
        assert_eq!(
            config.recordings_dir(&paths),
            paths.default_clips_dir.join("Recordings")
        );
        config.recording.subfolder = "  ".to_owned();
        assert_eq!(config.recordings_dir(&paths), paths.default_clips_dir);
    }

    #[test]
    fn clips_dir_falls_back_to_platform_default() {
        let paths = AppPaths::rooted_at("/tmp/openclips-test");
        let config = Config::default();
        assert_eq!(config.clips_dir(&paths), paths.default_clips_dir);
    }
}

#[cfg(test)]
mod hotkey_migration_tests {
    use super::*;

    #[test]
    fn old_save_replay_key_becomes_a_save_hotkey() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, "[hotkeys]\nsave_replay = \"F9\"\nsave = []\n").expect("write");
        let config = Config::load(&path).expect("load");
        assert_eq!(config.hotkeys.save.len(), 1);
        assert_eq!(config.hotkeys.save[0].binding.to_string(), "F9");
        assert_eq!(config.hotkeys.save[0].seconds, 0);
        assert!(config.hotkeys.save_replay.is_none());
        let text = toml::to_string(&config).expect("serialize");
        assert!(!text.contains("save_replay"));
    }
}
