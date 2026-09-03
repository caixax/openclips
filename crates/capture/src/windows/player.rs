//! In app playback: `playbin3` with an `appsink` video sink that hands RGBA
//! frames to the UI. Audio goes to the default output. Frames are scaled to
//! at most [`MAX_FRAME_WIDTH`] on the way out so the UI never copies more
//! than a preview needs.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tracing::{info, warn};

use super::encoders;
use super::media::file_uri;
use crate::backend::{Player, PlayerSink, VideoFrame};
use crate::error::CaptureError;

const MAX_FRAME_WIDTH: i32 = 1280;

pub struct GstPlayer {
    playbin: gst::Element,
    sink: Arc<dyn PlayerSink>,
    stop_flag: Arc<AtomicBool>,
    bus_thread: Option<JoinHandle<()>>,
    playing: bool,
}

impl GstPlayer {
    pub fn new(sink: Arc<dyn PlayerSink>) -> Result<Self, CaptureError> {
        let playbin = gst::ElementFactory::make("playbin3")
            .build()
            .map_err(|_| CaptureError::MissingElement("playbin3".to_owned()))?;
        let video_sink = build_video_sink(sink.clone())?;
        playbin.set_property("video-sink", &video_sink);
        Ok(Self {
            playbin,
            sink,
            stop_flag: Arc::new(AtomicBool::new(false)),
            bus_thread: None,
            playing: false,
        })
    }

    fn start_bus_watch(&mut self) {
        self.stop_bus_watch();
        let Some(bus) = self.playbin.bus() else {
            return;
        };
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag = stop_flag.clone();
        let sink = self.sink.clone();
        let thread = std::thread::Builder::new()
            .name("player-bus".to_owned())
            .spawn(move || {
                let poll = gst::ClockTime::from_mseconds(100);
                let kinds = [gst::MessageType::Error, gst::MessageType::Eos];
                while !flag.load(Ordering::SeqCst) {
                    let Some(msg) = bus.timed_pop_filtered(poll, &kinds) else {
                        continue;
                    };
                    match msg.view() {
                        gst::MessageView::Error(err) => {
                            let text = encoders::describe_error(err);
                            warn!("playback error: {text}");
                            sink.on_error(text);
                            return;
                        }
                        gst::MessageView::Eos(_) => {
                            sink.on_finished();
                        }
                        _ => {}
                    }
                }
            });
        match thread {
            Ok(handle) => {
                self.stop_flag = stop_flag;
                self.bus_thread = Some(handle);
            }
            Err(err) => warn!("could not spawn the player bus thread: {err}"),
        }
    }

    fn stop_bus_watch(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(thread) = self.bus_thread.take() {
            let _ = thread.join();
        }
    }
}

fn build_video_sink(sink: Arc<dyn PlayerSink>) -> Result<gst::Element, CaptureError> {
    let make = |name: &str| {
        gst::ElementFactory::make(name)
            .build()
            .map_err(|_| CaptureError::MissingElement(name.to_owned()))
    };
    let scale = make("videoscale")?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .field("width", gst::IntRange::new(16, MAX_FRAME_WIDTH))
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .build();
    let filter = make("capsfilter")?;
    filter.set_property("caps", &caps);
    let appsink = gst_app::AppSink::builder()
        .sync(true)
        .max_buffers(2)
        .drop(true)
        .build();
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |appsink| {
                let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let Some(buffer) = sample.buffer() else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                let Some((width, height)) = sample.caps().and_then(frame_size) else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let expected = (width * height * 4) as usize;
                if map.len() < expected {
                    return Ok(gst::FlowSuccess::Ok);
                }
                sink.on_frame(VideoFrame {
                    width,
                    height,
                    rgba: map.as_slice()[..expected].to_vec(),
                });
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    let bin = gst::Bin::with_name("openclips-video-sink");
    let appsink_element: gst::Element = appsink.upcast();
    bin.add_many([&scale, &filter, &appsink_element])
        .map_err(|e| CaptureError::Playback(e.to_string()))?;
    gst::Element::link_many([&scale, &filter, &appsink_element])
        .map_err(|e| CaptureError::Playback(e.to_string()))?;
    let target = scale
        .static_pad("sink")
        .ok_or_else(|| CaptureError::Playback("videoscale has no sink pad".to_owned()))?;
    let ghost =
        gst::GhostPad::with_target(&target).map_err(|e| CaptureError::Playback(e.to_string()))?;
    bin.add_pad(&ghost)
        .map_err(|e| CaptureError::Playback(e.to_string()))?;
    Ok(bin.upcast())
}

fn frame_size(caps: &gst::CapsRef) -> Option<(u32, u32)> {
    let s = caps.structure(0)?;
    let width = s.get::<i32>("width").ok()?;
    let height = s.get::<i32>("height").ok()?;
    Some((width.max(0) as u32, height.max(0) as u32))
}

impl Player for GstPlayer {
    fn load(&mut self, path: &Path) -> Result<(), CaptureError> {
        self.stop();
        let uri = file_uri(path)?;
        self.playbin.set_property("uri", &uri);
        self.start_bus_watch();
        self.playbin
            .set_state(gst::State::Paused)
            .map_err(|_| CaptureError::Playback("could not open the file".to_owned()))?;
        info!("player loaded {}", path.display());
        Ok(())
    }

    fn play(&mut self) {
        if self.playbin.set_state(gst::State::Playing).is_ok() {
            self.playing = true;
        }
    }

    fn pause(&mut self) {
        if self.playbin.set_state(gst::State::Paused).is_ok() {
            self.playing = false;
        }
    }

    fn seek(&mut self, position: Duration) {
        let _ = self.playbin.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::ClockTime::from_nseconds(position.as_nanos() as u64),
        );
    }

    fn set_volume(&mut self, volume: f64) {
        self.playbin.set_property("volume", volume.clamp(0.0, 2.0));
    }

    fn stop(&mut self) {
        self.stop_bus_watch();
        let _ = self.playbin.set_state(gst::State::Null);
        self.playing = false;
    }

    fn position(&self) -> Option<Duration> {
        self.playbin
            .query_position::<gst::ClockTime>()
            .map(|t| Duration::from_nanos(t.nseconds()))
    }

    fn duration(&self) -> Option<Duration> {
        self.playbin
            .query_duration::<gst::ClockTime>()
            .map(|t| Duration::from_nanos(t.nseconds()))
    }

    fn is_playing(&self) -> bool {
        self.playing
    }
}

impl Drop for GstPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}
