//! Writes a replay snapshot into an MP4 file:
//!
//! ```text
//! appsrc(byte-stream H.264) -> h264parse -> mp4mux -> filesink
//! appsrc(raw AAC)           -/
//! ```

use std::path::Path;
use std::time::SystemTime;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use openclips_core::clip::ClipFile;
use openclips_core::media::{EncodedFrame, StreamInfo};
use openclips_core::replay::ReplaySnapshot;
use tracing::info;

use super::audio;
use super::encoders::wait_until_done;
use crate::backend::ClipWriter;
use crate::error::CaptureError;

pub struct Mp4Writer;

impl ClipWriter for Mp4Writer {
    fn write_clip(&self, snapshot: &ReplaySnapshot, path: &Path) -> Result<ClipFile, CaptureError> {
        let first = snapshot.frames.first().ok_or(CaptureError::EmptyBuffer)?;
        if !first.keyframe {
            return Err(CaptureError::ClipWrite {
                path: path.to_path_buf(),
                reason: "snapshot does not start at a keyframe".to_owned(),
            });
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| CaptureError::ClipWrite {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })?;
        }

        let partial = path.with_extension("mp4.part");
        let result = write(snapshot, &partial).and_then(|()| {
            std::fs::rename(&partial, path).map_err(|e| format!("could not finalize file: {e}"))
        });
        if let Err(reason) = result {
            let _ = std::fs::remove_file(&partial);
            return Err(CaptureError::ClipWrite {
                path: path.to_path_buf(),
                reason,
            });
        }

        let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        info!(
            "clip written: {} ({:.1} s, {} frames, {} audio track(s), {} bytes)",
            path.display(),
            snapshot.duration.as_secs_f64(),
            snapshot.frames.len(),
            snapshot.audio.len(),
            bytes
        );
        Ok(ClipFile {
            path: path.to_path_buf(),
            duration: snapshot.duration,
            bytes,
            created: SystemTime::now(),
            game: None,
            audio_tracks: snapshot
                .audio
                .iter()
                .map(|a| a.info.label.clone())
                .collect(),
        })
    }
}

fn make(element: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(element)
        .build()
        .map_err(|e| e.to_string())
}

fn write(snapshot: &ReplaySnapshot, path: &Path) -> Result<(), String> {
    let stream = &snapshot.stream;
    let video_src = gst_app::AppSrc::builder()
        .caps(&h264_caps(stream))
        .format(gst::Format::Time)
        .is_live(false)
        .max_bytes(0)
        .build();
    let parse = make("h264parse")?;
    let mux = make("mp4mux")?;
    let filesink = gst::ElementFactory::make("filesink")
        .property("location", path.to_string_lossy().as_ref())
        .build()
        .map_err(|e| e.to_string())?;

    let pipeline = gst::Pipeline::with_name("openclips-mux");
    let src: gst::Element = video_src.clone().upcast();
    pipeline
        .add_many([&src, &parse, &mux, &filesink])
        .map_err(|e| e.to_string())?;
    gst::Element::link_many([&src, &parse, &mux, &filesink]).map_err(|e| e.to_string())?;

    let mut audio_srcs = Vec::new();
    for track in &snapshot.audio {
        if track.packets.is_empty() {
            continue;
        }
        let appsrc = gst_app::AppSrc::builder()
            .caps(&audio::packet_caps(&track.info))
            .format(gst::Format::Time)
            .is_live(false)
            .max_bytes(0)
            .build();
        let src: gst::Element = appsrc.clone().upcast();
        pipeline.add(&src).map_err(|e| e.to_string())?;
        src.link(&mux).map_err(|e| e.to_string())?;
        audio_srcs.push((appsrc, track));
    }

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|_| "could not start the muxer".to_owned())?;

    let origin = snapshot.origin().nanos();
    let frame_duration = stream.frame_duration().as_nanos() as u64;
    for frame in &snapshot.frames {
        let buffer = to_buffer(frame, origin, frame_duration);
        video_src
            .push_buffer(buffer)
            .map_err(|e| format!("muxer rejected a frame: {e:?}"))?;
    }
    video_src
        .end_of_stream()
        .map_err(|e| format!("could not finish the video stream: {e:?}"))?;

    for (appsrc, track) in &audio_srcs {
        let duration = audio::packet_duration_ns(&track.info);
        for packet in &track.packets {
            appsrc
                .push_buffer(audio::packet_to_buffer(packet, origin, duration))
                .map_err(|e| format!("muxer rejected an audio packet: {e:?}"))?;
        }
        appsrc
            .end_of_stream()
            .map_err(|e| format!("could not finish an audio stream: {e:?}"))?;
    }

    let outcome = wait_until_done(&pipeline, gst::ClockTime::from_seconds(60));
    let _ = pipeline.set_state(gst::State::Null);
    outcome
}

pub(super) fn h264_caps(stream: &StreamInfo) -> gst::Caps {
    gst::Caps::builder("video/x-h264")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .field("width", stream.width.max(1) as i32)
        .field("height", stream.height.max(1) as i32)
        .field(
            "framerate",
            gst::Fraction::new(stream.fps_num.max(1) as i32, stream.fps_den.max(1) as i32),
        )
        .build()
}

pub(super) fn to_buffer(frame: &EncodedFrame, origin_ns: u64, duration_ns: u64) -> gst::Buffer {
    let mut buffer = gst::Buffer::from_slice(frame.data.clone());
    if let Some(b) = buffer.get_mut() {
        b.set_pts(gst::ClockTime::from_nseconds(
            frame.pts.nanos().saturating_sub(origin_ns),
        ));
        let dts = frame.dts.unwrap_or(frame.pts);
        b.set_dts(gst::ClockTime::from_nseconds(
            dts.nanos().saturating_sub(origin_ns),
        ));
        b.set_duration(gst::ClockTime::from_nseconds(duration_ns));
        if !frame.keyframe {
            b.set_flags(gst::BufferFlags::DELTA_UNIT);
        }
    }
    buffer
}
