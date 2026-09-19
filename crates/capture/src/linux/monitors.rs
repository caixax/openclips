//! Monitor layout from the X server through XRandR. On a Wayland session
//! without XWayland there is no X server and the list is empty; the portal
//! then lets the user pick the screen in its own dialog.

use openclips_core::capture::MonitorInfo;
use tracing::debug;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::ConnectionExt as _;

pub fn enumerate() -> Vec<MonitorInfo> {
    match query() {
        Ok(monitors) => monitors,
        Err(err) => {
            debug!("no monitor layout from X11: {err}");
            Vec::new()
        }
    }
}

fn query() -> Result<Vec<MonitorInfo>, Box<dyn std::error::Error>> {
    let (conn, screen_index) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_index].root;
    let reply = conn.randr_get_monitors(root, true)?.reply()?;
    let resources = conn.randr_get_screen_resources_current(root)?.reply()?;

    let mut monitors = Vec::new();
    for monitor in reply.monitors {
        let name = conn
            .get_atom_name(monitor.name)?
            .reply()
            .map(|r| String::from_utf8_lossy(&r.name).into_owned())
            .unwrap_or_default();
        // The refresh rate lives on the mode of the CRTC behind the first
        // output of the monitor.
        let mut refresh_hz = 0u32;
        if let Some(output) = monitor.outputs.first()
            && let Ok(info) = conn
                .randr_get_output_info(*output, resources.config_timestamp)?
                .reply()
            && info.crtc != 0
            && let Ok(crtc) = conn
                .randr_get_crtc_info(info.crtc, resources.config_timestamp)?
                .reply()
            && let Some(mode) = resources.modes.iter().find(|m| m.id == crtc.mode)
        {
            let total = u64::from(mode.htotal) * u64::from(mode.vtotal);
            refresh_hz = (u64::from(mode.dot_clock) + total / 2)
                .checked_div(total)
                .unwrap_or(0) as u32;
        }
        monitors.push(MonitorInfo {
            // The output name (DP-1, HDMI-A-0) is stable across sessions.
            id: name.clone(),
            name,
            width: u32::from(monitor.width),
            height: u32::from(monitor.height),
            x: i32::from(monitor.x),
            y: i32::from(monitor.y),
            refresh_hz,
            primary: monitor.primary,
        });
    }
    if !monitors.is_empty() && !monitors.iter().any(|m| m.primary) {
        monitors[0].primary = true;
    }
    Ok(monitors)
}
