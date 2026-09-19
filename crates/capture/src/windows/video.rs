//! The video head on Windows:
//!
//! ```text
//! d3d11screencapturesrc -> capsfilter(fps) -> videorate -> capsfilter(grid)
//!   -> d3d11convert -> capsfilter(NV12) -> [d3d11download]
//! ```
//!
//! or, for game capture, the hook's `appsrc` in place of the screen source.
//! Frames stay in D3D11 memory up to an encoder that takes them there.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use openclips_core::capture::CaptureSettings;
use openclips_core::config::{CaptureApi, DisplaySelection};
use tracing::{info, warn};

use super::game_capture::{GameCaptureSource, Hooks};
use super::monitors;
use crate::backend::FrameSink;
use crate::error::CaptureError;
use crate::gst::encoders::EncoderSpec;
use crate::gst::{VideoHead, make, props};

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// Game capture must inject the hook and wait for the game to present, which
/// takes longer than a display source's first frame.
const GAME_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(25);

pub fn build_head(
    settings: &CaptureSettings,
    encoder: EncoderSpec,
    hooks: Option<&Hooks>,
    sink: Arc<dyn FrameSink>,
    cancel: &AtomicBool,
) -> Result<VideoHead, CaptureError> {
    let fps = settings.fps.max(1) as i32;
    // The start of the chain differs by source; both feed D3D11 frames into
    // the convert below.
    let (mut elements, game_source, context) = match settings.game_capture_pid {
        Some(pid) => {
            let hooks = hooks.ok_or_else(|| {
                CaptureError::GameCapture("the capture hook binaries are missing".to_owned())
            })?;
            build_game_head(hooks, pid, fps, sink, cancel)?
        }
        None => (build_display_head(settings, fps)?, None, None),
    };

    let convert = make("d3d11convert")?;
    // Square pixels always. When a game switches the display to a 4:3
    // mode and the frames are stretched to the desktop size, the converter
    // would otherwise keep the picture's shape through a pixel aspect
    // ratio, which both undoes the stretch and changes the encoded
    // parameters mid stream; a muxer cannot take that inside one file.
    let mut nv12_caps = gst::Caps::builder("video/x-raw")
        .features(["memory:D3D11Memory"])
        .field("format", "NV12")
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1));
    // Stretching: every frame is scaled to the display's desktop size, so a
    // 4:3 fullscreen mode fills the 16:9 frame the way the monitor shows it.
    if settings.stretch
        && let Some((width, height)) = stretch_target(&settings.display)
    {
        info!("stretching frames to {width}x{height}");
        props::set_bool(&convert, "add-borders", false);
        nv12_caps = nv12_caps
            .field("width", width as i32)
            .field("height", height as i32);
    }
    let nv12_filter = make("capsfilter")?;
    nv12_filter.set_property("caps", nv12_caps.build());
    elements.push(convert);
    elements.push(nv12_filter);
    if !encoder.gpu_input {
        elements.push(make("d3d11download")?);
    }

    let first_frame_timeout = if game_source.is_some() {
        GAME_FIRST_FRAME_TIMEOUT
    } else {
        FIRST_FRAME_TIMEOUT
    };
    Ok(VideoHead {
        elements,
        context,
        keepalive: game_source.map(|source| Box::new(source) as Box<dyn std::any::Any + Send>),
        first_frame_timeout,
        description: match (&settings.game_capture_pid, &settings.display) {
            (Some(pid), _) => format!("the game with process id {pid}"),
            (None, DisplaySelection::Primary) => "the primary display".to_owned(),
            (None, DisplaySelection::Monitor(id)) => id.clone(),
        },
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
    props::set_bool(&src, "show-border", false);
    let api = match settings.api {
        CaptureApi::DesktopDuplication => "dxgi",
        CaptureApi::GraphicsCapture => "wgc",
    };
    if !props::set_nick(&src, "capture-api", api) {
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
/// for the convert.
type GameHead = (
    Vec<gst::Element>,
    Option<GameCaptureSource>,
    Option<gst::Context>,
);

fn build_game_head(
    hooks: &Hooks,
    pid: u32,
    fps: i32,
    sink: Arc<dyn FrameSink>,
    cancel: &AtomicBool,
) -> Result<GameHead, CaptureError> {
    let on_fatal: Arc<dyn Fn(CaptureError) + Send + Sync> = Arc::new(move |err| sink.on_error(err));
    let (appsrc, source, context) = GameCaptureSource::start(hooks, pid, fps, on_fatal, cancel)?;
    // With frames on the GPU the grid caps carry the D3D11 feature, or the
    // filter would refuse them.
    let (rate, grid_filter) = grid(fps, context.is_some())?;
    let upload = make("d3d11upload")?;
    Ok((
        vec![appsrc, rate, grid_filter, upload],
        Some(source),
        context,
    ))
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

/// Runs inside a GStreamer streaming thread when it starts: registers it
/// with the multimedia class scheduler and raises its priority so capture
/// and encode are not starved while a game keeps the machine busy.
pub fn raise_streaming_thread() {
    use windows::Win32::System::Threading::{
        AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
    };
    use windows::core::w;

    thread_local! {
        static REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    // Streaming threads come from a pool and serve one pipeline after
    // another; a second registration of the same thread fails, so it is
    // done once per thread.
    if REGISTERED.with(|r| r.replace(true)) {
        return;
    }
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
