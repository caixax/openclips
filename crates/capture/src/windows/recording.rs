//! Full session recording: a long lived mux pipeline fed frame by frame.
//!
//! The file is written as fragmented MP4 and finalised into a regular MP4 on
//! stop, so a crash mid recording still leaves a playable file.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use openclips_core::clip::ClipFile;
use openclips_core::media::{AudioPacket, AudioTrackInfo, EncodedFrame, StreamInfo, Timestamp};
use tracing::info;

use super::audio;
use super::encoders::wait_until_done;
use super::mux::{h264_caps, to_buffer};
use crate::backend::{Recorder, RecordingSession};
use crate::error::CaptureError;

const FRAGMENT_MS: u32 = 1000;

pub struct Mp4Recorder;

impl Recorder for Mp4Recorder {
    fn start(
        &self,
        stream: &StreamInfo,
        audio: &[AudioTrackInfo],
        path: &Path,
    ) -> Result<Box<dyn RecordingSession>, CaptureError> {
        Mp4Session::open(stream, audio, path).map(|s| Box::new(s) as Box<dyn RecordingSession>)
    }
}

struct AudioInput {
    index: u32,
    appsrc: gst_app::AppSrc,
    duration_ns: u64,
}

pub struct Mp4Session {
    pipeline: gst::Pipeline,
    video: gst_app::AppSrc,
    audio: Vec<AudioInput>,
    path: PathBuf,
    partial: PathBuf,
    frame_duration_ns: u64,
    origin: Option<Timestamp>,
    last: Option<Timestamp>,
    frames: u64,
}

impl Mp4Session {
    fn open(
        stream: &StreamInfo,
        tracks: &[AudioTrackInfo],
        path: &Path,
    ) -> Result<Self, CaptureError> {
        let fail = |reason: String| CaptureError::ClipWrite {
            path: path.to_path_buf(),
            reason,
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| fail(e.to_string()))?;
        }
        let partial = path.with_extension("mp4.part");

        let video = gst_app::AppSrc::builder()
            .caps(&h264_caps(stream))
            .format(gst::Format::Time)
            .is_live(true)
            .max_bytes(0)
            .build();
        let parse = gst::ElementFactory::make("h264parse")
            .build()
            .map_err(|e| fail(e.to_string()))?;
        let mux = gst::ElementFactory::make("mp4mux")
            .property("fragment-duration", FRAGMENT_MS)
            .build()
            .map_err(|e| fail(e.to_string()))?;
        mux.set_property_from_str("fragment-mode", "first-moov-then-finalise");
        let filesink = gst::ElementFactory::make("filesink")
            .property("location", partial.to_string_lossy().as_ref())
            .build()
            .map_err(|e| fail(e.to_string()))?;

        let pipeline = gst::Pipeline::with_name("openclips-recording");
        let src: gst::Element = video.clone().upcast();
        pipeline
            .add_many([&src, &parse, &mux, &filesink])
            .map_err(|e| fail(e.to_string()))?;
        gst::Element::link_many([&src, &parse, &mux, &filesink])
            .map_err(|e| fail(e.to_string()))?;

        let mut audio = Vec::new();
        for track in tracks {
            let appsrc = gst_app::AppSrc::builder()
                .caps(&audio::packet_caps(track))
                .format(gst::Format::Time)
                .is_live(true)
                .max_bytes(0)
                .build();
            let src: gst::Element = appsrc.clone().upcast();
            pipeline.add(&src).map_err(|e| fail(e.to_string()))?;
            src.link(&mux).map_err(|e| fail(e.to_string()))?;
            audio.push(AudioInput {
                index: track.index,
                appsrc,
                duration_ns: audio::packet_duration_ns(track),
            });
        }

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| fail("could not start the recording muxer".to_owned()))?;

        info!(
            "recording started: {} ({} audio track(s))",
            path.display(),
            audio.len()
        );
        Ok(Self {
            pipeline,
            video,
            audio,
            path: path.to_path_buf(),
            partial,
            frame_duration_ns: stream.frame_duration().as_nanos() as u64,
            origin: None,
            last: None,
            frames: 0,
        })
    }

    fn duration(&self) -> Duration {
        match (self.origin, self.last) {
            (Some(origin), Some(last)) => {
                last.saturating_sub(origin) + Duration::from_nanos(self.frame_duration_ns)
            }
            _ => Duration::ZERO,
        }
    }

    fn rejected(&self, what: &str, err: gst::FlowError) -> CaptureError {
        CaptureError::ClipWrite {
            path: self.path.clone(),
            reason: format!("recording muxer rejected {what}: {err:?}"),
        }
    }
}

impl RecordingSession for Mp4Session {
    fn push(&mut self, frame: &EncodedFrame) -> Result<(), CaptureError> {
        let origin = *self.origin.get_or_insert(frame.pts);
        let buffer = to_buffer(frame, origin.nanos(), self.frame_duration_ns);
        self.video
            .push_buffer(buffer)
            .map_err(|e| self.rejected("a frame", e))?;
        self.last = Some(frame.pts);
        self.frames += 1;
        Ok(())
    }

    fn push_audio(&mut self, packet: &AudioPacket) -> Result<(), CaptureError> {
        let Some(origin) = self.origin else {
            return Ok(());
        };
        if packet.pts < origin {
            return Ok(());
        }
        let Some(input) = self.audio.iter().find(|a| a.index == packet.track) else {
            return Ok(());
        };
        let buffer = audio::packet_to_buffer(packet, origin.nanos(), input.duration_ns);
        input
            .appsrc
            .push_buffer(buffer)
            .map_err(|e| self.rejected("an audio packet", e))
            .map(|_| ())
    }

    fn finish(self: Box<Self>) -> Result<ClipFile, CaptureError> {
        let fail = |reason: String| CaptureError::ClipWrite {
            path: self.path.clone(),
            reason,
        };
        let mut result = self
            .video
            .end_of_stream()
            .map(|_| ())
            .map_err(|e| format!("could not finish the stream: {e:?}"));
        for input in &self.audio {
            if result.is_ok() {
                result = input
                    .appsrc
                    .end_of_stream()
                    .map(|_| ())
                    .map_err(|e| format!("could not finish an audio stream: {e:?}"));
            }
        }
        let result =
            result.and_then(|()| wait_until_done(&self.pipeline, gst::ClockTime::from_seconds(60)));
        let _ = self.pipeline.set_state(gst::State::Null);
        if let Err(reason) = result {
            let _ = std::fs::remove_file(&self.partial);
            return Err(fail(reason));
        }
        std::fs::rename(&self.partial, &self.path)
            .map_err(|e| fail(format!("could not finalize file: {e}")))?;

        let bytes = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let duration = self.duration();
        info!(
            "recording finished: {} ({:.1} s, {} frames, {} bytes)",
            self.path.display(),
            duration.as_secs_f64(),
            self.frames,
            bytes
        );
        Ok(ClipFile {
            path: self.path.clone(),
            duration,
            bytes,
            created: SystemTime::now(),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Mp4Session {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
