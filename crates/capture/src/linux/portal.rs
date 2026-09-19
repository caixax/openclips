//! Screen capture on Wayland: the ScreenCast portal hands out a PipeWire
//! node for the monitor or window the user picks in the desktop's own
//! dialog. Every desktop with a portal backend (KDE, GNOME, wlroots
//! compositors, Hyprland, COSMIC) goes through this one path.
//!
//! The permission is asked once: the portal returns a restore token, which
//! is stored and sent with the next request, and the session then starts
//! without a dialog for as long as the user does not revoke it.

use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, Session};
use futures_lite::future;
use tracing::{debug, info, warn};

use crate::error::CaptureError;

/// A started cast. The PipeWire remote and the portal session last as long
/// as this value; dropping it closes both, which ends the stream and takes
/// the "screen is being shared" indicator of the desktop away.
pub struct Cast {
    pub node_id: u32,
    pub size: Option<(i32, i32)>,
    remote: OwnedFd,
    session: Option<Session<Screencast>>,
}

impl Cast {
    /// The descriptor `pipewiresrc` connects through. Owned by the cast.
    pub fn remote_fd(&self) -> i32 {
        self.remote.as_raw_fd()
    }
}

impl Drop for Cast {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let closed = async_io::block_on(async {
                future::or(
                    async { session.close().await.map_err(|e| e.to_string()) },
                    async {
                        async_io::Timer::after(Duration::from_secs(2)).await;
                        Err("timed out".to_owned())
                    },
                )
                .await
            });
            if let Err(err) = closed {
                debug!("the screen cast session did not close cleanly: {err}");
            }
        }
    }
}

/// Why a cast did not start.
pub enum Failure {
    /// No portal, or one without ScreenCast: another source may still work.
    Unavailable(String),
    /// The portal answered and the answer was no (dialog dismissed, access
    /// denied) or the start was cancelled from our side.
    Refused(CaptureError),
}

/// Opens a cast, showing the desktop's dialog unless a stored token lets the
/// portal skip it. Blocks the calling thread; `cancel` ends the wait.
pub fn open(show_cursor: bool, cancel: &AtomicBool) -> Result<Cast, Failure> {
    async_io::block_on(future::or(negotiate(show_cursor), async {
        while !cancel.load(Ordering::SeqCst) {
            async_io::Timer::after(Duration::from_millis(100)).await;
        }
        Err(Failure::Refused(CaptureError::Cancelled))
    }))
}

async fn negotiate(show_cursor: bool) -> Result<Cast, Failure> {
    let unavailable = |err: ashpd::Error| Failure::Unavailable(err.to_string());
    let proxy = Screencast::new().await.map_err(unavailable)?;
    let session = proxy
        .create_session(Default::default())
        .await
        .map_err(unavailable)?;

    // Only what this portal offers may be asked for, or it closes the
    // session: some backends cannot share windows, some cannot draw the
    // cursor into the stream.
    let mut sources = SourceType::Monitor | SourceType::Window;
    if let Ok(available) = proxy.available_source_types().await {
        sources &= available;
        if sources.is_empty() {
            sources = SourceType::Monitor.into();
        }
    }
    let wanted_cursor = if show_cursor {
        CursorMode::Embedded
    } else {
        CursorMode::Hidden
    };
    let cursor = match proxy.available_cursor_modes().await {
        Ok(available) if available.contains(wanted_cursor) => Some(wanted_cursor),
        Ok(_) => None,
        Err(_) => Some(wanted_cursor),
    };

    let token = load_token();
    let mut options = SelectSourcesOptions::default()
        .set_sources(sources)
        .set_multiple(false)
        .set_persist_mode(PersistMode::ExplicitlyRevoked);
    if let Some(cursor) = cursor {
        options = options.set_cursor_mode(cursor);
    }
    if let Some(token) = token.as_deref() {
        options = options.set_restore_token(token);
    }
    proxy
        .select_sources(&session, options)
        .await
        .map_err(unavailable)?;

    let streams = proxy
        .start(&session, None, Default::default())
        .await
        .map_err(unavailable)?
        .response()
        .map_err(|err| {
            Failure::Refused(CaptureError::PipelineBuild(format!(
                "screen sharing was not allowed: {err}"
            )))
        })?;
    match streams.restore_token() {
        Some(new) if Some(new) != token.as_deref() => store_token(new),
        _ => {}
    }
    let stream = streams.streams().first().ok_or_else(|| {
        Failure::Refused(CaptureError::PipelineBuild(
            "the screen sharing dialog returned nothing to capture".to_owned(),
        ))
    })?;
    let remote = proxy
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .map_err(unavailable)?;
    info!(
        "screen cast started: PipeWire node {}, size {:?}, portal version {}",
        stream.pipe_wire_node_id(),
        stream.size(),
        proxy.version()
    );
    Ok(Cast {
        node_id: stream.pipe_wire_node_id(),
        size: stream.size(),
        remote,
        session: Some(session),
    })
}

/// `$XDG_STATE_HOME/openclips/screencast-token`, by default under
/// `~/.local/state`.
fn token_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(state.join("openclips").join("screencast-token"))
}

fn load_token() -> Option<String> {
    let text = std::fs::read_to_string(token_path()?).ok()?;
    let token = text.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

fn store_token(token: &str) {
    let Some(path) = token_path() else {
        return;
    };
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, token));
    if let Err(err) = written {
        warn!(
            "the screen sharing permission could not be remembered ({}): {err}",
            path.display()
        );
    }
}
