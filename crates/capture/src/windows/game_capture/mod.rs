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
//! with display capture unchanged.

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

use inject::Hooks;
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
    /// Confirms the signed hooks are present without starting a capture, so
    /// the backend can tell whether game capture is available at all.
    pub fn available() -> bool {
        Hooks::locate().is_ok()
    }

    /// Builds the `appsrc`, injects the hook and blocks until the first frame
    /// is ready or the handshake fails. Returning `Ok` means frames are about
    /// to flow, so the caller can build the rest of the pipeline; returning
    /// `Err` lets it fall back to display capture before anything else is set
    /// up. After a successful start the producer thread keeps pumping frames;
    /// if the hook later dies it calls `on_fatal` so the backend falls back.
    pub fn start(
        pid: u32,
        fps: i32,
        on_fatal: Arc<dyn Fn(CaptureError) + Send + Sync>,
    ) -> Result<(gst::Element, Self), CaptureError> {
        let hooks = Hooks::locate()?;
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
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), CaptureError>>();
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
        match ready_rx.recv_timeout(HANDSHAKE_TIMEOUT) {
            Ok(Ok(())) => Ok((element, source)),
            Ok(Err(err)) => Err(err),
            // Dropping `source` here stops and joins the producer thread.
            Err(_) => Err(CaptureError::GameCapture(
                "the capture hook did not respond in time".to_owned(),
            )),
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
    ready: mpsc::Sender<Result<(), CaptureError>>,
) {
    let fps = fps.max(1);
    let frame_interval_ns = 1_000_000_000u64 / fps as u64;

    // The handshake. Any failure is reported once, through the ready channel
    // (start() is still waiting) rather than on_fatal.
    let session = window::find_for_pid(pid).and_then(|target| {
        let offsets = hooks.graphics_offsets(target.is_64bit)?;
        HookSession::start(&hooks, &target, &offsets, frame_interval_ns)
    });
    let mut sink = match session {
        Ok(session) => {
            let _ = ready.send(Ok(()));
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
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", sink.format())
        .field("width", sink.width() as i32)
        .field("height", sink.height() as i32)
        .field("framerate", gst::Fraction::new(fps, 1))
        .build();
    appsrc.set_caps(Some(&caps));

    let period = Duration::from_nanos(frame_interval_ns);
    while !stop.load(Ordering::SeqCst) {
        let tick = Instant::now();
        if !sink.alive() {
            info!("game capture hook stopped for pid {pid}");
            let _ = appsrc.end_of_stream();
            on_fatal(CaptureError::GameCapture(
                "the game stopped presenting frames".to_owned(),
            ));
            return;
        }
        match sink.read_frame() {
            Ok(data) => {
                let buffer = gst::Buffer::from_mut_slice(data);
                if let Err(err) = appsrc.push_buffer(buffer) {
                    warn!("game capture pipeline stopped accepting frames: {err:?}");
                    return;
                }
            }
            Err(err) => {
                error!("{err}");
                let _ = appsrc.end_of_stream();
                on_fatal(err);
                return;
            }
        }
        if let Some(rest) = period.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    let _ = appsrc.end_of_stream();
}
