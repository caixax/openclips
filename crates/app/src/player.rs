//! Drives the in app player and pushes its frames into the UI.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openclips_capture::{Player, PlayerSink, VideoFrame};
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, Weak};
use tracing::error;

use crate::ui::{MainWindow, PlayerState};

struct SharedState {
    /// A frame is on its way to the UI thread; newer frames are dropped
    /// until it lands so playback never queues up behind the renderer.
    frame_pending: AtomicBool,
    finished: AtomicBool,
    error: Mutex<Option<String>>,
}

struct UiSink {
    window: Weak<MainWindow>,
    shared: Arc<SharedState>,
}

impl PlayerSink for UiSink {
    fn on_frame(&self, frame: VideoFrame) {
        if self.shared.frame_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let shared = self.shared.clone();
        let queued = self.window.upgrade_in_event_loop(move |window| {
            let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                &frame.rgba,
                frame.width,
                frame.height,
            );
            window
                .global::<PlayerState>()
                .set_frame(Image::from_rgba8(buffer));
            shared.frame_pending.store(false, Ordering::Release);
        });
        if queued.is_err() {
            self.shared.frame_pending.store(false, Ordering::Release);
        }
    }

    fn on_finished(&self) {
        self.shared.finished.store(true, Ordering::SeqCst);
    }

    fn on_error(&self, message: String) {
        *self.shared.error.lock().unwrap_or_else(|p| p.into_inner()) = Some(message);
    }
}

pub struct PlayerController {
    player: Box<dyn Player>,
    shared: Arc<SharedState>,
    current: Option<String>,
    /// Pause automatically when playback reaches this point.
    stop_at: Option<Duration>,
}

impl PlayerController {
    pub fn new(
        window: &MainWindow,
        create: impl FnOnce(Arc<dyn PlayerSink>) -> Result<Box<dyn Player>, crate::error::AppError>,
    ) -> Result<Self, crate::error::AppError> {
        let shared = Arc::new(SharedState {
            frame_pending: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            error: Mutex::new(None),
        });
        let sink: Arc<dyn PlayerSink> = Arc::new(UiSink {
            window: window.as_weak(),
            shared: shared.clone(),
        });
        Ok(Self {
            player: create(sink)?,
            shared,
            current: None,
            stop_at: None,
        })
    }

    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    pub fn position(&self) -> Option<Duration> {
        self.player.position()
    }

    /// Plays `from` up to `until`, then pauses.
    pub fn preview(&mut self, from: Duration, until: Duration, state: &PlayerState<'_>) {
        if self.current.is_none() {
            return;
        }
        self.shared.finished.store(false, Ordering::SeqCst);
        self.stop_at = Some(until);
        self.player.seek(from);
        self.player.play();
        state.set_playing(true);
    }

    pub fn open(&mut self, id: &str, path: &Path, state: &PlayerState<'_>) {
        self.shared.finished.store(false, Ordering::SeqCst);
        *self.shared.error.lock().unwrap_or_else(|p| p.into_inner()) = None;
        state.set_frame(Image::default());
        state.set_position(0.0);
        state.set_playing(false);
        match self.player.load(path) {
            Ok(()) => {
                self.current = Some(id.to_owned());
                self.player
                    .set_volume(f64::from(state.get_volume()) / 100.0);
                self.player.play();
                state.set_playing(true);
                state.set_message("".into());
            }
            Err(err) => {
                error!("{err}");
                self.current = None;
                state.set_message(format!("Could not play this clip: {err}").into());
            }
        }
    }

    pub fn toggle(&mut self, state: &PlayerState<'_>) {
        if self.current.is_none() {
            return;
        }
        self.stop_at = None;
        if self.shared.finished.swap(false, Ordering::SeqCst) {
            self.player.seek(Duration::ZERO);
            self.player.play();
        } else if self.player.is_playing() {
            self.player.pause();
        } else {
            self.player.play();
        }
        state.set_playing(self.player.is_playing());
    }

    pub fn seek(&mut self, seconds: f32) {
        if self.current.is_some() {
            self.stop_at = None;
            self.shared.finished.store(false, Ordering::SeqCst);
            self.player.seek(Duration::from_secs_f32(seconds.max(0.0)));
        }
    }

    pub fn set_volume(&mut self, percent: f32) {
        self.player.set_volume(f64::from(percent) / 100.0);
    }

    pub fn stop(&mut self, state: &PlayerState<'_>) {
        self.player.stop();
        self.current = None;
        state.set_playing(false);
        state.set_frame(Image::default());
    }

    /// Refreshes the progress shown in the UI. Called from the status timer.
    pub fn tick(&mut self, state: &PlayerState<'_>) {
        if self.current.is_none() {
            return;
        }
        if let Some(duration) = self.player.duration() {
            state.set_duration(duration.as_secs_f32());
            state.set_duration_text(crate::library::format_duration(duration).into());
        }
        if self.shared.finished.load(Ordering::SeqCst) {
            self.player.pause();
            state.set_playing(false);
            state.set_position(state.get_duration());
            state.set_position_text(state.get_duration_text());
        } else if let Some(position) = self.player.position() {
            if let Some(until) = self.stop_at
                && position >= until
                && self.player.is_playing()
            {
                self.player.pause();
                self.stop_at = None;
            }
            state.set_position(position.as_secs_f32());
            state.set_position_text(crate::library::format_duration(position).into());
            state.set_playing(self.player.is_playing());
        }
        if let Some(message) = self
            .shared
            .error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            state.set_message(format!("Playback error: {message}").into());
            state.set_playing(false);
        }
    }
}
