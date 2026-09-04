//! One running copy per user. The first process owns a named mutex; a second
//! launch finds it, asks the owner to show its window and exits.

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, OpenEventW, SetEvent, WaitForSingleObject,
    };
    use windows::core::w;

    const MUTEX_NAME: windows::core::PCWSTR = w!(r"Local\OpenClips.instance");
    const SHOW_EVENT_NAME: windows::core::PCWSTR = w!(r"Local\OpenClips.show");

    /// Held for the whole life of the first process.
    pub struct Guard {
        mutex: HANDLE,
        show: HANDLE,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: both handles were created by this process and are
            // closed exactly once.
            unsafe {
                let _ = CloseHandle(self.show);
                let _ = CloseHandle(self.mutex);
            }
        }
    }

    impl Guard {
        /// Whether another launch asked for the window since the last call.
        pub fn take_show_request(&self) -> bool {
            // SAFETY: the auto reset event stays valid while the guard lives.
            unsafe { WaitForSingleObject(self.show, 0) == WAIT_OBJECT_0 }
        }
    }

    pub fn claim() -> Option<Guard> {
        // SAFETY: the names are static null terminated strings.
        let mutex = unsafe { CreateMutexW(None, false, MUTEX_NAME) }.ok()?;
        // SAFETY: as above.
        let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if already {
            // SAFETY: the handle is valid and closed once.
            unsafe {
                let _ = CloseHandle(mutex);
            }
            signal_show();
            return None;
        }
        // SAFETY: an auto reset event, initially not signaled.
        let show = match unsafe { CreateEventW(None, false, false, SHOW_EVENT_NAME) } {
            Ok(handle) => handle,
            Err(_) => {
                // SAFETY: see above.
                unsafe {
                    let _ = CloseHandle(mutex);
                }
                return None;
            }
        };
        Some(Guard { mutex, show })
    }

    fn signal_show() {
        // SAFETY: opening a named event by a static name.
        if let Ok(event) = unsafe { OpenEventW(EVENT_MODIFY_STATE, false, SHOW_EVENT_NAME) } {
            // SAFETY: the handle is valid until closed below.
            unsafe {
                let _ = SetEvent(event);
                let _ = CloseHandle(event);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub struct Guard;

    impl Guard {
        pub fn take_show_request(&self) -> bool {
            false
        }
    }

    pub fn claim() -> Option<Guard> {
        Some(Guard)
    }
}

pub use imp::Guard;

/// Claims the single instance slot. `None` means another copy is already
/// running and was told to show its window.
pub fn claim() -> Option<Guard> {
    imp::claim()
}
