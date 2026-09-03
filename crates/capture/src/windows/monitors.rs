//! Monitor enumeration through GDI. The device name (`\\.\DISPLAY1`) is the
//! stable identifier; the HMONITOR handle is only valid for this session and
//! is what the capture source needs.

use std::mem::size_of;

use openclips_core::capture::MonitorInfo;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW,
    HDC, HMONITOR, MONITORINFOEXW,
};
use windows::core::{BOOL, PCWSTR};

const MONITORINFOF_PRIMARY: u32 = 1;

#[derive(Debug, Clone)]
pub struct Monitor {
    pub handle: isize,
    pub info: MonitorInfo,
}

pub fn enumerate() -> Vec<Monitor> {
    let mut raw: Vec<(HMONITOR, MONITORINFOEXW)> = Vec::new();
    // SAFETY: the callback only runs during this call and receives a pointer
    // to `raw`, which outlives the call.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut raw as *mut Vec<(HMONITOR, MONITORINFOEXW)> as isize),
        );
    }

    raw.into_iter()
        .map(|(handle, info)| {
            let device = String::from_utf16_lossy(&info.szDevice)
                .trim_end_matches('\0')
                .to_owned();
            let rect = info.monitorInfo.rcMonitor;
            let primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;
            Monitor {
                handle: handle.0 as isize,
                info: MonitorInfo {
                    name: friendly_name(&device, primary),
                    width: (rect.right - rect.left).max(0) as u32,
                    height: (rect.bottom - rect.top).max(0) as u32,
                    x: rect.left,
                    y: rect.top,
                    refresh_hz: refresh_rate(&device),
                    primary,
                    id: device,
                },
            }
        })
        .collect()
}

pub fn find_by_id(id: &str) -> Option<Monitor> {
    enumerate().into_iter().find(|m| m.info.id == id)
}

unsafe extern "system" fn collect(
    handle: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    // SAFETY: `lparam` is the pointer passed by `enumerate` above.
    let list = unsafe { &mut *(lparam.0 as *mut Vec<(HMONITOR, MONITORINFOEXW)>) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` is a properly sized MONITORINFOEXW.
    if unsafe { GetMonitorInfoW(handle, &mut info.monitorInfo) }.as_bool() {
        list.push((handle, info));
    }
    BOOL(1)
}

fn refresh_rate(device: &str) -> u32 {
    let wide: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: `wide` is null terminated and `mode` is a valid DEVMODEW.
    let ok =
        unsafe { EnumDisplaySettingsW(PCWSTR(wide.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode) };
    if ok.as_bool() {
        mode.dmDisplayFrequency
    } else {
        0
    }
}

fn friendly_name(device: &str, primary: bool) -> String {
    let number = device.trim_start_matches("\\\\.\\DISPLAY");
    if primary {
        format!("Display {number} (primary)")
    } else {
        format!("Display {number}")
    }
}
