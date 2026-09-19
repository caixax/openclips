//! In app playback: `uridecodebin` feeding an `appsink` video sink that hands
//! RGBA frames to the UI, and every audio track of the file through an
//! `audiomixer` to the default output, so a clip with separate desktop,
//! microphone and application tracks is heard as a whole and any track can
//! be silenced on its own. Frames are scaled to at most [`MAX_FRAME_WIDTH`]
//! and converted to RGBA by the platform's chain (on the GPU where there is
//! one), so the UI never touches a full size picture.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tracing::{info, warn};

use super::media::file_uri;
use super::{Native, Platform, encoders, make};
use crate::backend::{Player, PlayerSink};
use crate::error::CaptureError;

const MAX_FRAME_WIDTH: i32 = 1280;
/// Every track is brought to this format before the mixer, so tracks with
/// different rates or channel counts mix without renegotiation.
const MIX_RATE: i32 = 48_000;
const MIX_CHANNELS: i32 = 2;

pub struct GstPlayer {
    sink: Arc<dyn PlayerSink>,
    loaded: Option<Loaded>,
    /// Master volume, kept across files.
    volume: f64,
    playing: bool,
}

/// One open file: its pipeline, the audio mix and the bus watcher.
struct Loaded {
    pipeline: gst::Pipeline,
    audio: Arc<Mutex<AudioMix>>,
    stop_flag: Arc<AtomicBool>,
    bus_thread: Option<JoinHandle<()>>,
}

/// The audio side of the pipeline, built when the first audio track shows
/// up so a silent clip does not wait on a mixer with nothing to mix.
#[derive(Default)]
struct AudioMix {
    mixer: Option<gst::Element>,
    volume: Option<gst::Element>,
    /// Decoder pad name and the mixer pad it feeds, in pad name order,
    /// which is the track order of the file and of the editor.
    tracks: Vec<(String, gst::Pad)>,
    /// Tracks switched off by the editor, by index.
    muted: Vec<bool>,
    level: f64,
}

impl AudioMix {
    fn apply_mutes(&self) {
        for (index, (_, pad)) in self.tracks.iter().enumerate() {
            pad.set_property("mute", self.muted.get(index).copied().unwrap_or(false));
        }
    }

    fn apply_level(&self) {
        if let Some(volume) = &self.volume {
            volume.set_property("volume", self.level);
        }
    }
}

impl GstPlayer {
    pub fn new(sink: Arc<dyn PlayerSink>) -> Result<Self, CaptureError> {
        // Fail early when the elements playback needs are missing, so the
        // app can say playback is unavailable instead of failing per clip.
        for name in ["uridecodebin", "audiomixer", "autoaudiosink", "volume"] {
            make(name)?;
        }
        build_video_sink(sink.clone())?;
        Ok(Self {
            sink,
            loaded: None,
            volume: 1.0,
            playing: false,
        })
    }

    fn build(&self, uri: &str) -> Result<Loaded, CaptureError> {
        let pipeline = gst::Pipeline::with_name("openclips-player");
        let decode = make("uridecodebin")?;
        decode.set_property("uri", uri);
        let video_sink = build_video_sink(self.sink.clone())?;
        pipeline
            .add_many([&decode, &video_sink])
            .map_err(|e| CaptureError::Playback(e.to_string()))?;
        let video_pad = video_sink
            .static_pad("sink")
            .ok_or_else(|| CaptureError::Playback("video sink has no sink pad".to_owned()))?;

        let audio = Arc::new(Mutex::new(AudioMix {
            level: self.volume,
            ..AudioMix::default()
        }));
        let mix = audio.clone();
        let weak = pipeline.downgrade();
        decode.connect_pad_added(move |_, pad| {
            let kind = pad
                .current_caps()
                .and_then(|c| c.structure(0).map(|s| s.name().to_string()))
                .unwrap_or_default();
            if kind.starts_with("video/") {
                if !video_pad.is_linked() && pad.link(&video_pad).is_err() {
                    warn!("could not link the video track of the clip");
                }
            } else if kind.starts_with("audio/")
                && let Some(pipeline) = weak.upgrade()
            {
                let mut mix = mix.lock().unwrap_or_else(|p| p.into_inner());
                if attach_track(&pipeline, &mut mix, pad).is_none() {
                    warn!("could not link audio track {} of the clip", pad.name());
                }
            }
        });
        Ok(Loaded {
            pipeline,
            audio,
            stop_flag: Arc::new(AtomicBool::new(false)),
            bus_thread: None,
        })
    }

    fn start_bus_watch(&mut self) {
        let Some(loaded) = self.loaded.as_mut() else {
            return;
        };
        let Some(bus) = loaded.pipeline.bus() else {
            return;
        };
        let flag = loaded.stop_flag.clone();
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
            Ok(handle) => loaded.bus_thread = Some(handle),
            Err(err) => warn!("could not spawn the player bus thread: {err}"),
        }
    }

    fn pipeline(&self) -> Option<&gst::Pipeline> {
        self.loaded.as_ref().map(|l| &l.pipeline)
    }
}

/// Links a decoded audio track into the mixer, creating the mixer and the
/// output chain with the first track.
fn attach_track(pipeline: &gst::Pipeline, mix: &mut AudioMix, pad: &gst::Pad) -> Option<()> {
    let mut fresh = Vec::new();
    let mixer = match &mix.mixer {
        Some(mixer) => mixer.clone(),
        None => {
            let mixer = make("audiomixer").ok()?;
            let convert = make("audioconvert").ok()?;
            let resample = make("audioresample").ok()?;
            let volume = make("volume").ok()?;
            volume.set_property("volume", mix.level);
            let sink = make("autoaudiosink").ok()?;
            let chain = [mixer.clone(), convert, resample, volume.clone(), sink];
            pipeline.add_many(&chain).ok()?;
            gst::Element::link_many(&chain).ok()?;
            fresh.extend(chain);
            mix.mixer = Some(mixer.clone());
            mix.volume = Some(volume);
            mixer
        }
    };

    let queue = make("queue").ok()?;
    let convert = make("audioconvert").ok()?;
    let resample = make("audioresample").ok()?;
    let format = make("capsfilter").ok()?;
    format.set_property(
        "caps",
        gst::Caps::builder("audio/x-raw")
            .field("format", "F32LE")
            .field("rate", MIX_RATE)
            .field("channels", MIX_CHANNELS)
            .field("layout", "interleaved")
            .build(),
    );
    let branch = [queue.clone(), convert, resample, format.clone()];
    pipeline.add_many(&branch).ok()?;
    gst::Element::link_many(&branch).ok()?;
    let mixer_pad = mixer.request_pad_simple("sink_%u")?;
    format.static_pad("src")?.link(&mixer_pad).ok()?;
    fresh.extend(branch);
    // The mixer gets its state only once it has a pad, so it never runs
    // empty and ends the stream before the first track is linked.
    for element in &fresh {
        element.sync_state_with_parent().ok()?;
    }
    pad.link(&queue.static_pad("sink")?).ok()?;

    mix.tracks.push((pad.name().to_string(), mixer_pad));
    mix.tracks.sort_by(|a, b| a.0.cmp(&b.0));
    mix.apply_mutes();
    Some(())
}

/// The platform's conversion chain (decoded frames in, small RGBA in system
/// memory out) followed by the `appsink` that hands frames to the UI.
fn build_video_sink(sink: Arc<dyn PlayerSink>) -> Result<gst::Element, CaptureError> {
    let mut chain = Native::player_video_chain(MAX_FRAME_WIDTH)?;
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
                sink.on_frame(width, height, &map.as_slice()[..expected]);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    let bin = gst::Bin::with_name("openclips-video-sink");
    chain.push(appsink.upcast());
    let refs: Vec<&gst::Element> = chain.iter().collect();
    bin.add_many(&refs)
        .map_err(|e| CaptureError::Playback(e.to_string()))?;
    gst::Element::link_many(&refs).map_err(|e| CaptureError::Playback(e.to_string()))?;
    let target = chain
        .first()
        .and_then(|first| first.static_pad("sink"))
        .ok_or_else(|| CaptureError::Playback("the video chain has no sink pad".to_owned()))?;
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
        let loaded = self.build(&uri)?;
        self.loaded = Some(loaded);
        self.start_bus_watch();
        let opened = self
            .loaded
            .as_ref()
            .is_some_and(|l| l.pipeline.set_state(gst::State::Paused).is_ok());
        if !opened {
            self.stop();
            return Err(CaptureError::Playback("could not open the file".to_owned()));
        }
        info!("player loaded {}", path.display());
        Ok(())
    }

    fn play(&mut self) {
        if let Some(pipeline) = self.pipeline()
            && pipeline.set_state(gst::State::Playing).is_ok()
        {
            self.playing = true;
        }
    }

    fn pause(&mut self) {
        if let Some(pipeline) = self.pipeline()
            && pipeline.set_state(gst::State::Paused).is_ok()
        {
            self.playing = false;
        }
    }

    fn seek(&mut self, position: Duration) {
        if let Some(pipeline) = self.pipeline() {
            let _ = pipeline.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::ClockTime::from_nseconds(position.as_nanos() as u64),
            );
        }
    }

    fn seek_fast(&mut self, position: Duration) {
        if let Some(pipeline) = self.pipeline() {
            let _ = pipeline.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_NEAREST,
                gst::ClockTime::from_nseconds(position.as_nanos() as u64),
            );
        }
    }

    fn set_volume(&mut self, volume: f64) {
        self.volume = volume.clamp(0.0, 2.0);
        if let Some(loaded) = &self.loaded {
            let mut mix = loaded.audio.lock().unwrap_or_else(|p| p.into_inner());
            mix.level = self.volume;
            mix.apply_level();
        }
    }

    fn set_track_enabled(&mut self, index: usize, enabled: bool) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let mut mix = loaded.audio.lock().unwrap_or_else(|p| p.into_inner());
        if mix.muted.len() <= index {
            mix.muted.resize(index + 1, false);
        }
        mix.muted[index] = !enabled;
        mix.apply_mutes();
    }

    fn stop(&mut self) {
        if let Some(mut loaded) = self.loaded.take() {
            loaded.stop_flag.store(true, Ordering::SeqCst);
            if let Some(thread) = loaded.bus_thread.take() {
                let _ = thread.join();
            }
            let _ = loaded.pipeline.set_state(gst::State::Null);
        }
        self.playing = false;
    }

    fn position(&self) -> Option<Duration> {
        self.pipeline()?
            .query_position::<gst::ClockTime>()
            .map(|t| Duration::from_nanos(t.nseconds()))
    }

    fn duration(&self) -> Option<Duration> {
        self.pipeline()?
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
