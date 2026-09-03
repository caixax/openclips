//! Conversion between the config file and the settings page state.

use std::path::{Path, PathBuf};

use openclips_core::capture::{AudioDeviceInfo, AudioDeviceKind, MonitorInfo};
use openclips_core::config::{AudioSourceConfig, Config, DisplaySelection, EncoderPreference};
use openclips_core::config::{CaptureScope, GameAction, GameProfile};
use slint::{Model, ModelRc, SharedString, VecModel};

use crate::ui::{AudioSourceRow, GameProfileRow, SettingsState};
use slint::Image;

const ENCODERS: [EncoderPreference; 5] = [
    EncoderPreference::Auto,
    EncoderPreference::Nvenc,
    EncoderPreference::QuickSync,
    EncoderPreference::Amf,
    EncoderPreference::Software,
];

const UNIT_SECONDS: i32 = 0;
const UNIT_MINUTES: i32 = 1;
const KIND_OUTPUT: &str = "output";
const KIND_INPUT: &str = "input";

/// Fills the settings page from a config. `default_clips_dir` is shown when
/// the config has no explicit folder so the user sees where clips go.
pub fn populate(
    state: &SettingsState<'_>,
    config: &Config,
    monitors: &[MonitorInfo],
    audio_devices: &[AudioDeviceInfo],
    default_clips_dir: &Path,
) {
    set_monitors(state, monitors, &config.capture.display);
    state.set_encoder_index(
        ENCODERS
            .iter()
            .position(|e| *e == config.capture.encoder)
            .unwrap_or(0) as i32,
    );
    state.set_fps(config.capture.fps as i32);
    state.set_bitrate_kbps(config.capture.bitrate_kbps as i32);
    state.set_show_cursor(config.capture.show_cursor);

    let seconds = config.replay.length_seconds;
    if seconds >= 60 && seconds.is_multiple_of(60) {
        state.set_replay_length_value((seconds / 60) as i32);
        state.set_replay_unit_index(UNIT_MINUTES);
    } else {
        state.set_replay_length_value(seconds as i32);
        state.set_replay_unit_index(UNIT_SECONDS);
    }
    state.set_memory_cap_mb(config.replay.memory_cap_mb as i32);
    state.set_start_on_launch(config.replay.start_on_launch);
    state.set_start_minimized(config.general.start_minimized);

    let clips_dir = config
        .output
        .clips_dir
        .clone()
        .unwrap_or_else(|| default_clips_dir.to_path_buf());
    state.set_clips_dir(clips_dir.display().to_string().into());
    state.set_file_name_pattern(config.output.file_name_pattern.clone().into());
    state.set_recording_subfolder(config.recording.subfolder.clone().into());

    state.set_audio_enabled(config.audio.enabled);
    state.set_separate_tracks(config.audio.separate_tracks);
    state.set_audio_bitrate_kbps(config.audio.bitrate_kbps as i32);
    set_audio_sources(state, config, audio_devices);

    state.set_games_scope_index(match config.games.scope {
        CaptureScope::Global => 0,
        CaptureScope::PerGame => 1,
    });
    set_profile_display_names(state, monitors);
    state.set_hotkey_save_replay(config.hotkeys.save_replay.to_string().into());
    state.set_hotkey_toggle_buffer(config.hotkeys.toggle_replay_buffer.to_string().into());
    state.set_hotkey_toggle_recording(config.hotkeys.toggle_recording.to_string().into());
    state.set_listening_action(-1);
}

/// Replaces the display list, keeping the selection by identity.
pub fn set_monitors(
    state: &SettingsState<'_>,
    monitors: &[MonitorInfo],
    selected: &DisplaySelection,
) {
    let mut names: Vec<SharedString> = vec!["Primary display".into()];
    names.extend(monitors.iter().map(|m| {
        format!(
            "{} ({}x{} at {} Hz)",
            m.name, m.width, m.height, m.refresh_hz
        )
        .into()
    }));
    state.set_display_names(ModelRc::new(VecModel::from(names)));
    let index = match selected {
        DisplaySelection::Primary => 0,
        DisplaySelection::Monitor(id) => monitors
            .iter()
            .position(|m| &m.id == id)
            .map(|i| i as i32 + 1)
            .unwrap_or(0),
    };
    state.set_display_index(index);
}

pub fn selected_display(state: &SettingsState<'_>, monitors: &[MonitorInfo]) -> DisplaySelection {
    let index = state.get_display_index();
    if index <= 0 {
        return DisplaySelection::Primary;
    }
    monitors
        .get(index as usize - 1)
        .map(|m| DisplaySelection::Monitor(m.id.clone()))
        .unwrap_or(DisplaySelection::Primary)
}

fn kind_name(kind: AudioDeviceKind) -> &'static str {
    match kind {
        AudioDeviceKind::Output => KIND_OUTPUT,
        AudioDeviceKind::Input => KIND_INPUT,
    }
}

fn kind_from_name(name: &str) -> AudioDeviceKind {
    if name == KIND_INPUT {
        AudioDeviceKind::Input
    } else {
        AudioDeviceKind::Output
    }
}

/// Builds the audio rows: every connected device, plus configured devices
/// that are not connected right now so their settings are not lost.
pub fn audio_rows(config: &Config, devices: &[AudioDeviceInfo]) -> Vec<AudioSourceRow> {
    let mut rows: Vec<AudioSourceRow> = devices
        .iter()
        .map(|device| {
            let configured = config
                .audio
                .sources
                .iter()
                .find(|s| s.id == device.id && s.kind == device.kind);
            AudioSourceRow {
                id: device.id.clone().into(),
                kind: kind_name(device.kind).into(),
                name: device.name.clone().into(),
                enabled: configured.is_some_and(|s| s.enabled),
                volume: configured.map(|s| s.volume * 100.0).unwrap_or(100.0),
                muted: configured.is_some_and(|s| s.muted),
                connected: true,
            }
        })
        .collect();
    for source in &config.audio.sources {
        let connected = devices
            .iter()
            .any(|d| d.id == source.id && d.kind == source.kind);
        if !connected {
            rows.push(AudioSourceRow {
                id: source.id.clone().into(),
                kind: kind_name(source.kind).into(),
                name: source.name.clone().into(),
                enabled: source.enabled,
                volume: source.volume * 100.0,
                muted: source.muted,
                connected: false,
            });
        }
    }
    rows
}

/// Replaces the audio rows from the current config and device list. Edits
/// already made in the page are kept for rows that still exist.
pub fn set_audio_sources(state: &SettingsState<'_>, config: &Config, devices: &[AudioDeviceInfo]) {
    let rows = audio_rows(config, devices);
    state.set_audio_sources(ModelRc::new(VecModel::from(rows)));
}

/// Same as [`set_audio_sources`] but seeded from the rows currently shown,
/// so a device refresh does not throw away unsaved edits.
pub fn refresh_audio_sources(
    state: &SettingsState<'_>,
    base: &Config,
    devices: &[AudioDeviceInfo],
) {
    let mut edited = base.clone();
    edited.audio.sources = collect_audio_sources(state);
    set_audio_sources(state, &edited, devices);
}

fn collect_audio_sources(state: &SettingsState<'_>) -> Vec<AudioSourceConfig> {
    state
        .get_audio_sources()
        .iter()
        .filter(|row| row.enabled || !row.connected)
        .map(|row| AudioSourceConfig {
            id: row.id.to_string(),
            name: row.name.to_string(),
            kind: kind_from_name(&row.kind),
            enabled: row.enabled,
            volume: (row.volume / 100.0).clamp(0.0, 2.0),
            muted: row.muted,
        })
        .collect()
}

/// Builds a config from the page, starting from `base` so that settings the
/// page does not show survive untouched.
pub fn collect(
    state: &SettingsState<'_>,
    base: &Config,
    monitors: &[MonitorInfo],
    default_clips_dir: &Path,
) -> Result<Config, String> {
    let mut config = base.clone();

    config.capture.display = selected_display(state, monitors);
    config.capture.encoder = ENCODERS
        .get(state.get_encoder_index().max(0) as usize)
        .copied()
        .unwrap_or(EncoderPreference::Auto);
    config.capture.fps = state.get_fps().max(1) as u32;
    config.capture.bitrate_kbps = state.get_bitrate_kbps().max(1) as u32;
    config.capture.show_cursor = state.get_show_cursor();

    let value = state.get_replay_length_value().max(1) as u32;
    config.replay.length_seconds = if state.get_replay_unit_index() == UNIT_MINUTES {
        value.saturating_mul(60)
    } else {
        value
    };
    config.replay.memory_cap_mb = state.get_memory_cap_mb().max(1) as u32;
    config.replay.start_on_launch = state.get_start_on_launch();
    config.general.start_minimized = state.get_start_minimized();

    let clips_dir = state.get_clips_dir();
    let clips_dir = clips_dir.trim();
    config.output.clips_dir = if clips_dir.is_empty() || Path::new(clips_dir) == default_clips_dir {
        None
    } else {
        Some(PathBuf::from(clips_dir))
    };
    let pattern = state.get_file_name_pattern();
    config.output.file_name_pattern = pattern.trim().to_owned();
    config.recording.subfolder = state.get_recording_subfolder().trim().to_owned();

    config.audio.enabled = state.get_audio_enabled();
    config.audio.separate_tracks = state.get_separate_tracks();
    config.audio.bitrate_kbps = state.get_audio_bitrate_kbps().max(1) as u32;
    config.audio.sources = collect_audio_sources(state);

    config.games.scope = if state.get_games_scope_index() == 1 {
        CaptureScope::PerGame
    } else {
        CaptureScope::Global
    };
    config.games.profiles = collect_game_profiles(state, monitors);

    config.hotkeys.save_replay = parse_hotkey("Save replay", &state.get_hotkey_save_replay())?;
    config.hotkeys.toggle_replay_buffer =
        parse_hotkey("Start or stop buffer", &state.get_hotkey_toggle_buffer())?;
    config.hotkeys.toggle_recording = parse_hotkey(
        "Start or stop recording",
        &state.get_hotkey_toggle_recording(),
    )?;

    config.validate().map_err(|e| e.to_string())?;
    Ok(config)
}

fn parse_hotkey(label: &str, text: &str) -> Result<openclips_core::config::Hotkey, String> {
    text.parse().map_err(|e| format!("{label}: {e}"))
}

const ACTIONS: [GameAction; 3] = [
    GameAction::Buffer,
    GameAction::Recording,
    GameAction::Ignore,
];

fn set_profile_display_names(state: &SettingsState<'_>, monitors: &[MonitorInfo]) {
    let mut names: Vec<SharedString> =
        vec!["Use the global display".into(), "Primary display".into()];
    names.extend(monitors.iter().map(|m| SharedString::from(m.name.as_str())));
    state.set_profile_display_names(ModelRc::new(VecModel::from(names)));
}

fn profile_display_index(profile: &GameProfile, monitors: &[MonitorInfo]) -> i32 {
    match &profile.display {
        None => 0,
        Some(DisplaySelection::Primary) => 1,
        Some(DisplaySelection::Monitor(id)) => monitors
            .iter()
            .position(|m| &m.id == id)
            .map(|i| i as i32 + 2)
            .unwrap_or(0),
    }
}

fn display_from_index(index: i32, monitors: &[MonitorInfo]) -> Option<DisplaySelection> {
    match index {
        i if i <= 0 => None,
        1 => Some(DisplaySelection::Primary),
        i => monitors
            .get(i as usize - 2)
            .map(|m| DisplaySelection::Monitor(m.id.clone())),
    }
}

/// Fills the game profile rows. `icon_for` resolves an executable to a
/// cached icon file.
pub fn set_game_profiles(
    state: &SettingsState<'_>,
    profiles: &[GameProfile],
    monitors: &[MonitorInfo],
    icon_for: impl Fn(&str) -> Option<PathBuf>,
) {
    let rows: Vec<GameProfileRow> = profiles
        .iter()
        .map(|p| {
            let icon = icon_for(&p.exe).and_then(|path| Image::load_from_path(&path).ok());
            GameProfileRow {
                exe: p.exe.clone().into(),
                name: p.name.clone().into(),
                action_index: ACTIONS.iter().position(|a| *a == p.action).unwrap_or(0) as i32,
                replay_seconds: p.replay_length_seconds.unwrap_or(0) as i32,
                subfolder: p.subfolder.clone().unwrap_or_default().into(),
                display_index: profile_display_index(p, monitors),
                has_icon: icon.is_some(),
                icon: icon.unwrap_or_default(),
            }
        })
        .collect();
    state.set_game_profiles(ModelRc::new(VecModel::from(rows)));
}

/// Reads the profile rows back, dropping rows without an executable.
pub fn collect_game_profiles(
    state: &SettingsState<'_>,
    monitors: &[MonitorInfo],
) -> Vec<GameProfile> {
    state
        .get_game_profiles()
        .iter()
        .filter(|row| !row.exe.trim().is_empty())
        .map(|row| GameProfile {
            exe: row.exe.trim().to_lowercase(),
            name: row.name.trim().to_owned(),
            action: ACTIONS
                .get(row.action_index.max(0) as usize)
                .copied()
                .unwrap_or_default(),
            replay_length_seconds: (row.replay_seconds > 0).then_some(row.replay_seconds as u32),
            subfolder: {
                let sub = row.subfolder.trim();
                (!sub.is_empty()).then(|| sub.to_owned())
            },
            display: display_from_index(row.display_index, monitors),
        })
        .collect()
}
