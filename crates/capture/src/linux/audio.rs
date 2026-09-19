//! Sound through the PulseAudio protocol. PipeWire systems speak it through
//! `pipewire-pulse`, so one element covers both. What plays on an output is
//! captured from its monitor source, the counterpart of WASAPI loopback.

use gstreamer as gst;
use gstreamer::prelude::*;
use openclips_core::capture::{
    AudioDeviceInfo, AudioDeviceKind, AudioSourceSettings, DEFAULT_AUDIO_DEVICE_ID,
};
use tracing::debug;

use crate::error::CaptureError;

/// What the sound server resolves to the monitor of the current default
/// output, so the capture follows the default when it changes.
const DEFAULT_MONITOR: &str = "@DEFAULT_MONITOR@";

/// Lists capture endpoints: the monitor of every output and every input.
/// The defaults are reported once with [`DEFAULT_AUDIO_DEVICE_ID`] so that a
/// config can follow them.
pub fn list_devices() -> Result<Vec<AudioDeviceInfo>, CaptureError> {
    let mut found = vec![
        AudioDeviceInfo {
            id: DEFAULT_AUDIO_DEVICE_ID.to_owned(),
            name: "Default output (follows the system)".to_owned(),
            kind: AudioDeviceKind::Output,
        },
        AudioDeviceInfo {
            id: DEFAULT_AUDIO_DEVICE_ID.to_owned(),
            name: "Default microphone (follows the system)".to_owned(),
            kind: AudioDeviceKind::Input,
        },
    ];

    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Audio/Source"), None);
    if let Err(err) = monitor.start() {
        // No sound server: the defaults stay listed and fail at start with
        // a message that says so.
        debug!("audio device monitor: {err}");
        return Ok(found);
    }
    let devices = monitor.devices();
    monitor.stop();

    for device in devices {
        // Only the Pulse provider's devices: their element is `pulsesrc` and
        // its `device` property is the source name the config stores.
        let Ok(element) = device.create_element(None) else {
            continue;
        };
        let is_pulse = element
            .factory()
            .is_some_and(|factory| factory.name() == "pulsesrc");
        if !is_pulse {
            continue;
        }
        let Some(id) = element.property::<Option<String>>("device") else {
            continue;
        };
        let is_monitor = device
            .properties()
            .and_then(|p| p.get::<String>("device.class").ok())
            .is_some_and(|class| class == "monitor")
            || id.ends_with(".monitor");
        let kind = if is_monitor {
            AudioDeviceKind::Output
        } else {
            AudioDeviceKind::Input
        };
        let name = device
            .display_name()
            .trim_start_matches("Monitor of ")
            .to_owned();
        if id.is_empty() || found.iter().any(|d| d.id == id && d.kind == kind) {
            continue;
        }
        found.push(AudioDeviceInfo { id, name, kind });
    }
    found.sort_by_key(|d| {
        (
            d.kind != AudioDeviceKind::Output,
            d.id != DEFAULT_AUDIO_DEVICE_ID,
            d.name.to_lowercase(),
        )
    });
    Ok(found)
}

/// A `pulsesrc` set up for `source`.
pub fn make_source(source: &AudioSourceSettings, name: &str) -> Result<gst::Element, CaptureError> {
    if source.kind == AudioDeviceKind::Application {
        return Err(CaptureError::AudioSource {
            key: source.key(),
            message: "application tracks are not available on Linux yet".to_owned(),
        });
    }
    let src = gst::ElementFactory::make("pulsesrc")
        .name(name)
        .build()
        .map_err(|_| CaptureError::MissingElement("pulsesrc".to_owned()))?;
    src.set_property("client-name", "OpenClips");
    match (source.kind, source.id.as_str()) {
        (AudioDeviceKind::Output, DEFAULT_AUDIO_DEVICE_ID) => {
            src.set_property("device", DEFAULT_MONITOR);
        }
        // No device: the server's default source.
        (_, DEFAULT_AUDIO_DEVICE_ID) => {}
        (_, id) => src.set_property("device", id),
    }
    Ok(src)
}
