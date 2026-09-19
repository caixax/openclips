//! One running copy per user. The first process owns a named mutex (Windows)
//! or a socket in the runtime directory (elsewhere); a second launch finds
//! it, asks the owner to show its window and exits.
//!
//! On Linux the same socket carries commands, so `openclips --save-clip`,
//! `--toggle-buffer` and `--toggle-recording` act on the running copy. That
//! is how a key gets bound under Wayland, where no application may grab keys
//! globally: the desktop's own shortcut settings run the command.

/// What a later launch can ask of the running copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Show,
    SaveClip,
    ToggleBuffer,
    ToggleRecording,
}

impl Command {
    /// The command a command line asks for; `Show` when it names none.
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Self {
        for arg in args {
            match arg.as_str() {
                "--save-clip" => return Command::SaveClip,
                "--toggle-buffer" => return Command::ToggleBuffer,
                "--toggle-recording" => return Command::ToggleRecording,
                _ => {}
            }
        }
        Command::Show
    }

    #[cfg_attr(windows, allow(dead_code))]
    fn word(self) -> &'static str {
        match self {
            Command::Show => "show",
            Command::SaveClip => "save-clip",
            Command::ToggleBuffer => "toggle-buffer",
            Command::ToggleRecording => "toggle-recording",
        }
    }

    #[cfg_attr(windows, allow(dead_code))]
    fn from_word(word: &str) -> Option<Self> {
        [
            Command::Show,
            Command::SaveClip,
            Command::ToggleBuffer,
            Command::ToggleRecording,
        ]
        .into_iter()
        .find(|c| c.word() == word)
    }
}

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
        /// What other launches asked for since the last call. Only the show
        /// request travels on Windows, where global hotkeys cover the rest.
        pub fn take_commands(&self) -> Vec<super::Command> {
            // SAFETY: the auto reset event stays valid while the guard lives.
            if unsafe { WaitForSingleObject(self.show, 0) == WAIT_OBJECT_0 } {
                vec![super::Command::Show]
            } else {
                Vec::new()
            }
        }
    }

    pub fn claim(_request: super::Command) -> Option<Guard> {
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
    use std::sync::{Arc, Mutex};

    use tracing::warn;

    use super::Command;

    pub struct Guard {
        pending: Arc<Mutex<Vec<Command>>>,
        path: PathBuf,
    }

    impl Guard {
        /// What other launches asked for since the last call.
        pub fn take_commands(&self) -> Vec<Command> {
            std::mem::take(&mut *self.pending.lock().unwrap_or_else(|p| p.into_inner()))
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

    pub fn claim(request: Command) -> Option<Guard> {
        let path = socket_path();
        if let Ok(mut running) = UnixStream::connect(&path) {
            let _ = running.write_all(format!("{}\n", request.word()).as_bytes());
            return None;
        }
        let _ = std::fs::remove_file(&path);
        let pending = Arc::new(Mutex::new(Vec::new()));
        match UnixListener::bind(&path) {
            Ok(listener) => {
                let queue = pending.clone();
                let spawned = std::thread::Builder::new()
                    .name("instance".to_owned())
                    .spawn(move || {
                        for stream in listener.incoming().flatten() {
                            let mut line = String::new();
                            if BufReader::new(stream).read_line(&mut line).is_ok()
                                && let Some(command) = Command::from_word(line.trim())
                            {
                                queue
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .push(command);
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
        Some(Guard { pending, path })
    }
}

pub use imp::Guard;

/// Claims the single instance slot. `None` means another copy is already
/// running and was handed `request`.
pub fn claim(request: Command) -> Option<Guard> {
    imp::claim(request)
}

#[cfg(test)]
mod tests {
    use super::Command;

    #[test]
    fn command_line_names_the_command() {
        let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(Command::from_args(args(&["openclips"])), Command::Show);
        assert_eq!(
            Command::from_args(args(&["openclips", "--save-clip"])),
            Command::SaveClip
        );
        assert_eq!(
            Command::from_args(args(&["openclips", "--minimized", "--toggle-recording"])),
            Command::ToggleRecording
        );
    }

    #[test]
    fn words_round_trip() {
        for command in [
            Command::Show,
            Command::SaveClip,
            Command::ToggleBuffer,
            Command::ToggleRecording,
        ] {
            assert_eq!(Command::from_word(command.word()), Some(command));
        }
        assert_eq!(Command::from_word("reboot"), None);
    }
}
