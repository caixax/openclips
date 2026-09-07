//! The on-screen notice shown when a clip is saved. One small frameless
//! window is created at startup, parked hidden between clips and placed at
//! the bottom right of the game's screen each time, without taking the
//! focus away from the game.

use std::cell::RefCell;
use std::time::Duration;

use slint::ComponentHandle;
use tracing::warn;

use crate::error::AppError;
use crate::ui::ToastWindow;

const VISIBLE_FOR: Duration = Duration::from_millis(3500);
/// Poll interval and count while waiting for the event loop to create the
/// native window after `show`.
const SETUP_DELAY: Duration = Duration::from_millis(250);
const SETUP_ATTEMPTS: u32 = 40;
const MARGIN: i32 = 24;

#[derive(Default)]
pub struct Toast {
    window: RefCell<Option<ToastWindow>>,
    timer: RefCell<Option<slint::Timer>>,
}

impl Toast {
    /// Creates the window ahead of time, off screen, so its extended styles
    /// are in place before a clip needs it. A window shown with the default
    /// styles is activated by Windows, which takes the focus from the game
    /// and drops an exclusive fullscreen game to the desktop. Done at
    /// startup, while nothing is in front. The window stays shown as far as
    /// the toolkit knows: every visibility change through it rewrites the
    /// extended styles and would drop ours, so later shows and hides go
    /// straight to the OS (see `platform::set_visible`).
    pub fn prepare(&self) -> Result<(), AppError> {
        if self.window.borrow().is_some() {
            return Ok(());
        }
        let window = ToastWindow::new()?;
        window
            .window()
            .set_position(slint::PhysicalPosition::new(-10_000, -10_000));
        window.show()?;
        // The native window only exists once the event loop has created
        // it, so the styles are applied (and the window parked) from a
        // timer that retries until the handle is there.
        finish_setup(window.as_weak(), SETUP_ATTEMPTS);
        *self.window.borrow_mut() = Some(window);
        Ok(())
    }

    /// Shows `message` under `heading` for a few seconds. Showing again
    /// while visible replaces the text and restarts the timer.
    pub fn show(&self, heading: &str, message: &str) -> Result<(), AppError> {
        self.prepare()?;
        let window = self.window.borrow();
        let Some(window) = window.as_ref() else {
            return Ok(());
        };
        window.set_heading(heading.into());
        window.set_message(message.into());
        let previous = platform::foreground_window();
        place(window, previous);
        platform::set_visible(window, true);
        platform::restore_foreground(previous);

        let weak = window.as_weak();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::SingleShot, VISIBLE_FOR, move || {
            if let Some(window) = weak.upgrade() {
                platform::set_visible(&window, false);
            }
        });
        *self.timer.borrow_mut() = Some(timer);
        Ok(())
    }
}

fn finish_setup(weak: slint::Weak<ToastWindow>, attempts_left: u32) {
    slint::Timer::single_shot(SETUP_DELAY, move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        if platform::keep_out_of_the_way(&window) {
            platform::set_visible(&window, false);
        } else if attempts_left > 0 {
            finish_setup(weak, attempts_left - 1);
        } else {
            warn!("the clip notice window never appeared; it may take the focus");
        }
    });
}

/// Bottom right corner of the work area of the display showing `focused`
/// (the game), above the taskbar; the primary display when unknown.
fn place(window: &ToastWindow, focused: Option<platform::Handle>) {
    let scale = window.window().scale_factor();
    let size = window.window().size();
    let (width, height) = (size.width as i32, size.height as i32);
    let margin = (MARGIN as f32 * scale) as i32;
    let (right, bottom) = platform::work_area_bottom_right(focused);
    let x = right - width - margin;
    let y = bottom - height - margin;
    window
        .window()
        .set_position(slint::PhysicalPosition::new(x.max(0), y.max(0)));
}

#[cfg(windows)]
mod platform {
    use std::mem::size_of;

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::ComponentHandle;
    use tracing::warn;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, SPI_GETWORKAREA, SW_HIDE,
        SW_SHOWNOACTIVATE, SetForegroundWindow, SetWindowLongPtrW, ShowWindow,
        SystemParametersInfoW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };

    use super::ToastWindow;

    pub type Handle = HWND;

    pub fn foreground_window() -> Option<HWND> {
        // SAFETY: plain query with no arguments.
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.is_invalid()).then_some(hwnd)
    }

    /// Gives the focus back to whatever had it before the notice appeared.
    pub fn restore_foreground(previous: Option<HWND>) {
        if let Some(hwnd) = previous {
            // SAFETY: a stale handle only makes the call fail.
            let _ = unsafe { SetForegroundWindow(hwnd) };
        }
    }

    /// Shows or hides the window without activating it and without the
    /// toolkit touching its styles.
    pub fn set_visible(window: &ToastWindow, visible: bool) {
        let Some(hwnd) = hwnd_of(window) else {
            warn!("the clip notice has no window handle");
            return;
        };
        let command = if visible { SW_SHOWNOACTIVATE } else { SW_HIDE };
        // SAFETY: `hwnd` belongs to this thread's window.
        let _ = unsafe { ShowWindow(hwnd, command) };
    }

    /// No taskbar button and no activation on later shows. False when the
    /// native window does not exist yet.
    pub fn keep_out_of_the_way(window: &ToastWindow) -> bool {
        let Some(hwnd) = hwnd_of(window) else {
            return false;
        };
        // SAFETY: `hwnd` belongs to this thread's window; the style bits are
        // read, extended and written back.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let style = style | (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
        }
        true
    }

    pub fn work_area_bottom_right(focused: Option<HWND>) -> (i32, i32) {
        if let Some(hwnd) = focused {
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: a stale handle only yields the nearest monitor; `info`
            // is sized for the call.
            let found = unsafe {
                let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                GetMonitorInfoW(monitor, &mut info).as_bool()
            };
            if found {
                return (info.rcWork.right, info.rcWork.bottom);
            }
        }
        let mut rect = RECT::default();
        // SAFETY: `rect` is the out buffer SPI_GETWORKAREA expects.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(&mut rect as *mut RECT as *mut _),
                Default::default(),
            )
        };
        if ok.is_ok() {
            (rect.right, rect.bottom)
        } else {
            (1920, 1080)
        }
    }

    fn hwnd_of(window: &ToastWindow) -> Option<HWND> {
        let handle = window.window().window_handle();
        match handle.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
            _ => None,
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::ToastWindow;

    pub type Handle = ();

    pub fn foreground_window() -> Option<()> {
        None
    }

    pub fn restore_foreground(_previous: Option<()>) {}

    pub fn keep_out_of_the_way(_window: &ToastWindow) -> bool {
        true
    }

    pub fn set_visible(window: &ToastWindow, visible: bool) {
        let result = if visible {
            window.show()
        } else {
            window.hide()
        };
        if let Err(err) = result {
            tracing::warn!("could not change the clip notice visibility: {err}");
        }
    }

    pub fn work_area_bottom_right(_focused: Option<()>) -> (i32, i32) {
        (1920, 1080)
    }
}
