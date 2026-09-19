//! WASAPI audio: device enumeration and the source element of one audio
//! source. Playback devices are captured through WASAPI loopback, so game
//! and desktop sound come from the same element as microphones do.

use gstreamer as gst;
use gstreamer::prelude::*;
use openclips_core::capture::{
    AudioDeviceInfo, AudioDeviceKind, AudioSourceSettings, DEFAULT_AUDIO_DEVICE_ID,
};

use crate::error::CaptureError;
use crate::gst::props;

/// Lists capture endpoints: every playback device (as a loopback source)
/// and every recording device. The system defaults are reported once with
/// the [`DEFAULT_AUDIO_DEVICE_ID`] so that a config can follow them.
pub fn list_devices() -> Result<Vec<AudioDeviceInfo>, CaptureError> {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Audio/Source"), None);
    monitor
        .start()
        .map_err(|e| CaptureError::PipelineBuild(format!("audio device monitor: {e}")))?;
    let devices = monitor.devices();
    monitor.stop();

    let mut found: Vec<AudioDeviceInfo> = Vec::new();
    for device in devices {
        let Some(props) = device.properties() else {
            continue;
        };
        if props.get::<String>("device.api").ok().as_deref() != Some("wasapi2") {
            continue;
        }
        let loopback = props
            .get::<bool>("wasapi2.device.loopback")
            .unwrap_or(false);
        let kind = if loopback {
            AudioDeviceKind::Output
        } else {
            AudioDeviceKind::Input
        };
        let raw_id = props.get::<String>("device.id").unwrap_or_default();
        let name = device.display_name().to_string();
        let is_default = name.starts_with("Default Audio");
        let id = if is_default {
            DEFAULT_AUDIO_DEVICE_ID.to_owned()
        } else {
            raw_id
        };
        let name = if is_default {
            match kind {
                AudioDeviceKind::Output => "Default output (follows Windows)".to_owned(),
                AudioDeviceKind::Input => "Default microphone (follows Windows)".to_owned(),
                AudioDeviceKind::Application => name.clone(),
            }
        } else {
            name
        };
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

/// A `wasapi2src` set up for `source`.
pub fn make_source(source: &AudioSourceSettings, name: &str) -> Result<gst::Element, CaptureError> {
    let src = gst::ElementFactory::make("wasapi2src")
        .name(name)
        .build()
        .map_err(|_| CaptureError::MissingElement("wasapi2src".to_owned()))?;
    props::set_bool(&src, "low-latency", true);
    match source.kind {
        AudioDeviceKind::Output => {
            src.set_property("loopback", true);
            if source.id != DEFAULT_AUDIO_DEVICE_ID {
                src.set_property("device", &source.id);
            } else if source.process != 0 {
                // Leave the application that has its own track out of
                // the desktop mix. Process loopback only exists for the
                // default render device.
                src.set_property_from_str("loopback-mode", "exclude-process-tree");
                src.set_property("loopback-target-pid", source.process);
            }
        }
        AudioDeviceKind::Input => {
            if source.id != DEFAULT_AUDIO_DEVICE_ID {
                src.set_property("device", &source.id);
            }
        }
        AudioDeviceKind::Application => {
            if source.process == 0 {
                return Err(CaptureError::AudioSource {
                    key: source.key(),
                    message: format!("{} is not running", source.name),
                });
            }
            src.set_property("loopback", true);
            src.set_property_from_str("loopback-mode", "include-process-tree");
            src.set_property("loopback-target-pid", source.process);
        }
    }
    Ok(src)
}
