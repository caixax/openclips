//! The on-screen notice shown when a clip is saved. One small frameless
//! window is created on first use, parked hidden between clips and placed at
//! the bottom right of the primary screen each time, without taking the
//! focus away from the game.

use std::cell::RefCell;
use std::time::Duration;

use slint::ComponentHandle;
use tracing::warn;

use crate::error::AppError;
use crate::ui::ToastWindow;

const VISIBLE_FOR: Duration = Duration::from_millis(3500);
const MARGIN: i32 = 24;

#[derive(Default)]
pub struct Toast {
    window: RefCell<Option<ToastWindow>>,
    timer: RefCell<Option<slint::Timer>>,
}

impl Toast {
    /// Shows `message` under `heading` for a few seconds. Showing again
    /// while visible replaces the text and restarts the timer.
    pub fn show(&self, heading: &str, message: &str) -> Result<(), AppError> {
        let created = self.window.borrow().is_none();
        if created {
            *self.window.borrow_mut() = Some(ToastWindow::new()?);
        }
        let window = self.window.borrow();
        let Some(window) = window.as_ref() else {
            return Ok(());
        };
        window.set_heading(heading.into());
        window.set_message(message.into());
        let previous = platform::foreground_window();
        window.show()?;
        if created {
            platform::keep_out_of_the_way(window);
        }
        place(window);
        platform::restore_foreground(previous);

        let weak = window.as_weak();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::SingleShot, VISIBLE_FOR, move || {
            if let Some(window) = weak.upgrade()
                && let Err(err) = window.hide()
            {
                warn!("could not hide the clip notice: {err}");
            }
        });
        *self.timer.borrow_mut() = Some(timer);
        Ok(())
    }
}

/// Bottom right corner of the primary work area, above the taskbar.
fn place(window: &ToastWindow) {
    let scale = window.window().scale_factor();
    let size = window.window().size();
    let (width, height) = (size.width as i32, size.height as i32);
    let margin = (MARGIN as f32 * scale) as i32;
    let (right, bottom) = platform::work_area_bottom_right();
    let x = right - width - margin;
    let y = bottom - height - margin;
    window
        .window()
        .set_position(slint::PhysicalPosition::new(x.max(0), y.max(0)));
}

#[cfg(windows)]
mod platform {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::ComponentHandle;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, SPI_GETWORKAREA, SetForegroundWindow,
        SetWindowLongPtrW, SystemParametersInfoW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };

    use super::ToastWindow;

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

    /// No taskbar button and no activation on later shows.
    pub fn keep_out_of_the_way(window: &ToastWindow) {
        let Some(hwnd) = hwnd_of(window) else {
            return;
        };
        // SAFETY: `hwnd` belongs to this thread's window; the style bits are
        // read, extended and written back.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let style = style | (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
        }
    }

    pub fn work_area_bottom_right() -> (i32, i32) {
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

    pub fn foreground_window() -> Option<()> {
        None
    }

    pub fn restore_foreground(_previous: Option<()>) {}

    pub fn keep_out_of_the_way(_window: &ToastWindow) {}

    pub fn work_area_bottom_right() -> (i32, i32) {
        (1920, 1080)
    }
}
