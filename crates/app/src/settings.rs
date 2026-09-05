//! Conversion between the config file and the settings page state.

use std::path::{Path, PathBuf};

use openclips_core::capture::{AudioDeviceInfo, AudioDeviceKind, MonitorInfo};
use openclips_core::config::{AudioSourceConfig, Config, DisplaySelection, EncoderPreference};
use openclips_core::config::{CaptureApi, CaptureScope, GameAction, GameProfile, Language};
use openclips_core::config::{Hotkey, HotkeyActionKind, HotkeyBinding};
use slint::{Model, ModelRc, SharedString, VecModel};

use crate::ui::{AudioSourceRow, GameProfileRow, HotkeyRow, SettingsState};
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
const KIND_APP: &str = "app";
/// Hotkey rows use key capture action ids from this value upwards.
pub const SAVE_ACTION_BASE: i32 = 100;

/// Quality presets as (name, fps, bitrate in kbps). The last entry is custom.
pub const QUALITY_PRESETS: [(&str, u32, u32); 3] = [
    ("Low", 30, 8000),
    ("Standard", 60, 15000),
    ("High", 60, 30000),
];

pub fn quality_index(fps: u32, bitrate_kbps: u32) -> i32 {
    QUALITY_PRESETS
        .iter()
        .position(|(_, f, b)| *f == fps && *b == bitrate_kbps)
        .map_or(QUALITY_PRESETS.len() as i32, |i| i as i32)
}

/// Short quality description for the top bar.
pub fn quality_label(fps: u32, bitrate_kbps: u32) -> String {
    let mbps = bitrate_kbps as f64 / 1000.0;
    match QUALITY_PRESETS
        .iter()
        .find(|(_, f, b)| *f == fps && *b == bitrate_kbps)
    {
        Some((name, _, _)) => format!("{name} quality, {fps} fps"),
        None => format!("{fps} fps, {mbps:.0} Mbps"),
    }
}

/// Keycap texts for a binding, for example ["ALT", "F7"].
pub fn key_parts(hotkey: Hotkey) -> ModelRc<SharedString> {
    let parts: Vec<SharedString> = hotkey
        .to_string()
        .split('+')
        .map(|p| SharedString::from(p.to_uppercase()))
        .collect();
    ModelRc::new(VecModel::from(parts))
}

fn hotkey_row(b: &HotkeyBinding) -> HotkeyRow {
    HotkeyRow {
        binding: b.binding.to_string().into(),
        keys: key_parts(b.binding),
        action_index: HotkeyActionKind::ALL
            .iter()
            .position(|a| *a == b.action)
            .unwrap_or(0) as i32,
        minutes: (b.seconds / 60) as i32,
        seconds: (b.seconds % 60) as i32,
    }
}

fn hotkey_rows(state: &SettingsState<'_>) -> Vec<HotkeyRow> {
    state.get_hotkeys().iter().collect()
}

pub fn set_hotkeys(state: &SettingsState<'_>, bindings: &[HotkeyBinding]) {
    let rows: Vec<HotkeyRow> = bindings.iter().map(hotkey_row).collect();
    state.set_hotkeys(ModelRc::new(VecModel::from(rows)));
}

/// Replaces the binding of one row after a key capture.
pub fn set_hotkey_binding(state: &SettingsState<'_>, index: usize, hotkey: Hotkey) {
    let model = state.get_hotkeys();
    if let Some(mut row) = model.row_data(index) {
        row.binding = hotkey.to_string().into();
        row.keys = key_parts(hotkey);
        model.set_row_data(index, row);
    }
}

/// Appends a save row with a binding that is not used yet.
pub fn add_hotkey(state: &SettingsState<'_>) {
    let mut rows = hotkey_rows(state);
    let used: Vec<String> = rows.iter().map(|r| r.binding.to_string()).collect();
    let free = (1..=12)
        .map(|n| format!("Alt+F{n}"))
        .find(|b| !used.contains(b))
        .unwrap_or_else(|| "Alt+F1".to_owned());
    let binding: Hotkey = free
        .parse()
        .unwrap_or_else(|_| HotkeyBinding::default().binding);
    rows.push(hotkey_row(&HotkeyBinding {
        binding,
        action: HotkeyActionKind::SaveReplay,
        seconds: 15,
    }));
    state.set_hotkeys(ModelRc::new(VecModel::from(rows)));
}

pub fn remove_hotkey(state: &SettingsState<'_>, index: usize) {
    let mut rows = hotkey_rows(state);
    if rows.len() > 1 && index < rows.len() {
        rows.remove(index);
        state.set_hotkeys(ModelRc::new(VecModel::from(rows)));
    }
}

fn collect_hotkeys(state: &SettingsState<'_>) -> Result<Vec<HotkeyBinding>, String> {
    let mut bindings = Vec::new();
    for (i, row) in state.get_hotkeys().iter().enumerate() {
        let binding = parse_hotkey(&format!("Hotkey {}", i + 1), &row.binding)?;
        let action = HotkeyActionKind::ALL
            .get(row.action_index.max(0) as usize)
            .copied()
            .unwrap_or_default();
        let seconds = row.minutes.clamp(0, 60) as u32 * 60 + row.seconds.clamp(0, 59) as u32;
        bindings.push(HotkeyBinding {
            binding,
            action,
            seconds: if action == HotkeyActionKind::SaveReplay {
                seconds
            } else {
                0
            },
        });
    }
    Ok(bindings)
}

/// Adds an application audio source row for `exe` unless one exists.
pub fn add_app_source(state: &SettingsState<'_>, exe: &str) {
    let exe = exe.trim();
    if exe.is_empty() {
        return;
    }
    let id = exe.to_lowercase();
    let mut rows: Vec<AudioSourceRow> = state.get_audio_sources().iter().collect();
    if rows
        .iter()
        .any(|r| r.kind == KIND_APP && r.id == id.as_str())
    {
        return;
    }
    let name = exe.trim_end_matches(".exe").trim_end_matches(".EXE");
    rows.push(AudioSourceRow {
        id: id.into(),
        kind: KIND_APP.into(),
        name: name.into(),
        enabled: true,
        volume: 100.0,
        muted: false,
        connected: true,
    });
    state.set_audio_sources(ModelRc::new(VecModel::from(rows)));
}

pub fn remove_audio_source(state: &SettingsState<'_>, id: &str) {
    let rows: Vec<AudioSourceRow> = state
        .get_audio_sources()
        .iter()
        .filter(|r| !(r.kind == KIND_APP && r.id == id))
        .collect();
    state.set_audio_sources(ModelRc::new(VecModel::from(rows)));
}

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
    state.set_quality_index(quality_index(
        config.capture.fps,
        config.capture.bitrate_kbps,
    ));
    state.set_show_cursor(config.capture.show_cursor);
    state.set_stretch(config.capture.stretch);
    state.set_capture_api_index(match config.capture.api {
        CaptureApi::DesktopDuplication => 0,
        CaptureApi::GraphicsCapture => 1,
    });
    state.set_launch_on_startup(config.general.launch_on_startup);
    state.set_clip_sound(config.general.clip_sound);
    state.set_clip_toast(config.general.clip_toast);
    state.set_animations(config.general.animations);
    state.set_language_index(
        Language::ALL
            .iter()
            .position(|l| *l == config.general.language)
            .unwrap_or(0) as i32,
    );

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
    state.set_updates_check(config.updates.check);
    state.set_discord_enabled(config.discord.enabled);
    state.set_discord_show_game(config.discord.show_game);
    state.set_discord_client_id(config.discord.client_id.clone().into());

    let clips_dir = config
        .output
        .clips_dir
        .clone()
        .unwrap_or_else(|| default_clips_dir.to_path_buf());
    state.set_clips_dir(clips_dir.display().to_string().into());
    state.set_file_name_pattern(config.output.file_name_pattern.clone().into());
    state.set_clips_subfolder(config.output.clips_subfolder.clone().into());
    state.set_recording_subfolder(config.recording.subfolder.clone().into());
    state.set_edited_subfolder(config.output.edited_subfolder.clone().into());

    state.set_audio_enabled(config.audio.enabled);
    state.set_separate_tracks(config.audio.separate_tracks);
    state.set_audio_bitrate_kbps(config.audio.bitrate_kbps as i32);
    set_audio_sources(state, config, audio_devices);

    state.set_games_scope_index(match config.games.scope {
        CaptureScope::Global => 0,
        CaptureScope::PerGame => 1,
    });
    set_profile_display_names(state, monitors);
    set_hotkeys(state, &config.hotkeys.bindings);
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
        AudioDeviceKind::Application => KIND_APP,
    }
}

fn kind_from_name(name: &str) -> AudioDeviceKind {
    match name {
        KIND_INPUT => AudioDeviceKind::Input,
        KIND_APP => AudioDeviceKind::Application,
        _ => AudioDeviceKind::Output,
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
                connected: source.kind == AudioDeviceKind::Application,
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
        .filter(|row| row.enabled || !row.connected || row.kind == KIND_APP)
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
    config.capture.stretch = state.get_stretch();
    config.capture.api = if state.get_capture_api_index() == 1 {
        CaptureApi::GraphicsCapture
    } else {
        CaptureApi::DesktopDuplication
    };
    config.general.launch_on_startup = state.get_launch_on_startup();
    config.general.clip_sound = state.get_clip_sound();
    config.general.clip_toast = state.get_clip_toast();
    config.general.animations = state.get_animations();
    config.general.language = Language::ALL
        .get(state.get_language_index().max(0) as usize)
        .copied()
        .unwrap_or_default();

    let value = state.get_replay_length_value().max(1) as u32;
    config.replay.length_seconds = if state.get_replay_unit_index() == UNIT_MINUTES {
        value.saturating_mul(60)
    } else {
        value
    };
    config.replay.memory_cap_mb = state.get_memory_cap_mb().max(1) as u32;
    config.replay.start_on_launch = state.get_start_on_launch();
    config.general.start_minimized = state.get_start_minimized();
    config.updates.check = state.get_updates_check();
    config.discord.enabled = state.get_discord_enabled();
    config.discord.show_game = state.get_discord_show_game();
    config.discord.client_id = state.get_discord_client_id().trim().to_owned();

    let clips_dir = state.get_clips_dir();
    let clips_dir = clips_dir.trim();
    config.output.clips_dir = if clips_dir.is_empty() || Path::new(clips_dir) == default_clips_dir {
        None
    } else {
        Some(PathBuf::from(clips_dir))
    };
    let pattern = state.get_file_name_pattern();
    config.output.file_name_pattern = pattern.trim().to_owned();
    config.output.clips_subfolder = state.get_clips_subfolder().trim().to_owned();
    config.recording.subfolder = state.get_recording_subfolder().trim().to_owned();
    config.output.edited_subfolder = state.get_edited_subfolder().trim().to_owned();

    config.audio.enabled = state.get_audio_enabled();
    config.audio.separate_tracks = state.get_separate_tracks();
    config.audio.bitrate_kbps = state.get_audio_bitrate_kbps().max(1) as u32;
    config.audio.sources = collect_audio_sources(state);

    config.games.scope = if state.get_games_scope_index() == 1 {
        CaptureScope::PerGame
    } else {
        CaptureScope::Global
    };
    config.games.profiles = collect_game_profiles(state, monitors, &config.games.profiles);

    config.hotkeys.bindings = collect_hotkeys(state)?;
    // A save hotkey longer than the buffer would be capped, so the buffer
    // follows the longest one instead of asking for a second edit.
    let longest = config
        .hotkeys
        .bindings
        .iter()
        .filter(|b| b.action == HotkeyActionKind::SaveReplay)
        .map(|b| b.seconds)
        .max()
        .unwrap_or(0);
    if longest > config.replay.length_seconds {
        config.replay.length_seconds = longest;
    }

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
    previous: &[GameProfile],
) -> Vec<GameProfile> {
    state
        .get_game_profiles()
        .iter()
        .filter(|row| !row.exe.trim().is_empty())
        .map(|row| {
            let exe = row.exe.trim().to_lowercase();
            // The capture method has no UI control yet; carry the value set in
            // the config file across settings saves instead of dropping it.
            let capture_method = previous
                .iter()
                .find(|p| p.exe == exe)
                .and_then(|p| p.capture_method);
            GameProfile {
                exe,
                name: row.name.trim().to_owned(),
                action: ACTIONS
                    .get(row.action_index.max(0) as usize)
                    .copied()
                    .unwrap_or_default(),
                replay_length_seconds: (row.replay_seconds > 0)
                    .then_some(row.replay_seconds as u32),
                subfolder: {
                    let sub = row.subfolder.trim();
                    (!sub.is_empty()).then(|| sub.to_owned())
                },
                display: display_from_index(row.display_index, monitors),
                capture_method,
            }
        })
        .collect()
}
