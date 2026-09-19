//! Running processes from `/proc`.
//!
//! A game started through Proton or Wine runs as `wine64-preloader` (or
//! similar) with the Windows executable in its command line, so the name
//! reported for such a process is that `.exe`: the games database, which is
//! keyed by Windows executable names, then recognises it as it does on
//! Windows.

use std::path::{Path, PathBuf};

use openclips_core::games::RunningProcess;

use crate::backend::ProcessWatcher;
use crate::error::CaptureError;

pub struct ProcWatcher;

impl ProcessWatcher for ProcWatcher {
    fn running(&self) -> Result<Vec<RunningProcess>, CaptureError> {
        let entries = std::fs::read_dir("/proc")
            .map_err(|e| CaptureError::PipelineBuild(format!("could not read /proc: {e}")))?;
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let dir = entry.path();
            // Kernel threads have an empty command line.
            let Ok(cmdline) = std::fs::read(dir.join("cmdline")) else {
                continue;
            };
            let Some(exe) = executable_name(&cmdline) else {
                continue;
            };
            found.push(RunningProcess {
                pid,
                exe,
                path: std::fs::read_link(dir.join("exe")).ok(),
                // Which window has the focus is a question for the
                // compositor, and Wayland does not answer it.
                foreground: false,
            });
        }
        Ok(found)
    }

    fn process_path(&self, pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }
}

/// The lower case file name a process goes by: the first `.exe` argument for
/// Wine and Proton processes, the program itself otherwise.
fn executable_name(cmdline: &[u8]) -> Option<String> {
    let mut args = cmdline
        .split(|b| *b == 0)
        .filter(|arg| !arg.is_empty())
        .map(String::from_utf8_lossy);
    let program = args.next()?;
    let windows_exe = std::iter::once(program.clone())
        .chain(args)
        .find(|arg| arg.to_ascii_lowercase().ends_with(".exe"));
    let name = match &windows_exe {
        // A Windows path keeps its backslashes inside the command line.
        Some(arg) => arg.rsplit(['\\', '/']).next().unwrap_or(arg).to_owned(),
        None => Path::new(program.as_ref())
            .file_name()?
            .to_string_lossy()
            .into_owned(),
    };
    (!name.is_empty()).then(|| name.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::executable_name;

    #[test]
    fn native_process_uses_its_file_name() {
        assert_eq!(
            executable_name(b"/usr/bin/Firefox\0--new-window\0").as_deref(),
            Some("firefox")
        );
    }

    #[test]
    fn proton_process_uses_the_windows_executable() {
        let cmdline = b"/home/u/.steam/proton/files/bin/wine64-preloader\0Z:\\games\\Half-Life\\HL.exe\0-steam\0";
        assert_eq!(executable_name(cmdline).as_deref(), Some("hl.exe"));
    }

    #[test]
    fn kernel_thread_has_no_name() {
        assert_eq!(executable_name(b""), None);
    }
}
