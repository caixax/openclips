//! Finding the window to hook for a process. Game capture needs the target's
//! top level window, its owning GUI thread (for the anti-cheat friendly
//! injection path) and its bitness.

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetWindow, GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::BOOL;

use super::inject::process_is_64bit;
use crate::error::CaptureError;

/// The resolved target of a hook.
pub struct TargetWindow {
    pub pid: u32,
    pub hwnd: HWND,
    pub thread_id: u32,
    pub is_64bit: bool,
}

struct Search {
    pid: u32,
    hwnd: HWND,
    thread_id: u32,
}

/// Picks the process's most likely game window: a visible, unowned top level
/// window belonging to the process.
pub fn find_for_pid(pid: u32) -> Result<TargetWindow, CaptureError> {
    let mut search = Search {
        pid,
        hwnd: HWND::default(),
        thread_id: 0,
    };
    // SAFETY: the callback only touches `search`, passed as its LPARAM, and
    // uses read only Win32 window queries.
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut search as *mut Search as isize));
    }
    if search.hwnd.is_invalid() || search.thread_id == 0 {
        return Err(CaptureError::GameCapture(
            "the game has no capturable window yet".to_owned(),
        ));
    }
    Ok(TargetWindow {
        pid,
        hwnd: search.hwnd,
        thread_id: search.thread_id,
        is_64bit: process_is_64bit(pid),
    })
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Search` handed to EnumWindows.
    let search = unsafe { &mut *(lparam.0 as *mut Search) };
    let mut window_pid = 0u32;
    // SAFETY: `hwnd` is a live window from the enumeration.
    let thread_id = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut window_pid)) };
    if window_pid != search.pid {
        return BOOL(1);
    }
    // SAFETY: read only queries on a live window.
    let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    let owned = unsafe { GetWindow(hwnd, GW_OWNER) }
        .map(|o| !o.is_invalid())
        .unwrap_or(false);
    if visible && !owned {
        search.hwnd = hwnd;
        search.thread_id = thread_id;
        // Stop: the first visible top level window is the game's.
        return BOOL(0);
    }
    BOOL(1)
}
