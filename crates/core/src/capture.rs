//! Platform neutral descriptions of what to capture and what is available.
//! Backends consume these; the UI and config produce them.

use std::path::PathBuf;

use crate::config::{CaptureConfig, DisplaySelection, EncoderPreference};

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

/// Everything a backend needs to start producing frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSettings {
    pub display: DisplaySelection,
    pub encoder: EncoderInfo,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Distance between keyframes in frames. Small values make clip starts
    /// precise at a small bitrate cost.
    pub keyframe_interval: u32,
    pub show_cursor: bool,
    /// Optional directory for backend scratch files, for example a RAM disk.
    pub temp_dir: Option<PathBuf>,
}

impl CaptureSettings {
    pub fn from_config(
        config: &CaptureConfig,
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
            temp_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
