//! The video head on X11:
//!
//! ```text
//! ximagesrc -> capsfilter(fps) -> videorate -> capsfilter(grid)
//!   -> [videoscale] -> videoconvert -> capsfilter(square pixels)
//! ```
//!
//! `ximagesrc` reads the root window, so it sees whatever the X server
//! composites: every window on an X11 session. Under Wayland the root window
//! of XWayland only holds X clients, which is why a Wayland session needs the
//! portal source instead.

use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use openclips_core::capture::CaptureSettings;
use openclips_core::config::DisplaySelection;
use tracing::{info, warn};

use super::monitors;
use crate::error::CaptureError;
use crate::gst::{VideoHead, make, props};

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(8);

pub fn build_head(settings: &CaptureSettings) -> Result<VideoHead, CaptureError> {
    if settings.game_capture_pid.is_some() {
        return Err(CaptureError::GameCapture(
            "game capture through a hook only exists on Windows".to_owned(),
        ));
    }
    if std::env::var_os("DISPLAY").is_none() {
        return Err(CaptureError::PipelineBuild(
            "no X11 display to capture (DISPLAY is not set)".to_owned(),
        ));
    }
    let fps = settings.fps.max(1) as i32;

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
            format!("{} ({}x{})", monitor.name, width, height)
        }
        None => {
            warn!("no monitor layout from the X server, capturing the whole screen");
            "the whole X11 screen".to_owned()
        }
    };

    let rate_filter = make("capsfilter")?;
    rate_filter.set_property(
        "caps",
        gst::Caps::builder("video/x-raw")
            .field("framerate", gst::Fraction::new(fps, 1))
            .build(),
    );
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

    let mut elements = vec![src, rate_filter, rate, grid_filter];
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
        keepalive: None,
        first_frame_timeout: FIRST_FRAME_TIMEOUT,
        description,
    })
}
