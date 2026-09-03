//! Runtime discovery and configuration of H.264 encoders.
//!
//! Encoders are not test driven ahead of time. An NVENC session opened by a
//! throwaway probe pipeline was observed to make the following real session
//! fail to open, so the capture pipeline itself is the verification: it must
//! deliver a first frame before a start counts as successful, and the caller
//! moves on to the next encoder when it does not.

use gstreamer as gst;
use gstreamer::prelude::*;
use openclips_core::capture::{EncoderInfo, EncoderKind};
use tracing::{debug, info};

use super::props;

/// Candidate encoders in order of preference. `d3d11_input` marks encoders
/// that accept frames on the GPU, which avoids a readback for every frame.
#[derive(Debug, Clone, Copy)]
pub struct EncoderSpec {
    pub kind: EncoderKind,
    pub element: &'static str,
    pub d3d11_input: bool,
}

const CANDIDATES: &[EncoderSpec] = &[
    EncoderSpec {
        kind: EncoderKind::Nvenc,
        element: "nvd3d11h264enc",
        d3d11_input: true,
    },
    EncoderSpec {
        kind: EncoderKind::Nvenc,
        element: "nvh264enc",
        d3d11_input: false,
    },
    EncoderSpec {
        kind: EncoderKind::QuickSync,
        element: "qsvh264enc",
        d3d11_input: true,
    },
    EncoderSpec {
        kind: EncoderKind::Amf,
        element: "amfh264enc",
        d3d11_input: true,
    },
    EncoderSpec {
        kind: EncoderKind::MediaFoundation,
        element: "mfh264enc",
        d3d11_input: false,
    },
    EncoderSpec {
        kind: EncoderKind::Software,
        element: "x264enc",
        d3d11_input: false,
    },
];

pub fn spec_for(element: &str) -> Option<EncoderSpec> {
    CANDIDATES.iter().copied().find(|c| c.element == element)
}

/// Lists the registered candidates, one per kind, best first. The hardware
/// plugins only register their elements when the driver and a capable GPU
/// are present, so registration is a meaningful first filter.
pub fn discover() -> Vec<EncoderInfo> {
    let mut found: Vec<EncoderInfo> = Vec::new();
    for candidate in CANDIDATES {
        if found.iter().any(|e| e.kind == candidate.kind) {
            continue;
        }
        if gst::ElementFactory::find(candidate.element).is_none() {
            debug!("encoder {} is not registered", candidate.element);
            continue;
        }
        info!("encoder {} is registered", candidate.element);
        found.push(EncoderInfo {
            kind: candidate.kind,
            element: candidate.element.to_owned(),
        });
    }
    found
}

/// Waits for a playing pipeline to reach EOS, fail, or time out.
pub fn wait_until_done(pipeline: &gst::Pipeline, timeout: gst::ClockTime) -> Result<(), String> {
    let bus = pipeline.bus().ok_or("pipeline has no bus")?;
    match bus.timed_pop_filtered(timeout, &[gst::MessageType::Eos, gst::MessageType::Error]) {
        Some(msg) => match msg.view() {
            gst::MessageView::Eos(_) => Ok(()),
            gst::MessageView::Error(err) => Err(describe_error(err)),
            _ => Ok(()),
        },
        None => Err("timed out".to_owned()),
    }
}

/// Turns a bus error into one readable line: the error text plus the last,
/// most specific part of the debug string.
pub fn describe_error(err: &gst::message::Error) -> String {
    let debug = err.debug().map(|d| d.to_string()).unwrap_or_default();
    let detail = debug
        .rsplit(':')
        .map(str::trim)
        .find(|part| !part.is_empty())
        .unwrap_or_default();
    if detail.is_empty() {
        err.error().to_string()
    } else {
        format!("{} ({detail})", err.error())
    }
}

pub struct EncoderTuning {
    pub bitrate_kbps: u32,
    pub keyframe_interval: u32,
}

/// Applies a common low latency, constant bitrate, no B-frame profile in
/// each encoder's own vocabulary. Every keyframe carries its parameter sets
/// so that any keyframe can start a clip.
pub fn configure(enc: &gst::Element, spec: EncoderSpec, tuning: &EncoderTuning) {
    let bitrate = i64::from(tuning.bitrate_kbps);
    let gop = i64::from(tuning.keyframe_interval);
    match spec.kind {
        EncoderKind::Nvenc => {
            props::set_number(enc, "bitrate", bitrate);
            props::set_number(enc, "max-bitrate", bitrate);
            props::set_number(enc, "gop-size", gop);
            props::set_number(enc, "bframes", 0);
            props::set_bool(enc, "repeat-sequence-header", true);
            props::set_bool(enc, "zerolatency", true);
            props::set_nick(enc, "rc-mode", "cbr");
            props::set_nick(enc, "preset", "p4");
            props::set_nick(enc, "tune", "low-latency");
        }
        EncoderKind::QuickSync => {
            props::set_number(enc, "bitrate", bitrate);
            props::set_number(enc, "max-bitrate", bitrate);
            props::set_number(enc, "gop-size", gop);
            props::set_number(enc, "b-frames", 0);
            props::set_number(enc, "bframes", 0);
            props::set_nick(enc, "rate-control", "cbr");
            props::set_bool(enc, "low-latency", true);
        }
        EncoderKind::Amf => {
            props::set_number(enc, "bitrate", bitrate);
            props::set_number(enc, "max-bitrate", bitrate);
            props::set_number(enc, "gop-size", gop);
            props::set_number(enc, "max-b-frames", 0);
            props::set_nick(enc, "rate-control", "cbr");
            props::set_nick(enc, "usage", "ultra-low-latency");
        }
        EncoderKind::MediaFoundation => {
            props::set_number(enc, "bitrate", bitrate);
            props::set_number(enc, "max-bitrate", bitrate);
            props::set_number(enc, "gop-size", gop);
            props::set_number(enc, "bframes", 0);
            props::set_nick(enc, "rc-mode", "cbr");
            props::set_bool(enc, "low-latency", true);
        }
        EncoderKind::Software => {
            props::set_number(enc, "bitrate", bitrate);
            props::set_number(enc, "key-int-max", gop);
            props::set_number(enc, "bframes", 0);
            props::set_nick(enc, "tune", "zerolatency");
            props::set_nick(enc, "speed-preset", "veryfast");
            props::set_nick(enc, "pass", "cbr");
        }
    }
}
