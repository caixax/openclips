//! WASAPI audio: device enumeration and the per track capture branches.
//!
//! Each track is a set of sources mixed together and encoded once:
//!
//! ```text
//! wasapi2src -> queue -> audioconvert -> audioresample -> volume -> capsfilter -\
//! wasapi2src -> ...                                                            -> audiomixer
//!   -> queue -> audioconvert -> capsfilter -> AAC encoder -> aacparse -> appsink
//! ```
//!
//! Playback devices are captured through WASAPI loopback, so game and
//! desktop sound come from the same element as microphones do.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use openclips_core::capture::{
    AudioDeviceInfo, AudioDeviceKind, AudioTrackPlan, DEFAULT_AUDIO_DEVICE_ID,
};
use openclips_core::media::{AudioCodec, AudioPacket, AudioTrackInfo};
use tracing::{debug, info, warn};

use super::props;
use crate::backend::FrameSink;
use crate::error::CaptureError;

pub const SAMPLE_RATE: i32 = 48_000;
pub const CHANNELS: i32 = 2;
/// Extra time the mixer waits for late sources, in nanoseconds.
const MIXER_LATENCY_NS: u64 = 40_000_000;
const SOURCE_NAME_PREFIX: &str = "audio-src-";

// Media Foundation comes last on purpose: loading an MF encoder into the
// process makes NVENC session creation fail with NV_ENC_ERR_INVALID_VERSION.
const AAC_ENCODERS: &[&str] = &["avenc_aac", "voaacenc", "mfaacenc"];

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

pub fn choose_encoder() -> Option<&'static str> {
    AAC_ENCODERS
        .iter()
        .copied()
        .find(|e| gst::ElementFactory::find(e).is_some())
}

/// Live handles into one track's branch.
pub struct AudioBranch {
    /// Source key to its `volume` element.
    pub volumes: HashMap<String, gst::Element>,
    /// Source element name to source key, for attributing bus errors.
    pub source_names: HashMap<String, String>,
}

fn make(element: &str) -> Result<gst::Element, CaptureError> {
    gst::ElementFactory::make(element)
        .build()
        .map_err(|_| CaptureError::MissingElement(element.to_owned()))
}

fn make_named(element: &str, name: &str) -> Result<gst::Element, CaptureError> {
    gst::ElementFactory::make(element)
        .name(name)
        .build()
        .map_err(|_| CaptureError::MissingElement(element.to_owned()))
}

fn add_and_link(pipeline: &gst::Pipeline, chain: &[gst::Element]) -> Result<(), CaptureError> {
    let refs: Vec<&gst::Element> = chain.iter().collect();
    pipeline
        .add_many(&refs)
        .map_err(|e| CaptureError::PipelineBuild(e.to_string()))?;
    gst::Element::link_many(&refs).map_err(|e| CaptureError::PipelineBuild(e.to_string()))
}

/// Builds one track into `pipeline`. The track index is what packets carry.
pub fn build_track(
    pipeline: &gst::Pipeline,
    index: u32,
    plan: &AudioTrackPlan,
    bitrate_kbps: u32,
    sink: Arc<dyn FrameSink>,
) -> Result<AudioBranch, CaptureError> {
    let encoder_name = choose_encoder().ok_or(CaptureError::NoAudioEncoder)?;

    let mixer = make_named("audiomixer", &format!("audio-mix-{index}"))?;
    props::set_number(&mixer, "latency", MIXER_LATENCY_NS as i64);
    pipeline
        .add(&mixer)
        .map_err(|e| CaptureError::PipelineBuild(e.to_string()))?;

    let mut branch = AudioBranch {
        volumes: HashMap::new(),
        source_names: HashMap::new(),
    };

    let mixed_caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("rate", SAMPLE_RATE)
        .field("channels", CHANNELS)
        .build();

    for (n, source) in plan.sources.iter().enumerate() {
        let key = source.key();
        let src_name = format!("{SOURCE_NAME_PREFIX}{index}-{n}");
        let src = make_named("wasapi2src", &src_name)?;
        props::set_bool(&src, "low-latency", true);
        if source.kind == AudioDeviceKind::Output {
            src.set_property("loopback", true);
        }
        if source.id != DEFAULT_AUDIO_DEVICE_ID {
            src.set_property("device", &source.id);
        }
        let queue = make("queue")?;
        let convert = make("audioconvert")?;
        let resample = make("audioresample")?;
        let volume = make("volume")?;
        volume.set_property("volume", f64::from(source.volume.clamp(0.0, 10.0)));
        volume.set_property("mute", source.muted);
        let filter = make("capsfilter")?;
        filter.set_property("caps", &mixed_caps);

        add_and_link(
            pipeline,
            &[
                src,
                queue,
                convert,
                resample,
                volume.clone(),
                filter.clone(),
            ],
        )?;
        let mixer_pad = mixer
            .request_pad_simple("sink_%u")
            .ok_or_else(|| CaptureError::PipelineBuild("audiomixer has no free pad".to_owned()))?;
        let filter_pad = filter
            .static_pad("src")
            .ok_or_else(|| CaptureError::PipelineBuild("capsfilter has no src pad".to_owned()))?;
        filter_pad.link(&mixer_pad).map_err(|e| {
            CaptureError::PipelineBuild(format!("could not link audio source: {e:?}"))
        })?;

        branch.volumes.insert(key.clone(), volume);
        branch.source_names.insert(src_name, key);
    }

    let out_queue = make("queue")?;
    let out_convert = make("audioconvert")?;
    let out_caps = gst::Caps::builder("audio/x-raw")
        .field("rate", SAMPLE_RATE)
        .field("channels", CHANNELS)
        .build();
    let out_filter = make("capsfilter")?;
    out_filter.set_property("caps", &out_caps);
    let encoder = make(encoder_name)?;
    configure_encoder(&encoder, encoder_name, bitrate_kbps);
    let parse = make("aacparse")?;
    let raw_caps = gst::Caps::builder("audio/mpeg")
        .field("mpegversion", 4i32)
        .field("stream-format", "raw")
        .build();
    let appsink = gst_app::AppSink::builder()
        .caps(&raw_caps)
        .sync(false)
        .max_buffers(64)
        .drop(false)
        .build();
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(sample_handler(index, plan.label.clone(), sink))
            .build(),
    );
    add_and_link(
        pipeline,
        &[
            out_queue.clone(),
            out_convert,
            out_filter,
            encoder,
            parse,
            appsink.upcast(),
        ],
    )?;
    mixer
        .link(&out_queue)
        .map_err(|e| CaptureError::PipelineBuild(e.to_string()))?;

    info!(
        "audio track {index} ({}) with {} source(s) via {encoder_name}",
        plan.label,
        plan.sources.len()
    );
    Ok(branch)
}

/// Media Foundation only accepts a fixed set of bitrates; others take any.
fn configure_encoder(encoder: &gst::Element, name: &str, bitrate_kbps: u32) {
    let bps = i64::from(bitrate_kbps) * 1000;
    let value = if name == "mfaacenc" {
        const ALLOWED: [i64; 4] = [96_000, 128_000, 160_000, 192_000];
        ALLOWED
            .iter()
            .copied()
            .min_by_key(|a| (a - bps).abs())
            .unwrap_or(160_000)
    } else {
        bps
    };
    if !props::set_number(encoder, "bitrate", value) {
        debug!("{name} has no bitrate property");
    }
}

fn sample_handler(
    index: u32,
    label: String,
    sink: Arc<dyn FrameSink>,
) -> impl Fn(&gst_app::AppSink) -> Result<gst::FlowSuccess, gst::FlowError> + Send + 'static {
    let announced: Mutex<Option<AudioTrackInfo>> = Mutex::new(None);
    move |appsink| {
        let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
        if let Some(caps) = sample.caps() {
            let mut announced = announced
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if announced.is_none() {
                match track_info(index, &label, caps) {
                    Some(info) => {
                        info!(
                            "audio track {index}: {} Hz, {} channel(s)",
                            info.sample_rate, info.channels
                        );
                        sink.on_audio_track(info.clone());
                        *announced = Some(info);
                    }
                    None => {
                        warn!("audio track {index} produced caps without codec data");
                        return Ok(gst::FlowSuccess::Ok);
                    }
                }
            }
        }
        let Some(buffer) = sample.buffer() else {
            return Ok(gst::FlowSuccess::Ok);
        };
        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
        sink.on_audio(AudioPacket {
            track: index,
            pts: super::pipeline::running_time(&sample, buffer.pts()),
            duration: buffer.duration().map(|d| d.into()),
            data: Arc::from(map.as_slice()),
        });
        Ok(gst::FlowSuccess::Ok)
    }
}

fn track_info(index: u32, label: &str, caps: &gst::CapsRef) -> Option<AudioTrackInfo> {
    let s = caps.structure(0)?;
    let codec_data = s.get::<gst::Buffer>("codec_data").ok()?;
    let map = codec_data.map_readable().ok()?;
    Some(AudioTrackInfo {
        index,
        label: label.to_owned(),
        codec: AudioCodec::Aac,
        sample_rate: s.get::<i32>("rate").unwrap_or(SAMPLE_RATE).max(1) as u32,
        channels: s.get::<i32>("channels").unwrap_or(CHANNELS).max(1) as u32,
        codec_data: Arc::from(map.as_slice()),
    })
}

/// Caps for feeding stored packets back into a muxer.
pub fn packet_caps(info: &AudioTrackInfo) -> gst::Caps {
    gst::Caps::builder("audio/mpeg")
        .field("mpegversion", 4i32)
        .field("stream-format", "raw")
        .field("rate", info.sample_rate as i32)
        .field("channels", info.channels as i32)
        .field(
            "codec_data",
            gst::Buffer::from_slice(info.codec_data.clone()),
        )
        .build()
}

/// Nominal packet duration: one AAC frame of 1024 samples.
pub fn packet_duration_ns(info: &AudioTrackInfo) -> u64 {
    1_000_000_000u64 * 1024 / u64::from(info.sample_rate.max(1))
}

pub fn packet_to_buffer(
    packet: &AudioPacket,
    origin_ns: u64,
    fallback_duration_ns: u64,
) -> gst::Buffer {
    let mut buffer = gst::Buffer::from_slice(packet.data.clone());
    if let Some(b) = buffer.get_mut() {
        let pts = packet.pts.nanos().saturating_sub(origin_ns);
        b.set_pts(gst::ClockTime::from_nseconds(pts));
        b.set_dts(gst::ClockTime::from_nseconds(pts));
        let duration = packet
            .duration
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(fallback_duration_ns);
        b.set_duration(gst::ClockTime::from_nseconds(duration));
    }
    buffer
}
