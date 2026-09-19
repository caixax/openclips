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

/// A Unix socket in the runtime directory. Whoever binds it is the instance;
/// a later launch connects, writes `show` and leaves. A socket file left by
/// a copy that died is detected by the refused connection and replaced.
#[cfg(not(windows))]
mod imp {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tracing::warn;

    pub struct Guard {
        show: Arc<AtomicBool>,
        path: PathBuf,
    }

    impl Guard {
        pub fn take_show_request(&self) -> bool {
            self.show.swap(false, Ordering::SeqCst)
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn socket_path() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir)
            .join("openclips.sock")
    }

    pub fn claim() -> Option<Guard> {
        let path = socket_path();
        if let Ok(mut running) = UnixStream::connect(&path) {
            let _ = running.write_all(b"show\n");
            return None;
        }
        let _ = std::fs::remove_file(&path);
        let show = Arc::new(AtomicBool::new(false));
        match UnixListener::bind(&path) {
            Ok(listener) => {
                let flag = show.clone();
                let spawned = std::thread::Builder::new()
                    .name("instance".to_owned())
                    .spawn(move || {
                        for stream in listener.incoming().flatten() {
                            let mut line = String::new();
                            if BufReader::new(stream).read_line(&mut line).is_ok()
                                && line.trim() == "show"
                            {
                                flag.store(true, Ordering::SeqCst);
                            }
                        }
                    });
                if let Err(err) = spawned {
                    warn!("could not watch for a second launch: {err}");
                }
            }
            // Not being able to guard is no reason not to run.
            Err(err) => warn!("could not claim {}: {err}", path.display()),
        }
        Some(Guard { show, path })
    }
}

pub use imp::Guard;

/// Claims the single instance slot. `None` means another copy is already
/// running and was told to show its window.
pub fn claim() -> Option<Guard> {
    imp::claim()
}
