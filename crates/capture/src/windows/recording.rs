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
use openclips_core::media::{EncodedFrame, StreamInfo, Timestamp};
use tracing::info;

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
        path: &Path,
    ) -> Result<Box<dyn RecordingSession>, CaptureError> {
        Mp4Session::open(stream, path).map(|s| Box::new(s) as Box<dyn RecordingSession>)
    }
}

pub struct Mp4Session {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    path: PathBuf,
    partial: PathBuf,
    frame_duration_ns: u64,
    origin: Option<Timestamp>,
    last: Option<Timestamp>,
    frames: u64,
}

impl Mp4Session {
    fn open(stream: &StreamInfo, path: &Path) -> Result<Self, CaptureError> {
        let fail = |reason: String| CaptureError::ClipWrite {
            path: path.to_path_buf(),
            reason,
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| fail(e.to_string()))?;
        }
        let partial = path.with_extension("mp4.part");

        let appsrc = gst_app::AppSrc::builder()
            .caps(&h264_caps(stream))
            .format(gst::Format::Time)
            .is_live(true)
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
        let src: gst::Element = appsrc.clone().upcast();
        pipeline
            .add_many([&src, &parse, &mux, &filesink])
            .map_err(|e| fail(e.to_string()))?;
        gst::Element::link_many([&src, &parse, &mux, &filesink])
            .map_err(|e| fail(e.to_string()))?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| fail("could not start the recording muxer".to_owned()))?;

        info!("recording started: {}", path.display());
        Ok(Self {
            pipeline,
            appsrc,
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
}

impl RecordingSession for Mp4Session {
    fn push(&mut self, frame: &EncodedFrame) -> Result<(), CaptureError> {
        let origin = *self.origin.get_or_insert(frame.pts);
        let buffer = to_buffer(frame, origin.nanos(), self.frame_duration_ns);
        self.appsrc
            .push_buffer(buffer)
            .map_err(|e| CaptureError::ClipWrite {
                path: self.path.clone(),
                reason: format!("recording muxer rejected a frame: {e:?}"),
            })?;
        self.last = Some(frame.pts);
        self.frames += 1;
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<ClipFile, CaptureError> {
        let fail = |reason: String| CaptureError::ClipWrite {
            path: self.path.clone(),
            reason,
        };
        let result = self
            .appsrc
            .end_of_stream()
            .map_err(|e| format!("could not finish the stream: {e:?}"))
            .and_then(|_| wait_until_done(&self.pipeline, gst::ClockTime::from_seconds(60)));
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
