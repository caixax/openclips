//! Conversion between the config file and the settings page state.

use std::path::{Path, PathBuf};

use openclips_core::capture::MonitorInfo;
use openclips_core::config::{Config, DisplaySelection, EncoderPreference};
use slint::{ModelRc, SharedString, VecModel};

use crate::ui::SettingsState;

const ENCODERS: [EncoderPreference; 5] = [
    EncoderPreference::Auto,
    EncoderPreference::Nvenc,
    EncoderPreference::QuickSync,
    EncoderPreference::Amf,
    EncoderPreference::Software,
];

const UNIT_SECONDS: i32 = 0;
const UNIT_MINUTES: i32 = 1;

/// Fills the settings page from a config. `default_clips_dir` is shown when
/// the config has no explicit folder so the user sees where clips go.
pub fn populate(
    state: &SettingsState<'_>,
    config: &Config,
    monitors: &[MonitorInfo],
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
