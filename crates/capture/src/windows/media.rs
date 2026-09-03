//! Reading clip files back: metadata through the discoverer and thumbnails
//! through a short decode pipeline.

use std::path::Path;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_pbutils as gst_pbutils;

use super::encoders::wait_until_done;
use crate::backend::{MediaInfo, MediaTools};
use crate::error::CaptureError;

const PROBE_TIMEOUT_SECONDS: u64 = 15;
const THUMBNAIL_TIMEOUT_SECONDS: u64 = 30;

pub struct GstMediaTools;

fn media_error(path: &Path, reason: impl ToString) -> CaptureError {
    CaptureError::Media {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    }
}

pub(super) fn file_uri(path: &Path) -> Result<String, CaptureError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| media_error(path, e))?
            .join(path)
    };
    gst::glib::filename_to_uri(&absolute, None)
        .map(|uri| uri.to_string())
        .map_err(|e| media_error(path, e))
}

impl MediaTools for GstMediaTools {
    fn probe(&self, path: &Path) -> Result<MediaInfo, CaptureError> {
        let uri = file_uri(path)?;
        let discoverer =
            gst_pbutils::Discoverer::new(gst::ClockTime::from_seconds(PROBE_TIMEOUT_SECONDS))
                .map_err(|e| media_error(path, e))?;
        let info = discoverer
            .discover_uri(&uri)
            .map_err(|e| media_error(path, e))?;
        let (width, height) = info
            .video_streams()
            .first()
            .map(|v| (v.width(), v.height()))
            .unwrap_or((0, 0));
        Ok(MediaInfo {
            duration: info
                .duration()
                .map(|d| Duration::from_nanos(d.nseconds()))
                .unwrap_or_default(),
            width,
            height,
            has_audio: !info.audio_streams().is_empty(),
        })
    }

    fn thumbnail(
        &self,
        path: &Path,
        output: &Path,
        at: Duration,
        max_width: u32,
    ) -> Result<(), CaptureError> {
        if let Some(dir) = output.parent() {
            std::fs::create_dir_all(dir).map_err(|e| media_error(path, e))?;
        }
        let make = |name: &str| {
            gst::ElementFactory::make(name)
                .build()
                .map_err(|_| CaptureError::MissingElement(name.to_owned()))
        };
        let src = gst::ElementFactory::make("filesrc")
            .property("location", path.to_string_lossy().as_ref())
            .build()
            .map_err(|_| CaptureError::MissingElement("filesrc".to_owned()))?;
        let decode = make("decodebin")?;
        let convert = make("videoconvert")?;
        let scale = make("videoscale")?;
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGB")
            .field("width", max_width.max(16) as i32)
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        let filter = make("capsfilter")?;
        filter.set_property("caps", &caps);
        let png = make("pngenc")?;
        png.set_property("snapshot", true);
        let sink = gst::ElementFactory::make("filesink")
            .property("location", output.to_string_lossy().as_ref())
            .build()
            .map_err(|_| CaptureError::MissingElement("filesink".to_owned()))?;

        let pipeline = gst::Pipeline::with_name("openclips-thumbnail");
        pipeline
            .add_many([&src, &decode, &convert, &scale, &filter, &png, &sink])
            .map_err(|e| media_error(path, e))?;
        src.link(&decode).map_err(|e| media_error(path, e))?;
        gst::Element::link_many([&convert, &scale, &filter, &png, &sink])
            .map_err(|e| media_error(path, e))?;

        let convert_pad = convert
            .static_pad("sink")
            .ok_or_else(|| media_error(path, "videoconvert has no sink pad"))?;
        decode.connect_pad_added(move |_, pad| {
            let is_video = pad
                .current_caps()
                .and_then(|c| c.structure(0).map(|s| s.name().starts_with("video/")))
                .unwrap_or(false);
            if is_video && !convert_pad.is_linked() {
                let _ = pad.link(&convert_pad);
            }
        });

        let result = run_thumbnail(&pipeline, at);
        let _ = pipeline.set_state(gst::State::Null);
        result.map_err(|reason| {
            let _ = std::fs::remove_file(output);
            media_error(path, reason)
        })
    }
}

fn run_thumbnail(pipeline: &gst::Pipeline, at: Duration) -> Result<(), String> {
    pipeline
        .set_state(gst::State::Paused)
        .map_err(|_| "could not open the file".to_owned())?;
    let (result, _, _) = pipeline.state(gst::ClockTime::from_seconds(THUMBNAIL_TIMEOUT_SECONDS));
    result.map_err(|_| "could not decode the file".to_owned())?;
    if !at.is_zero() {
        let _ = pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_nseconds(at.as_nanos() as u64),
        );
    }
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|_| "could not decode the file".to_owned())?;
    wait_until_done(
        pipeline,
        gst::ClockTime::from_seconds(THUMBNAIL_TIMEOUT_SECONDS),
    )
}
