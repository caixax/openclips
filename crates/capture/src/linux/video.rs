//! The video head. Two sources, picked from the session:
//!
//! ```text
//! Wayland: pipewiresrc (portal ScreenCast) -.
//! X11:     ximagesrc ----------------------+-> capsfilter(fps) -> videorate
//!   -> capsfilter(grid) -> [videoscale] -> videoconvert -> capsfilter(square pixels)
//! ```
//!
//! `ximagesrc` reads the root window, so it sees whatever the X server
//! composites: every window on an X11 session. Under Wayland the root window
//! of XWayland only holds X clients, so a Wayland session goes through the
//! ScreenCast portal, which works the same on every desktop that has one.
//! `OPENCLIPS_CAPTURE=x11` or `=portal` overrides the choice.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use openclips_core::capture::{CaptureSettings, MonitorInfo};
use openclips_core::config::DisplaySelection;
use tracing::{info, warn};

use super::{monitors, portal};
use crate::error::CaptureError;
use crate::gst::{VideoHead, make, props};

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(8);
/// A cast that is going to deliver does so at once; one that lost its
/// negotiation never will, and the sooner the retry the better.
const PORTAL_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(3);

/// Which source a session calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Portal,
    X11,
}

/// The source for this session. A Wayland session is one that says so, or
/// one with a Wayland socket that does not claim to be X11.
fn preferred_source() -> Source {
    match std::env::var("OPENCLIPS_CAPTURE").as_deref() {
        Ok("x11") => return Source::X11,
        Ok("portal") => return Source::Portal,
        _ => {}
    }
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let wayland =
        session == "wayland" || (session != "x11" && std::env::var_os("WAYLAND_DISPLAY").is_some());
    if wayland { Source::Portal } else { Source::X11 }
}

/// A source element, what it captures (for the log) and what has to stay
/// alive while it runs.
type SourcePart = (gst::Element, String, Option<Box<dyn std::any::Any + Send>>);

pub fn build_head(
    settings: &CaptureSettings,
    cancel: &AtomicBool,
) -> Result<VideoHead, CaptureError> {
    if settings.game_capture_pid.is_some() {
        return Err(CaptureError::GameCapture(
            "game capture through a hook only exists on Windows".to_owned(),
        ));
    }
    let fps = settings.fps.max(1) as i32;
    let source = preferred_source();
    info!("screen source for this session: {source:?}");
    if source == Source::Portal {
        match portal::open(settings.show_cursor, cancel) {
            Ok(cast) => return finish_head(portal_source(cast, fps)?, settings, fps, None),
            Err(portal::Failure::Refused(err)) => return Err(err),
            // No portal at all (a bare window manager, WSLg): the X server,
            // if there is one, is the only thing left to read.
            Err(portal::Failure::Unavailable(reason)) => {
                warn!("the ScreenCast portal is unavailable ({reason}), trying X11");
            }
        }
    }
    let (src, description, target) = x11_source(settings)?;
    finish_head((src, description, None), settings, fps, target)
}

/// `pipewiresrc` on the node of the cast. The cast rides along as the
/// keepalive: its descriptor is the connection the element uses.
fn portal_source(cast: portal::Cast, fps: i32) -> Result<SourcePart, CaptureError> {
    let src = make("pipewiresrc")?;
    src.set_property("fd", cast.remote_fd());
    src.set_property("path", cast.node_id.to_string());
    props::set_bool(&src, "do-timestamp", true);
    // Compositors only send a frame when the picture changes. Without this
    // a still desktop produces nothing: no first frame, and a replay buffer
    // that stops growing. The source repeats its last frame instead.
    if !props::set_number(&src, "keepalive-time", i64::from((1000 / fps).max(1))) {
        warn!("this pipewiresrc cannot repeat frames; a still screen will stall the capture");
    }
    props::set_bool(&src, "always-copy", true);
    let description = match cast.size {
        Some((width, height)) => format!("the shared screen ({width}x{height}, PipeWire)"),
        None => "the shared screen (PipeWire)".to_owned(),
    };
    Ok((src, description, Some(Box::new(cast))))
}

fn x11_source(
    settings: &CaptureSettings,
) -> Result<(gst::Element, String, Option<MonitorInfo>), CaptureError> {
    if std::env::var_os("DISPLAY").is_none() {
        return Err(CaptureError::PipelineBuild(
            "there is no screen to capture: no ScreenCast portal and no X11 display".to_owned(),
        ));
    }
    let src = make("ximagesrc")?;
    // Damage tracking sends partial updates, which saves nothing here (the
    // encoder wants whole frames) and stalls when nothing moves.
    src.set_property("use-damage", false);
    src.set_property("show-pointer", settings.show_cursor);
    props::set_bool(&src, "remote", false);

    let target = match &settings.display {
        DisplaySelection::Primary => monitors::enumerate().into_iter().find(|m| m.primary),
        DisplaySelection::Monitor(id) => Some(
            monitors::enumerate()
                .into_iter()
                .find(|m| &m.id == id)
                .ok_or_else(|| CaptureError::MonitorNotFound(id.clone()))?,
        ),
    };
    let description = match &target {
        Some(monitor) => {
            // The whole root window spans every monitor; the region limits
            // it to one. Even sizes, because 4:2:0 video has no odd ones.
            let width = monitor.width & !1;
            let height = monitor.height & !1;
            src.set_property("startx", monitor.x.max(0) as u32);
            src.set_property("starty", monitor.y.max(0) as u32);
            src.set_property("endx", (monitor.x.max(0) as u32 + width).saturating_sub(1));
            src.set_property("endy", (monitor.y.max(0) as u32 + height).saturating_sub(1));
            format!("{} ({}x{}, X11)", monitor.name, width, height)
        }
        None => {
            warn!("no monitor layout from the X server, capturing the whole screen");
            "the whole X11 screen".to_owned()
        }
    };
    Ok((src, description, target))
}

/// Everything after the source, the same for both.
fn finish_head(
    (src, description, keepalive): SourcePart,
    settings: &CaptureSettings,
    fps: i32,
    target: Option<MonitorInfo>,
) -> Result<VideoHead, CaptureError> {
    // `ximagesrc` grabs at the rate the caps ask for, so it is told. A
    // PipeWire node runs at the compositor's pace and announces a variable
    // rate; pinning one there leaves the two sides without a common format.
    let paced_by_caps = keepalive.is_none();
    let mut elements = vec![src];
    if paced_by_caps {
        let rate_filter = make("capsfilter")?;
        rate_filter.set_property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("framerate", gst::Fraction::new(fps, 1))
                .build(),
        );
        elements.push(rate_filter);
    }
    // The source paces itself, but its timestamps drift under load;
    // videorate re-stamps frames onto an exact grid so the ring buffer math
    // and the container frame rate stay honest.
    let rate = make("videorate")?;
    rate.set_property("skip-to-first", true);
    let grid_filter = make("capsfilter")?;
    grid_filter.set_property(
        "caps",
        gst::Caps::builder("video/x-raw")
            .field("framerate", gst::Fraction::new(fps, 1))
            .build(),
    );

    elements.push(rate);
    elements.push(grid_filter);
    // Square pixels always, so the encoded parameters never change with the
    // display mode (see the Windows head for what that breaks).
    let mut out_caps =
        gst::Caps::builder("video/x-raw").field("pixel-aspect-ratio", gst::Fraction::new(1, 1));
    if settings.stretch
        && let Some(monitor) = &target
    {
        let (width, height) = (monitor.width & !1, monitor.height & !1);
        info!("stretching frames to {width}x{height}");
        let scale = make("videoscale")?;
        props::set_bool(&scale, "add-borders", false);
        elements.push(scale);
        out_caps = out_caps
            .field("width", width as i32)
            .field("height", height as i32);
    }
    elements.push(make("videoconvert")?);
    let out_filter = make("capsfilter")?;
    out_filter.set_property("caps", out_caps.build());
    elements.push(out_filter);

    Ok(VideoHead {
        elements,
        context: None,
        // `pipewiresrc` offers the PipeWire graph clock. A pipeline that
        // picks it never sees time advance the way its live audio sources
        // and the frame rate grid expect, and no frame comes out.
        system_clock: !paced_by_caps,
        keepalive,
        first_frame_timeout: if !paced_by_caps {
            PORTAL_FIRST_FRAME_TIMEOUT
        } else {
            FIRST_FRAME_TIMEOUT
        },
        description,
    })
}
