//! Locating and running OBS Studio's signed capture hook helpers.
//!
//! OpenClips ships the unmodified, Authenticode signed `graphics-hook`,
//! `inject-helper` and `get-graphics-offsets` binaries from OBS (see
//! `third_party/obs-capture`). Anti-cheat vendors whitelist that signature,
//! so injection must always go through these binaries: we never build or
//! re-sign our own hook.

use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{info, warn};

use super::protocol::{GraphicsOffsets, parse_offsets};
use crate::error::CaptureError;

/// The six files that must sit together in the hooks directory.
const REQUIRED: &[&str] = &[
    "graphics-hook64.dll",
    "graphics-hook32.dll",
    "inject-helper64.exe",
    "inject-helper32.exe",
    "get-graphics-offsets64.exe",
    "get-graphics-offsets32.exe",
];

/// A resolved directory holding the signed hook binaries.
#[derive(Debug, Clone)]
pub struct Hooks {
    dir: PathBuf,
}

impl Hooks {
    /// Finds the hooks directory. Order: `OPENCLIPS_HOOKS_DIR`, an
    /// `obs-capture` folder next to the executable (release layout), then the
    /// `third_party/obs-capture` folder in the source tree (dev layout).
    pub fn locate() -> Result<Self, CaptureError> {
        for dir in candidates() {
            if REQUIRED.iter().all(|f| dir.join(f).is_file()) {
                info!("game capture hooks at {}", dir.display());
                return Ok(Self { dir });
            }
        }
        Err(CaptureError::GameCapture(
            "the OBS capture hook binaries were not found (expected obs-capture next to the executable)".to_owned(),
        ))
    }

    fn hook_dll(&self, target_64bit: bool) -> PathBuf {
        self.dir.join(if target_64bit {
            "graphics-hook64.dll"
        } else {
            "graphics-hook32.dll"
        })
    }

    fn inject_helper(&self, target_64bit: bool) -> PathBuf {
        self.dir.join(if target_64bit {
            "inject-helper64.exe"
        } else {
            "inject-helper32.exe"
        })
    }

    fn offsets_helper(&self, target_64bit: bool) -> PathBuf {
        self.dir.join(if target_64bit {
            "get-graphics-offsets64.exe"
        } else {
            "get-graphics-offsets32.exe"
        })
    }

    /// Runs `get-graphics-offsets` for the given bitness and parses the
    /// Present vtable offsets the hook needs. These depend on the system
    /// d3d/dxgi DLL versions, so they are read once per capture.
    pub fn graphics_offsets(&self, target_64bit: bool) -> Result<GraphicsOffsets, CaptureError> {
        let exe = self.offsets_helper(target_64bit);
        let output = Command::new(&exe).output().map_err(|e| {
            CaptureError::GameCapture(format!("could not run {}: {e}", exe.display()))
        })?;
        if !output.status.success() {
            return Err(CaptureError::GameCapture(format!(
                "{} exited with {}",
                exe.display(),
                output.status
            )));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(parse_offsets(&text))
    }

    /// Injects the capture hook into the target using the signed helper.
    /// `thread_id` is the target's main GUI thread; the helper installs the
    /// hook through `SetWindowsHookEx` (the anti-cheat friendly path OBS uses
    /// by default), which loads the DLL when that thread next pumps messages.
    pub fn inject(&self, target_64bit: bool, thread_id: u32) -> Result<(), CaptureError> {
        let helper = self.inject_helper(target_64bit);
        let dll = self.hook_dll(target_64bit);
        // Argument order matches OBS: <hook dll> <use_safe_inject> <id>.
        // Safe inject (1) hooks the GUI thread; the id is then the thread id.
        let status = Command::new(&helper)
            .arg(&dll)
            .arg("1")
            .arg(thread_id.to_string())
            .status()
            .map_err(|e| {
                CaptureError::GameCapture(format!("could not run {}: {e}", helper.display()))
            })?;
        // The helper returns the injection result as its exit code: 0 on
        // success, negative INJECT_ERROR_* values otherwise.
        match status.code() {
            Some(0) => Ok(()),
            Some(code) => Err(CaptureError::GameCapture(format!(
                "hook injection failed ({})",
                inject_error(code)
            ))),
            None => Err(CaptureError::GameCapture(
                "the inject helper was terminated".to_owned(),
            )),
        }
    }
}

/// Maps the helper's negative exit codes to a readable reason.
fn inject_error(code: i32) -> &'static str {
    match code {
        -1 => "injection failed",
        -2 => "invalid parameters",
        -3 => "could not open the target process",
        -4 => "unexpected failure",
        _ => "unknown error",
    }
}

fn candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(env) = std::env::var_os("OPENCLIPS_HOOKS_DIR") {
        dirs.push(PathBuf::from(env));
    }
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        dirs.push(exe_dir.join("obs-capture"));
    }
    // Dev tree: crates/capture/src/windows/game_capture -> repo root.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(root) = manifest.parent().and_then(Path::parent) {
        dirs.push(root.join("third_party").join("obs-capture"));
    }
    dirs
}

/// Whether the process behind `pid` is 64 bit. Defaults to true (matching a
/// 64 bit OS) when the process cannot be queried.
pub fn process_is_64bit(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            IsWow64Process, OpenProcess, PROCESS_QUERY_INFORMATION,
        };

        // SAFETY: standard Win32 calls; the handle is closed before return.
        unsafe {
            let Ok(handle) = OpenProcess(PROCESS_QUERY_INFORMATION, false, pid) else {
                return true;
            };
            let mut wow64 = windows::core::BOOL(0);
            let queried = IsWow64Process(handle, &mut wow64).is_ok();
            let _ = CloseHandle(handle);
            if !queried {
                warn!("could not determine process bitness for pid {pid}");
                return true;
            }
            // WOW64 means a 32 bit process on 64 bit Windows.
            !wow64.as_bool()
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        true
    }
}
