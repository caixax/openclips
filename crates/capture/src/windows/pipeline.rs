//! The live capture pipeline:
//!
//! ```text
//! d3d11screencapturesrc -> capsfilter(fps) -> videorate -> d3d11convert
//!   -> capsfilter(NV12) -> [d3d11download] -> encoder -> h264parse(config-interval=-1)
//!   -> appsink
//! ```
//!
//! plus one audio branch per track (see `audio.rs`). Frames leave the
//! pipeline as Annex B access units with parameter sets on every keyframe,
//! so the replay buffer can start a clip at any keyframe. Audio and video
//! share the pipeline clock, so their timestamps are directly comparable.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use openclips_core::capture::CaptureSettings;
use openclips_core::config::{CaptureApi, DisplaySelection};
use openclips_core::media::{EncodedFrame, StreamInfo, Timestamp, VideoCodec};
use tracing::{error, info, warn};

use super::audio;
use super::encoders::{self, EncoderTuning};
use super::game_capture::GameCaptureSource;
use super::monitors;
use crate::backend::FrameSink;
use crate::error::CaptureError;

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// Game capture must inject the hook and wait for the game to present, which
/// takes longer than a display source's first frame.
const GAME_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(25);

/// Source element name to source key, shared with the bus watch so that
/// an error can be attributed to one audio device.
type SourceNames = Arc<HashMap<String, String>>;

pub struct CapturePipeline {
    pipeline: gst::Pipeline,
    stop_flag: Arc<AtomicBool>,
    bus_thread: Option<JoinHandle<()>>,
    volumes: HashMap<String, gst::Element>,
    /// Kept alive for the pipeline's lifetime: dropping it stops the hook
    /// thread. `None` for display capture.
    _game_source: Option<GameCaptureSource>,
}

impl CapturePipeline {
    pub fn start(
        settings: &CaptureSettings,
        sink: Arc<dyn FrameSink>,
    ) -> Result<Self, CaptureError> {
        let first_frame = Arc::new(AtomicBool::new(false));
        let built = build(settings, sink.clone(), first_frame.clone())?;
        let pipeline = built.pipeline;
        let bus = pipeline
            .bus()
            .ok_or_else(|| CaptureError::PipelineBuild("pipeline has no bus".to_owned()))?;

        if std::env::var("OPENCLIPS_MMCSS").as_deref() == Ok("1") {
            bus.set_sync_handler(|_, msg| {
                if let gst::MessageView::StreamStatus(status) = msg.view()
                    && status.type_() == gst::StreamStatusType::Enter
                {
                    raise_streaming_thread();
                }
                gst::BusSyncReply::Pass
            });
        }
        if let Err(err) = pipeline.set_state(gst::State::Playing) {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(CaptureError::EncoderStart {
                encoder: settings.encoder.element.clone(),
                reason: format!("could not start capture: {err}"),
            });
        }
        let first_frame_timeout = if settings.game_capture_pid.is_some() {
            GAME_FIRST_FRAME_TIMEOUT
        } else {
            FIRST_FRAME_TIMEOUT
        };
        if let Err(err) =
            wait_for_first_frame(&bus, &first_frame, &built.source_names, first_frame_timeout)
        {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(match err {
                CaptureError::Pipeline { message, .. } => CaptureError::EncoderStart {
                    encoder: settings.encoder.element.clone(),
                    reason: message,
                },
                other => other,
            });
        }
        log_negotiated_caps(&pipeline);
        info!(
            "capture started with {} on {} and {} audio track(s)",
            settings.encoder.element,
            describe_display(&settings.display),
            settings.audio_tracks.len()
        );

        let stop_flag = Arc::new(AtomicBool::new(false));
        let bus_thread = spawn_bus_watch(bus, stop_flag.clone(), sink, built.source_names);
        Ok(Self {
            pipeline,
            stop_flag,
            bus_thread: Some(bus_thread),
            volumes: built.volumes,
            _game_source: built.game_source,
        })
    }

    pub fn set_audio_level(&self, source_key: &str, volume: f32, muted: bool) -> bool {
        let Some(element) = self.volumes.get(source_key) else {
            return false;
        };
        element.set_property("volume", f64::from(volume.clamp(0.0, 10.0)));
        element.set_property("mute", muted);
        true
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Err(err) = self.pipeline.set_state(gst::State::Null) {
            warn!("capture pipeline did not stop cleanly: {err}");
        }
        if let Some(thread) = self.bus_thread.take() {
            let _ = thread.join();
        }
        info!("capture stopped");
    }
}

impl Drop for CapturePipeline {
    fn drop(&mut self) {
        if self.bus_thread.is_some() {
            self.shutdown();
        }
    }
}

fn describe_display(display: &DisplaySelection) -> String {
    match display {
        DisplaySelection::Primary => "the primary display".to_owned(),
        DisplaySelection::Monitor(id) => id.clone(),
    }
}

/// Turns a bus error into the matching error value. Errors raised by an
/// audio source element are attributed to that source.
fn classify_error(err: &gst::message::Error, names: &HashMap<String, String>) -> CaptureError {
    let element = err.src().map(|s| s.name().to_string()).unwrap_or_default();
    let message = encoders::describe_error(err);
    if let Some(key) = names.get(&element) {
        return CaptureError::AudioSource {
            key: key.clone(),
            message,
        };
    }
    CaptureError::Pipeline { message, element }
}

/// The first encoded frame is the proof that the source, the GPU path and
/// the encoder all agreed. Until it arrives, an error on the bus means the
/// pipeline is unusable as configured.
fn wait_for_first_frame(
    bus: &gst::Bus,
    first_frame: &AtomicBool,
    names: &HashMap<String, String>,
    timeout: Duration,
) -> Result<(), CaptureError> {
    let deadline = Instant::now() + timeout;
    let poll = gst::ClockTime::from_mseconds(50);
    while !first_frame.load(Ordering::SeqCst) {
        if let Some(msg) =
            bus.timed_pop_filtered(poll, &[gst::MessageType::Error, gst::MessageType::Eos])
        {
            match msg.view() {
                gst::MessageView::Error(err) => return Err(classify_error(err, names)),
                // End of stream before the first frame means the source gave
                // up during startup (game capture signals this when the hook
                // cannot attach), so the caller can fall back.
                gst::MessageView::Eos(_) => {
                    return Err(CaptureError::Pipeline {
                        message: "the capture source stopped before the first frame".to_owned(),
                        element: String::new(),
                    });
                }
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            return Err(CaptureError::Pipeline {
                message: format!("no frame was produced within {} seconds", timeout.as_secs()),
                element: String::new(),
            });
        }
    }
    Ok(())
}

/// Timestamps from different branches only compare as running time, which
/// accounts for each pad's segment. Falls back to the raw timestamp when a
/// sample carries no segment.
pub(super) fn running_time(sample: &gst::Sample, pts: Option<gst::ClockTime>) -> Timestamp {
    let pts = pts.unwrap_or(gst::ClockTime::ZERO);
    let running = sample
        .segment()
        .and_then(|segment| segment.downcast_ref::<gst::ClockTime>())
        .and_then(|segment| segment.to_running_time(pts))
        .unwrap_or(pts);
    Timestamp::from_nanos(running.nseconds())
}

/// Logs the caps on every video src pad so a system memory copy in the
/// chain is visible: every hop before the encoder must carry
/// `memory:D3D11Memory`.
fn log_negotiated_caps(pipeline: &gst::Pipeline) {
    let mut iter = pipeline.iterate_elements();
    while let Ok(Some(element)) = iter.next() {
        for pad in element.src_pads() {
            let Some(caps) = pad.current_caps() else {
                continue;
            };
            let Some(structure) = caps.structure(0) else {
                continue;
            };
            if !structure.name().starts_with("video/") {
                continue;
            }
            let features = caps
                .features(0)
                .map(|f| f.to_string())
                .unwrap_or_else(|| "system memory".to_owned());
            let format = structure
                .get::<&str>("format")
                .map(|f| format!(", {f}"))
                .unwrap_or_default();
            let rate = structure
                .get::<gst::Fraction>("framerate")
                .map(|f| format!(", {}/{}", f.numer(), f.denom()))
                .unwrap_or_default();
            info!(
                "{}:{} -> {} [{features}{format}{rate}]",
                element.name(),
                pad.name(),
                structure.name()
            );
        }
    }
}

/// Runs inside a GStreamer streaming thread when it starts: registers it
/// with the multimedia class scheduler and raises its priority so capture
/// and encode are not starved while a game keeps the machine busy.
#[cfg(windows)]
fn raise_streaming_thread() {
    use windows::Win32::System::Threading::{
        AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
    };
    use windows::core::w;

    // SAFETY: plain Win32 calls on the current thread.
    unsafe {
        let mut index = 0u32;
        if AvSetMmThreadCharacteristicsW(w!("Capture"), &mut index).is_err() {
            warn!("MMCSS registration failed for a streaming thread");
        }
        if SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST).is_err() {
            warn!("could not raise a streaming thread's priority");
        }
    }
}

#[cfg(not(windows))]
fn raise_streaming_thread() {}

fn make(element: &str) -> Result<gst::Element, CaptureError> {
    gst::ElementFactory::make(element)
        .build()
        .map_err(|_| CaptureError::MissingElement(element.to_owned()))
}

struct Built {
    pipeline: gst::Pipeline,
    volumes: HashMap<String, gst::Element>,
    source_names: SourceNames,
    game_source: Option<GameCaptureSource>,
}

fn build(
    settings: &CaptureSettings,
    sink: Arc<dyn FrameSink>,
    first_frame: Arc<AtomicBool>,
) -> Result<Built, CaptureError> {
    let spec = encoders::spec_for(&settings.encoder.element)
        .ok_or_else(|| CaptureError::MissingElement(settings.encoder.element.clone()))?;

    let fps = settings.fps.max(1) as i32;
    // The head of the chain differs by source; both feed NV12 D3D11 frames
    // into the shared convert and encode tail below.
    let (head, game_source): (Vec<gst::Element>, Option<GameCaptureSource>) =
        match settings.game_capture_pid {
            Some(pid) => build_game_head(pid, fps, sink.clone())?,
            None => (build_display_head(settings, fps)?, None),
        };

    let convert = make("d3d11convert")?;
    let mut nv12_caps = gst::Caps::builder("video/x-raw")
        .features(["memory:D3D11Memory"])
        .field("format", "NV12");
    // Stretching: every frame is scaled to the display's desktop size, so a
    // 4:3 fullscreen mode fills the 16:9 frame the way the monitor shows it.
    if settings.stretch
        && let Some((width, height)) = stretch_target(&settings.display)
    {
        info!("stretching frames to {width}x{height}");
        super::props::set_bool(&convert, "add-borders", false);
        nv12_caps = nv12_caps
            .field("width", width as i32)
            .field("height", height as i32);
    }
    let nv12_caps = nv12_caps.build();
    let nv12_filter = make("capsfilter")?;
    nv12_filter.set_property("caps", &nv12_caps);

    let enc = make(spec.element)?;
    encoders::configure(
        &enc,
        spec,
        &EncoderTuning {
            bitrate_kbps: settings.bitrate_kbps,
            keyframe_interval: settings.keyframe_interval,
        },
    );

    let parse = make("h264parse")?;
    parse.set_property("config-interval", -1i32);

    let out_caps = gst::Caps::builder("video/x-h264")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .field("profile", "high")
        .build();
    let appsink = gst_app::AppSink::builder()
        .caps(&out_caps)
        .sync(false)
        .max_buffers(8)
        .drop(false)
        .build();
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(new_sample_handler(
                sink.clone(),
                settings.encoder.element.clone(),
                first_frame,
            ))
            .build(),
    );

    let pipeline = gst::Pipeline::with_name("openclips-capture");
    let mut chain: Vec<gst::Element> = head;
    chain.push(convert);
    chain.push(nv12_filter);
    if !spec.d3d11_input {
        chain.push(make("d3d11download")?);
    }
    // OPENCLIPS_QUEUE=1 decouples capture from encode with a few frames of
    // slack; the oldest frame goes when the encoder stalls longer than that.
    if std::env::var("OPENCLIPS_QUEUE").as_deref() == Ok("1") {
        let queue = make("queue")?;
        queue.set_property("max-size-buffers", 4u32);
        queue.set_property("max-size-bytes", 0u32);
        queue.set_property("max-size-time", 0u64);
        queue.set_property_from_str("leaky", "downstream");
        chain.push(queue);
    }
    chain.push(enc);
    chain.push(parse);
    chain.push(appsink.upcast());

    let refs: Vec<&gst::Element> = chain.iter().collect();
    pipeline
        .add_many(&refs)
        .map_err(|e| CaptureError::PipelineBuild(e.to_string()))?;
    gst::Element::link_many(&refs).map_err(|e| CaptureError::PipelineBuild(e.to_string()))?;

    let mut volumes = HashMap::new();
    let mut source_names = HashMap::new();
    for (index, plan) in settings.audio_tracks.iter().enumerate() {
        let branch = audio::build_track(
            &pipeline,
            index as u32,
            plan,
            settings.audio_bitrate_kbps,
            sink.clone(),
        )?;
        volumes.extend(branch.volumes);
        source_names.extend(branch.source_names);
    }

    Ok(Built {
        pipeline,
        volumes,
        source_names: Arc::new(source_names),
        game_source,
    })
}

/// The desktop resolution of the captured display, even values only.
fn stretch_target(display: &DisplaySelection) -> Option<(u32, u32)> {
    let device = match display {
        DisplaySelection::Primary => monitors::primary_device()?,
        DisplaySelection::Monitor(id) => id.clone(),
    };
    let (width, height) = monitors::desktop_size(&device)?;
    Some((width & !1, height & !1))
}

/// The display capture head: `d3d11screencapturesrc` re-gridded to the output
/// frame rate, all in D3D11 memory.
fn build_display_head(
    settings: &CaptureSettings,
    fps: i32,
) -> Result<Vec<gst::Element>, CaptureError> {
    let src = make("d3d11screencapturesrc")?;
    // Desktop Duplication draws the pointer itself and GStreamer 1.28 reads
    // past the desktop image in that code (ProcessMonoMask) when a game
    // changes the display mode, which kills the process. Graphics Capture
    // leaves the pointer to the compositor, so it is only honoured there.
    let draw_cursor = settings.show_cursor && settings.api != CaptureApi::DesktopDuplication;
    if settings.show_cursor && !draw_cursor {
        warn!(
            "the cursor is left out of Desktop Duplication captures (GStreamer crashes drawing it on display mode changes)"
        );
    }
    src.set_property("show-cursor", draw_cursor);
    // The yellow capture border Windows draws for Graphics Capture.
    super::props::set_bool(&src, "show-border", false);
    let api = match settings.api {
        CaptureApi::DesktopDuplication => "dxgi",
        CaptureApi::GraphicsCapture => "wgc",
    };
    if !super::props::set_nick(&src, "capture-api", api) {
        warn!("this GStreamer build has no capture-api selection, using the default");
    }
    match &settings.display {
        DisplaySelection::Primary => src.set_property("monitor-index", -1i32),
        DisplaySelection::Monitor(id) => {
            let monitor = monitors::find_by_id(id)
                .ok_or_else(|| CaptureError::MonitorNotFound(id.clone()))?;
            src.set_property("monitor-handle", monitor.handle as u64);
        }
    }

    // OPENCLIPS_SOURCE_FPS asks the source for another rate (for example the
    // display refresh rate) and lets videorate pick the nearest frame for
    // each output slot.
    let source_fps = std::env::var("OPENCLIPS_SOURCE_FPS")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(fps);
    let rate_caps = gst::Caps::builder("video/x-raw")
        .features(["memory:D3D11Memory"])
        .field("framerate", gst::Fraction::new(source_fps, 1))
        .build();
    let rate_filter = make("capsfilter")?;
    rate_filter.set_property("caps", &rate_caps);

    let (rate, grid_filter) = grid(fps, true)?;
    Ok(vec![src, rate_filter, rate, grid_filter])
}

/// The game capture head: the injected hook feeds an `appsrc` with the game's
/// backbuffer, re-gridded to the output frame rate, then uploaded to D3D11
/// for the shared convert and encode tail.
fn build_game_head(
    pid: u32,
    fps: i32,
    sink: Arc<dyn FrameSink>,
) -> Result<(Vec<gst::Element>, Option<GameCaptureSource>), CaptureError> {
    let on_fatal: Arc<dyn Fn(CaptureError) + Send + Sync> = Arc::new(move |err| sink.on_error(err));
    let (appsrc, source) = GameCaptureSource::start(pid, fps, on_fatal)?;
    let (rate, grid_filter) = grid(fps, false)?;
    let upload = make("d3d11upload")?;
    Ok((vec![appsrc, rate, grid_filter, upload], Some(source)))
}

/// A `videorate` plus a caps filter that pins the output frame rate. The
/// source paces itself, but its timestamps drift under load; videorate
/// re-stamps frames onto an exact grid so the ring buffer math and the
/// container frame rate stay honest. It only touches metadata.
fn grid(fps: i32, d3d11: bool) -> Result<(gst::Element, gst::Element), CaptureError> {
    let rate = make("videorate")?;
    rate.set_property("skip-to-first", true);
    let caps = if d3d11 {
        gst::Caps::builder("video/x-raw")
            .features(["memory:D3D11Memory"])
            .field("framerate", gst::Fraction::new(fps, 1))
            .build()
    } else {
        gst::Caps::builder("video/x-raw")
            .field("framerate", gst::Fraction::new(fps, 1))
            .build()
    };
    let grid_filter = make("capsfilter")?;
    grid_filter.set_property("caps", &caps);
    Ok((rate, grid_filter))
}

struct StreamTracker {
    sink: Arc<dyn FrameSink>,
    encoder: String,
    current: Option<StreamInfo>,
}

impl StreamTracker {
    fn update(&mut self, caps: &gst::CapsRef) {
        let Some(s) = caps.structure(0) else {
            return;
        };
        let width = s.get::<i32>("width").unwrap_or(0).max(0) as u32;
        let height = s.get::<i32>("height").unwrap_or(0).max(0) as u32;
        let (fps_num, fps_den) = s
            .get::<gst::Fraction>("framerate")
            .map(|f| (f.numer().max(0) as u32, f.denom().max(1) as u32))
            .unwrap_or((0, 1));
        let info = StreamInfo {
            codec: VideoCodec::H264,
            width,
            height,
            fps_num,
            fps_den,
            encoder: self.encoder.clone(),
        };
        if self.current.as_ref() != Some(&info) {
            info!(
                "stream: {}x{} @ {}/{} fps via {}",
                width, height, fps_num, fps_den, self.encoder
            );
            self.sink.on_stream(info.clone());
            self.current = Some(info);
        }
    }
}

fn new_sample_handler(
    sink: Arc<dyn FrameSink>,
    encoder: String,
    first_frame: Arc<AtomicBool>,
) -> impl Fn(&gst_app::AppSink) -> Result<gst::FlowSuccess, gst::FlowError> + Send + 'static {
    let tracker = Mutex::new(StreamTracker {
        sink: sink.clone(),
        encoder,
        current: None,
    });
    move |appsink| {
        let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
        if let Some(caps) = sample.caps() {
            tracker
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .update(caps);
        }
        let Some(buffer) = sample.buffer() else {
            return Ok(gst::FlowSuccess::Ok);
        };
        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
        let pts = running_time(&sample, buffer.pts());
        let frame = EncodedFrame {
            pts,
            dts: buffer.dts().map(|_| running_time(&sample, buffer.dts())),
            duration: buffer.duration().map(|d| d.into()),
            keyframe: !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
            data: Arc::from(map.as_slice()),
        };
        sink.on_frame(frame);
        first_frame.store(true, Ordering::SeqCst);
        Ok(gst::FlowSuccess::Ok)
    }
}

fn spawn_bus_watch(
    bus: gst::Bus,
    stop_flag: Arc<AtomicBool>,
    sink: Arc<dyn FrameSink>,
    names: SourceNames,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("capture-bus".to_owned())
        .spawn(move || {
            let poll = gst::ClockTime::from_mseconds(100);
            let kinds = [
                gst::MessageType::Error,
                gst::MessageType::Eos,
                gst::MessageType::Warning,
            ];
            while !stop_flag.load(Ordering::SeqCst) {
                let Some(msg) = bus.timed_pop_filtered(poll, &kinds) else {
                    continue;
                };
                match msg.view() {
                    gst::MessageView::Error(err) => {
                        let error = classify_error(err, &names);
                        error!("capture pipeline error: {error}");
                        sink.on_error(error);
                        return;
                    }
                    gst::MessageView::Eos(_) => {
                        warn!("capture pipeline reached end of stream");
                        sink.on_error(CaptureError::Pipeline {
                            message: "the capture source stopped".to_owned(),
                            element: String::new(),
                        });
                        return;
                    }
                    gst::MessageView::Warning(w) => {
                        warn!("capture pipeline warning: {}", w.error());
                    }
                    _ => {}
                }
            }
        })
        .unwrap_or_else(|err| {
            error!("could not spawn the bus watch thread: {err}");
            std::thread::spawn(|| {})
        })
}
