//! Finds the GStreamer runtime before any of its DLLs are needed.
//!
//! The GStreamer imports are delay loaded (see `build.rs`), so the process
//! starts without them on `PATH`. This module points the loader at the
//! installed runtime and produces a readable error when it is missing.

use std::path::{Path, PathBuf};

pub const GSTREAMER_ENV: &str = "GSTREAMER_1_0_ROOT_MSVC_X86_64";

const DEFAULT_ROOTS: &[&str] = &[
    r"C:\Program Files\gstreamer\1.0\msvc_x86_64",
    r"C:\gstreamer\1.0\msvc_x86_64",
];

const PROBE_DLL: &str = "gstreamer-1.0-0.dll";

/// Where the runtime was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    pub root: PathBuf,
    pub bin: PathBuf,
}

fn candidates(exe_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = exe_dir {
        // A bundled runtime next to the executable wins over an installed one.
        roots.push(dir.join("gstreamer"));
        roots.push(dir.to_path_buf());
    }
    if let Some(env) = std::env::var_os(GSTREAMER_ENV) {
        roots.push(PathBuf::from(env));
    }
    roots.extend(DEFAULT_ROOTS.iter().map(PathBuf::from));
    roots
}

/// Picks the first candidate root whose `bin` folder holds the core DLL.
/// A root that *is* the bin folder (bundled layout) is accepted too.
pub fn find_runtime(exe_dir: Option<&Path>) -> Option<Runtime> {
    for root in candidates(exe_dir) {
        let bin = root.join("bin");
        if bin.join(PROBE_DLL).is_file() {
            return Some(Runtime { root, bin });
        }
        if root.join(PROBE_DLL).is_file() {
            return Some(Runtime {
                bin: root.clone(),
                root,
            });
        }
    }
    None
}

/// Registers the runtime folder with the DLL loader and returns it. Only
/// the search path changes; nothing is loaded yet.
pub fn locate() -> Result<Runtime, String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let runtime = find_runtime(exe_dir.as_deref()).ok_or_else(|| {
        format!(
            "GStreamer 1.28 (MSVC 64 bit) was not found. Install it from https://gstreamer.freedesktop.org/download/ \
             or set {GSTREAMER_ENV} to its folder."
        )
    })?;
    register_dll_directory(&runtime.bin)?;
    Ok(runtime)
}

#[cfg(windows)]
fn register_dll_directory(bin: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::System::LibraryLoader::SetDllDirectoryW;
    use windows::core::PCWSTR;

    let wide: Vec<u16> = bin
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a null terminated UTF-16 string that outlives the call.
    unsafe { SetDllDirectoryW(PCWSTR(wide.as_ptr())) }.map_err(|e| e.to_string())?;
    // Plugins loaded later by GStreamer resolve their own imports through
    // PATH, so the runtime folder goes there as well.
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(&path));
    if let Ok(joined) = std::env::join_paths(paths) {
        // SAFETY: called on the main thread before any other thread exists.
        unsafe { std::env::set_var("PATH", joined) };
    }
    Ok(())
}

#[cfg(not(windows))]
fn register_dll_directory(_bin: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_runtime_next_to_exe_wins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bundled = dir.path().join("gstreamer").join("bin");
        std::fs::create_dir_all(&bundled).expect("mkdir");
        std::fs::write(bundled.join(PROBE_DLL), b"x").expect("write");
        let found = find_runtime(Some(dir.path())).expect("found");
        assert_eq!(found.bin, bundled);
    }

    #[test]
    fn flat_layout_is_accepted() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(PROBE_DLL), b"x").expect("write");
        let found = find_runtime(Some(dir.path())).expect("found");
        assert_eq!(found.bin, dir.path());
    }
}
