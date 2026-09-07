//! Game capture through OBS Studio's signed capture hook.
//!
//! Instead of sampling the desktop like `d3d11screencapturesrc`, this injects
//! the OBS `graphics-hook` DLL into a game and reads the backbuffer it copies
//! on every present. Because it sees the game's real frames it does not drop
//! them when the GPU is saturated, the way display capture does. The injected
//! code is entirely OBS's Authenticode signed binary, whitelisted by
//! anti-cheat vendors, so a hooked game treats OpenClips exactly as it treats
//! OBS. Vanguard style kernel anti-cheats that block all injection are the
//! exception; the caller keeps display capture as the fallback.
//!
//! Frames enter the existing pipeline through an `appsrc` and `d3d11upload`
//! (see `pipeline.rs`), so encoding, muxing and the replay ring are shared
//! with display capture unchanged. They stay on the GPU: the capture device
//! is adopted by GStreamer (see `gpu.rs`) and each frame is a texture the
//! upload element passes through. `OPENCLIPS_GAME_CPU=1` forces the older
//! path through system memory for comparison.

mod gpu;
mod inject;
mod protocol;
mod session;
mod window;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tracing::{error, info, warn};

use crate::error::CaptureError;

pub use inject::Hooks;
use session::HookSession;

/// How long to wait for the hook handshake before giving up and letting the
/// caller fall back to display capture. A little above the session's own
/// ready timeout.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(22);

/// A running game capture: the `appsrc` feeding the pipeline plus the thread
/// that injects the hook and pumps frames into it.
pub struct GameCaptureSource {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl GameCaptureSource {
    /// Builds the `appsrc`, injects the hook and blocks until the first frame
    /// is ready or the handshake fails. Returning `Ok` means frames are about
    /// to flow, so the caller can build the rest of the pipeline; returning
    /// `Err` lets it fall back to display capture before anything else is set
    /// up. After a successful start the producer thread keeps pumping frames;
    /// if the hook later dies it calls `on_fatal` so the backend falls back.
    /// The context, when present, must be set on the pipeline so its D3D11
    /// elements share the device the frames live on.
    pub fn start(
        hooks: &Hooks,
        pid: u32,
        fps: i32,
        on_fatal: Arc<dyn Fn(CaptureError) + Send + Sync>,
        cancel: &AtomicBool,
    ) -> Result<(gst::Element, Self, Option<gst::Context>), CaptureError> {
        let hooks = hooks.clone();
        let appsrc = gst_app::AppSrc::builder()
            .name("openclips-gamesrc")
            .format(gst::Format::Time)
            .is_live(true)
            .do_timestamp(true)
            .build();
        // Bound the queue so a stalled encoder drops the oldest frame rather
        // than blocking the capture thread.
        appsrc.set_property("max-buffers", 4u64);
        appsrc.set_property_from_str("leaky-type", "downstream");

        let stop = Arc::new(AtomicBool::new(false));
        let element: gst::Element = appsrc.clone().upcast();
        let worker_src = appsrc;
        let worker_stop = stop.clone();
        // The handshake result comes back on this channel so start() is
        // synchronous while the session itself stays on the producer thread
        // (its D3D and COM objects never cross threads).
        let (ready_tx, ready_rx) = mpsc::channel::<Result<Option<gst::Context>, CaptureError>>();
        let thread = std::thread::Builder::new()
            .name("game-capture".to_owned())
            .spawn(move || run(hooks, pid, fps, worker_src, worker_stop, on_fatal, ready_tx))
            .map_err(|e| {
                CaptureError::GameCapture(format!("could not start the capture thread: {e}"))
            })?;

        let source = Self {
            stop,
            thread: Some(thread),
        };
        // Short waits so a stop during the handshake is honoured promptly;
        // dropping `source` on any error path stops and joins the thread.
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            match ready_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(context)) => return Ok((element, source, context)),
                Ok(Err(err)) => return Err(err),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(CaptureError::GameCapture(
                        "the capture thread ended before the handshake".to_owned(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if cancel.load(Ordering::SeqCst) {
                return Err(CaptureError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(CaptureError::GameCapture(
                    "the capture hook did not respond in time".to_owned(),
                ));
            }
        }
    }
}

impl Drop for GameCaptureSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    hooks: Hooks,
    pid: u32,
    fps: i32,
    appsrc: gst_app::AppSrc,
    stop: Arc<AtomicBool>,
    on_fatal: Arc<dyn Fn(CaptureError) + Send + Sync>,
    ready: mpsc::Sender<Result<Option<gst::Context>, CaptureError>>,
) {
    let fps = fps.max(1);
    let frame_interval_ns = 1_000_000_000u64 / fps as u64;

    // The handshake. Any failure is reported once, through the ready channel
    // (start() is still waiting) rather than on_fatal.
    let session = window::find_for_pid(pid).and_then(|target| {
        let offsets = hooks.graphics_offsets(target.is_64bit)?;
        HookSession::start(&hooks, &target, &offsets, frame_interval_ns)
    });
    let use_cpu = std::env::var("OPENCLIPS_GAME_CPU").as_deref() == Ok("1");
    let mut sink = match session {
        Ok(session) => {
            let gpu = !use_cpu && session.has_gpu_path();
            let _ = ready.send(Ok(if gpu { session.gpu_context() } else { None }));
            session
        }
        Err(err) => {
            error!("game capture could not start: {err}");
            let _ = ready.send(Err(err));
            return;
        }
    };
    drop(ready);

    // Set the caps from the negotiated stream before the first buffer.
    let gpu = !use_cpu && sink.has_gpu_path();
    let fields = gst::Caps::builder("video/x-raw")
        .field("format", sink.format())
        .field("width", sink.width() as i32)
        .field("height", sink.height() as i32)
        .field("framerate", gst::Fraction::new(fps, 1));
    let caps = if gpu {
        fields.features(["memory:D3D11Memory"]).build()
    } else {
        fields.build()
    };
    appsrc.set_caps(Some(&caps));
    info!(
        "game capture frames stay on the {}",
        if gpu { "GPU" } else { "CPU (system memory)" }
    );

    // System memory path: frames are eight megabytes and more, so a pool
    // hands the same few buffers round instead of allocating one per frame.
    // The upload element returns them as soon as the copy to the GPU is done.
    let pool = if gpu {
        None
    } else {
        match frame_pool(&caps, sink.frame_bytes()) {
            Ok(pool) => Some(pool),
            Err(err) => {
                error!("{err}");
                let _ = appsrc.end_of_stream();
                on_fatal(err);
                return;
            }
        }
    };

    // The producer competes with the game for the processor; the same
    // scheduling class as the streaming threads keeps its pace.
    super::pipeline::raise_streaming_thread();

    let period = Duration::from_nanos(frame_interval_ns);
    let mut frames: u64 = 0;
    let mut skipped: u64 = 0;
    // Reads are due on an absolute grid. Sleeping for "period minus the
    // work" drifts by the sleep overshoot (a millisecond or two on Windows)
    // every frame, which starves videorate and makes it repeat frames.
    let mut due = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        // Two kernel waits per check; every half second is plenty.
        if frames.is_multiple_of(30) && !sink.alive() {
            info!("game capture hook stopped for pid {pid}");
            let _ = appsrc.end_of_stream();
            on_fatal(CaptureError::GameCapture(
                "the game stopped presenting frames".to_owned(),
            ));
            return;
        }
        let buffer = match &pool {
            None => sink.read_frame_gpu(),
            Some(pool) => {
                let Ok(mut buffer) = pool.acquire_buffer(None) else {
                    // The pool is inactive: the pipeline is shutting down.
                    return;
                };
                let filled = match buffer.get_mut() {
                    Some(buffer) => {
                        buffer.set_pts(None);
                        buffer.set_dts(None);
                        match buffer.map_writable() {
                            Ok(mut map) => sink.read_frame_into(map.as_mut_slice()),
                            Err(_) => Err(CaptureError::GameCapture(
                                "could not map a frame buffer".to_owned(),
                            )),
                        }
                    }
                    None => Err(CaptureError::GameCapture(
                        "the frame buffer is shared".to_owned(),
                    )),
                };
                filled.map(|()| Some(buffer))
            }
        };
        match buffer {
            Ok(Some(buffer)) => {
                if let Err(err) = appsrc.push_buffer(buffer) {
                    warn!("game capture pipeline stopped accepting frames: {err:?}");
                    return;
                }
            }
            Ok(None) => skipped += 1,
            Err(err) => {
                error!("{err}");
                let _ = appsrc.end_of_stream();
                on_fatal(err);
                return;
            }
        }
        frames += 1;
        due += period;
        wait_until(due);
        // Far behind (the game froze): start the grid over instead of
        // bursting to catch up.
        if Instant::now() > due + period * 2 {
            due = Instant::now();
        }
    }
    let _ = appsrc.end_of_stream();
    if let Some(pool) = pool {
        let _ = pool.set_active(false);
    }
    if skipped > 0 {
        warn!("game capture skipped {skipped} of {frames} frames waiting for a free texture");
    }
}

/// Sleeps until `due`, spinning through the last millisecond because the
/// scheduler wakes late by about that much.
fn wait_until(due: Instant) {
    let spin = Duration::from_micros(700);
    loop {
        let now = Instant::now();
        if now >= due {
            return;
        }
        let left = due - now;
        if left > spin {
            std::thread::sleep(left - spin);
        } else {
            std::hint::spin_loop();
        }
    }
}

/// A pool of a few frame sized buffers for the capture thread.
fn frame_pool(caps: &gst::Caps, size: usize) -> Result<gst::BufferPool, CaptureError> {
    let pool = gst::BufferPool::new();
    let mut config = pool.config();
    config.set_params(Some(caps), size as u32, 2, 8);
    pool.set_config(config).map_err(|e| {
        CaptureError::GameCapture(format!("could not configure the frame pool: {e}"))
    })?;
    pool.set_active(true)
        .map_err(|e| CaptureError::GameCapture(format!("could not start the frame pool: {e}")))?;
    Ok(pool)
}
