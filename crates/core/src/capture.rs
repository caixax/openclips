//! Platform neutral descriptions of what to capture and what is available.
//! Backends consume these; the UI and config produce them.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{AudioConfig, CaptureApi, CaptureConfig, DisplaySelection, EncoderPreference};

/// A physical display as reported by the backend. `id` is stable across
/// sessions on the same machine and is what the config stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub refresh_hz: u32,
    pub primary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncoderKind {
    Nvenc,
    QuickSync,
    Amf,
    MediaFoundation,
    Software,
}

impl EncoderKind {
    pub const fn label(self) -> &'static str {
        match self {
            EncoderKind::Nvenc => "NVIDIA NVENC",
            EncoderKind::QuickSync => "Intel Quick Sync",
            EncoderKind::Amf => "AMD AMF",
            EncoderKind::MediaFoundation => "Media Foundation",
            EncoderKind::Software => "Software (x264)",
        }
    }

    pub const fn is_hardware(self) -> bool {
        !matches!(self, EncoderKind::Software)
    }
}

/// An encoder the backend has verified to work on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderInfo {
    pub kind: EncoderKind,
    /// Backend specific identifier, for example a GStreamer element name.
    pub element: String,
}

/// Picks the encoder to use from the verified list and the user preference.
/// Falls back to the best available encoder when the preferred one is
/// missing, and returns `None` only when the list is empty.
pub fn choose_encoder(
    available: &[EncoderInfo],
    preference: EncoderPreference,
) -> Option<&EncoderInfo> {
    let wanted = match preference {
        EncoderPreference::Auto => None,
        EncoderPreference::Nvenc => Some(EncoderKind::Nvenc),
        EncoderPreference::QuickSync => Some(EncoderKind::QuickSync),
        EncoderPreference::Amf => Some(EncoderKind::Amf),
        EncoderPreference::Software => Some(EncoderKind::Software),
    };
    if let Some(kind) = wanted
        && let Some(found) = available.iter().find(|e| e.kind == kind)
    {
        return Some(found);
    }
    const ORDER: [EncoderKind; 5] = [
        EncoderKind::Nvenc,
        EncoderKind::QuickSync,
        EncoderKind::Amf,
        EncoderKind::MediaFoundation,
        EncoderKind::Software,
    ];
    ORDER
        .iter()
        .find_map(|kind| available.iter().find(|e| e.kind == *kind))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioDeviceKind {
    /// A playback device captured through loopback (game and desktop sound).
    Output,
    /// A recording device such as a microphone.
    Input,
    /// The sound of one application (by executable name), captured on its
    /// own track so it can be muted afterwards.
    Application,
}

impl AudioDeviceKind {
    pub const fn label(self) -> &'static str {
        match self {
            AudioDeviceKind::Output => "Output",
            AudioDeviceKind::Input => "Input",
            AudioDeviceKind::Application => "App",
        }
    }
}

/// Identifier of the system default device of a kind. Backends resolve it
/// at start so that the capture follows the default when it changes.
pub const DEFAULT_AUDIO_DEVICE_ID: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    pub id: String,
    pub name: String,
    pub kind: AudioDeviceKind,
}

/// One device feeding a track, with its mix level.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSourceSettings {
    pub id: String,
    pub name: String,
    pub kind: AudioDeviceKind,
    pub volume: f32,
    pub muted: bool,
    /// For `Application`: the process to capture. For the default output:
    /// a process to leave out of the desktop mix. Zero means none.
    pub process: u32,
}

impl AudioSourceSettings {
    /// Key used to address the source at runtime, unique per kind and id.
    pub fn key(&self) -> String {
        audio_source_key(self.kind, &self.id)
    }
}

pub fn audio_source_key(kind: AudioDeviceKind, id: &str) -> String {
    format!("{}:{}", kind.label().to_ascii_lowercase(), id)
}

/// A set of sources mixed into one encoded track.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioTrackPlan {
    pub label: String,
    pub sources: Vec<AudioSourceSettings>,
}

/// Groups the enabled sources into tracks: one mixed track, or when
/// `separate_tracks` is set, outputs and inputs on their own tracks.
pub fn plan_audio_tracks(audio: &AudioConfig) -> Vec<AudioTrackPlan> {
    if !audio.enabled {
        return Vec::new();
    }
    let sources: Vec<AudioSourceSettings> = audio
        .sources
        .iter()
        .filter(|s| s.enabled)
        .map(|s| AudioSourceSettings {
            id: s.id.clone(),
            name: s.name.clone(),
            kind: s.kind,
            volume: s.volume,
            muted: s.muted,
            process: 0,
        })
        .collect();
    if sources.is_empty() {
        return Vec::new();
    }
    let (apps, devices): (Vec<_>, Vec<_>) = sources
        .into_iter()
        .partition(|s| s.kind == AudioDeviceKind::Application);
    let mut tracks = Vec::new();
    if !audio.separate_tracks {
        if !devices.is_empty() {
            tracks.push(AudioTrackPlan {
                label: "Audio".to_owned(),
                sources: devices,
            });
        }
    } else {
        let (outputs, inputs): (Vec<_>, Vec<_>) = devices
            .into_iter()
            .partition(|s| s.kind == AudioDeviceKind::Output);
        if !outputs.is_empty() {
            tracks.push(AudioTrackPlan {
                label: "Desktop".to_owned(),
                sources: outputs,
            });
        }
        if !inputs.is_empty() {
            tracks.push(AudioTrackPlan {
                label: "Microphone".to_owned(),
                sources: inputs,
            });
        }
    }
    for app in apps {
        tracks.push(AudioTrackPlan {
            label: app.name.clone(),
            sources: vec![app],
        });
    }
    tracks
}

/// Everything a backend needs to start producing frames.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureSettings {
    pub display: DisplaySelection,
    pub encoder: EncoderInfo,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Distance between keyframes in frames. Small values make clip starts
    /// precise at a small bitrate cost.
    pub keyframe_interval: u32,
    pub show_cursor: bool,
    pub api: CaptureApi,
    /// Optional directory for backend scratch files, for example a RAM disk.
    pub temp_dir: Option<PathBuf>,
    pub audio_tracks: Vec<AudioTrackPlan>,
    pub audio_bitrate_kbps: u32,
}

impl CaptureSettings {
    pub fn from_config(
        config: &CaptureConfig,
        audio: &AudioConfig,
        encoder: EncoderInfo,
        temp_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            display: config.display.clone(),
            encoder,
            fps: config.fps,
            bitrate_kbps: config.bitrate_kbps,
            keyframe_interval: config.fps.max(1),
            show_cursor: config.show_cursor,
            api: config.api,
            temp_dir,
            audio_tracks: plan_audio_tracks(audio),
            audio_bitrate_kbps: audio.bitrate_kbps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AudioSourceConfig;

    fn source(id: &str, kind: AudioDeviceKind, enabled: bool) -> AudioSourceConfig {
        AudioSourceConfig {
            id: id.to_owned(),
            name: id.to_owned(),
            kind,
            enabled,
            volume: 1.0,
            muted: false,
        }
    }

    #[test]
    fn plans_one_mixed_track_by_default() {
        let audio = AudioConfig {
            sources: vec![
                source("spk", AudioDeviceKind::Output, true),
                source("mic", AudioDeviceKind::Input, true),
                source("off", AudioDeviceKind::Input, false),
            ],
            ..AudioConfig::default()
        };
        let tracks = plan_audio_tracks(&audio);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].sources.len(), 2);
    }

    #[test]
    fn plans_separate_tracks_by_kind() {
        let audio = AudioConfig {
            separate_tracks: true,
            sources: vec![
                source("spk", AudioDeviceKind::Output, true),
                source("mic", AudioDeviceKind::Input, true),
            ],
            ..AudioConfig::default()
        };
        let tracks = plan_audio_tracks(&audio);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].label, "Desktop");
        assert_eq!(tracks[1].label, "Microphone");

        let only_mic = AudioConfig {
            separate_tracks: true,
            sources: vec![source("mic", AudioDeviceKind::Input, true)],
            ..AudioConfig::default()
        };
        assert_eq!(plan_audio_tracks(&only_mic).len(), 1);
    }

    #[test]
    fn applications_get_their_own_track() {
        let audio = AudioConfig {
            sources: vec![
                source("spk", AudioDeviceKind::Output, true),
                source("discord.exe", AudioDeviceKind::Application, true),
            ],
            ..AudioConfig::default()
        };
        let tracks = plan_audio_tracks(&audio);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].label, "Audio");
        assert_eq!(tracks[1].label, "discord.exe");
        assert_eq!(tracks[1].sources[0].kind, AudioDeviceKind::Application);
    }

    #[test]
    fn disabled_audio_plans_nothing() {
        let audio = AudioConfig {
            enabled: false,
            ..AudioConfig::default()
        };
        assert!(plan_audio_tracks(&audio).is_empty());
        let none = AudioConfig {
            sources: vec![],
            ..AudioConfig::default()
        };
        assert!(plan_audio_tracks(&none).is_empty());
    }

    fn enc(kind: EncoderKind) -> EncoderInfo {
        EncoderInfo {
            kind,
            element: format!("{kind:?}"),
        }
    }

    #[test]
    fn auto_prefers_hardware_in_order() {
        let list = [
            enc(EncoderKind::Software),
            enc(EncoderKind::Amf),
            enc(EncoderKind::Nvenc),
        ];
        let chosen = choose_encoder(&list, EncoderPreference::Auto).expect("some");
        assert_eq!(chosen.kind, EncoderKind::Nvenc);
    }

    #[test]
    fn preference_is_honoured_when_available() {
        let list = [enc(EncoderKind::Nvenc), enc(EncoderKind::Software)];
        let chosen = choose_encoder(&list, EncoderPreference::Software).expect("some");
        assert_eq!(chosen.kind, EncoderKind::Software);
    }

    #[test]
    fn missing_preference_falls_back() {
        let list = [
            enc(EncoderKind::MediaFoundation),
            enc(EncoderKind::Software),
        ];
        let chosen = choose_encoder(&list, EncoderPreference::Nvenc).expect("some");
        assert_eq!(chosen.kind, EncoderKind::MediaFoundation);
        assert!(choose_encoder(&[], EncoderPreference::Auto).is_none());
    }
}
