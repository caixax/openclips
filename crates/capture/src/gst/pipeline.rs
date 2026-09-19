//! The live capture pipeline:
//!
//! ```text
//! <platform video head> -> [leaky queue] -> encoder -> h264parse(config-interval=-1)
//!   -> appsink
//! ```
//!
//! plus one audio branch per track (see `audio.rs`). The head (screen or game
//! source, frame rate grid, conversion to what the encoder takes) comes from
//! the platform. Frames leave the pipeline as Annex B access units with
//! parameter sets on every keyframe, so the replay buffer can start a clip
//! at any keyframe. Audio and video share the pipeline clock, so their
//! timestamps are directly comparable.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use openclips_core::capture::CaptureSettings;
use openclips_core::media::{EncodedFrame, StreamInfo, VideoCodec};
use tracing::{error, info, warn};

use super::encoders::{self, EncoderTuning};
use super::{Native, Platform, audio, env_flag, make, running_time};
use crate::backend::FrameSink;
use crate::error::CaptureError;

/// Source element name to source key, shared with the bus watch so that
/// an error can be attributed to one audio device.
type SourceNames = Arc<HashMap<String, String>>;

pub struct CapturePipeline {
    pipeline: gst::Pipeline,
    stop_flag: Arc<AtomicBool>,
    bus_thread: Option<JoinHandle<()>>,
    volumes: HashMap<String, gst::Element>,
    /// What the video head needs alive while the pipeline runs (a hook
    /// session, a portal session). Dropped after the pipeline stops.
    _keepalive: Option<Box<dyn std::any::Any + Send>>,
}

impl CapturePipeline {
    /// Builds and starts the pipeline, returning once the first frame is
    /// out. `cancel` is polled while waiting so a stop from another thread
    /// ends the start early.
    pub fn start(
        native: &Native,
        settings: &CaptureSettings,
        sink: Arc<dyn FrameSink>,
        cancel: &AtomicBool,
    ) -> Result<Self, CaptureError> {
        let first_frame = Arc::new(AtomicBool::new(false));
        let built = build(native, settings, sink.clone(), first_frame.clone(), cancel)?;
        let pipeline = built.pipeline;
        let bus = pipeline
            .bus()
            .ok_or_else(|| CaptureError::PipelineBuild("pipeline has no bus".to_owned()))?;

        // Measured with a game in front: registering the streaming threads
        // with the multimedia scheduler cut repeated frames from about ten
        // percent to none for a percent of a core. OPENCLIPS_MMCSS=0 turns it
        // off for comparison.
        if env_flag("OPENCLIPS_MMCSS", true) {
            bus.set_sync_handler(|_, msg| {
                if let gst::MessageView::StreamStatus(status) = msg.view()
                    && status.type_() == gst::StreamStatusType::Enter
                {
                    Native::streaming_thread_started();
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
        if let Err(err) = wait_for_first_frame(
            &bus,
            &first_frame,
            &built.source_names,
            built.first_frame_timeout,
            cancel,
        ) {
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
            built.description,
            settings.audio_tracks.len()
        );

        let stop_flag = Arc::new(AtomicBool::new(false));
        let bus_thread = spawn_bus_watch(bus, stop_flag.clone(), sink, built.source_names);
        Ok(Self {
            pipeline,
            stop_flag,
            bus_thread: Some(bus_thread),
            volumes: built.volumes,
            _keepalive: built.keepalive,
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
    cancel: &AtomicBool,
) -> Result<(), CaptureError> {
    let deadline = Instant::now() + timeout;
    let poll = gst::ClockTime::from_mseconds(50);
    while !first_frame.load(Ordering::SeqCst) {
        if cancel.load(Ordering::SeqCst) {
            return Err(CaptureError::Cancelled);
        }
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

struct Built {
    pipeline: gst::Pipeline,
    volumes: HashMap<String, gst::Element>,
    source_names: SourceNames,
    keepalive: Option<Box<dyn std::any::Any + Send>>,
    first_frame_timeout: Duration,
    description: String,
}

fn build(
    native: &Native,
    settings: &CaptureSettings,
    sink: Arc<dyn FrameSink>,
    first_frame: Arc<AtomicBool>,
    cancel: &AtomicBool,
) -> Result<Built, CaptureError> {
    let spec = encoders::spec_for(&settings.encoder.element)
        .ok_or_else(|| CaptureError::MissingElement(settings.encoder.element.clone()))?;
    let head = native.video_head(settings, spec, sink.clone(), cancel)?;

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
    let mut chain: Vec<gst::Element> = head.elements;
    // A few frames of slack between capture and encode: the oldest frame
    // goes when the encoder stalls longer than that instead of the source
    // waiting on it. Measured alongside MMCSS above. OPENCLIPS_QUEUE=0
    // removes it for comparison.
    if env_flag("OPENCLIPS_QUEUE", true) {
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
    // A head whose frames live on a GPU device of its own publishes it, so
    // every element picks that device up and none copies frames to another.
    if head.system_clock {
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
    }
    if let Some(context) = &head.context {
        pipeline.set_context(context);
    }

    let mut volumes = HashMap::new();
    let mut source_names = HashMap::new();
    for (index, plan) in settings.audio_tracks.iter().enumerate() {
        let branch = audio::build_track(
            native,
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
        keepalive: head.keepalive,
        first_frame_timeout: head.first_frame_timeout,
        description: head.description,
    })
}

struct StreamTracker {
    sink: Arc<dyn FrameSink>,
    encoder: String,
    current: Option<StreamInfo>,
    /// The caps the current info was read from; the same caps arrive with
    /// every frame, and comparing them is cheaper than rebuilding the info.
    caps: Option<gst::Caps>,
}

impl StreamTracker {
    fn update(&mut self, caps: &gst::CapsRef) {
        if self.caps.as_deref().is_some_and(|known| known == caps) {
            return;
        }
        self.caps = Some(caps.to_owned());
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
        // Every caps change goes to the sink, also when size and rate
        // stayed the same: the encoded parameters changed in some other
        // way (aspect, colour), and the ring and the recorder must not
        // join frames across it.
        if self.current.as_ref() != Some(&info) {
            info!(
                "stream: {}x{} @ {}/{} fps via {}",
                width, height, fps_num, fps_den, self.encoder
            );
        } else {
            info!("stream parameters changed: {caps}");
        }
        self.sink.on_stream(info.clone());
        self.current = Some(info);
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
        caps: None,
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
