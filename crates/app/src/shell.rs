use std::path::Path;
use std::process::Command;

use tracing::warn;

/// Opens a folder in the platform file manager. Failures are logged, not
/// surfaced, because nothing in the app depends on the file manager.
pub fn open_folder(path: &Path) {
    if let Err(err) = std::fs::create_dir_all(path) {
        warn!("could not create {}: {err}", path.display());
        return;
    }
    let result = if cfg!(target_os = "windows") {
        Command::new("explorer.exe").arg(path).spawn()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(path).spawn()
    } else {
        Command::new("xdg-open").arg(path).spawn()
    };
    if let Err(err) = result {
        warn!("could not open {}: {err}", path.display());
    }
}

/// Opens a web page in the default browser.
pub fn open_url(url: &str) {
    if !url.starts_with("https://") {
        return;
    }
    #[cfg(windows)]
    {
        // Straight to the shell: `cmd /C start` would flash a console window
        // from this GUI process.
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{HSTRING, w};

        // SAFETY: plain shell call with owned null terminated strings.
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(url),
                None,
                None,
                SW_SHOWNORMAL,
            )
        };
        // Values up to 32 are error codes by contract.
        if result.0 as usize <= 32 {
            warn!(
                "could not open {url}: ShellExecute returned {}",
                result.0 as usize
            );
        }
    }
    #[cfg(not(windows))]
    {
        let result = if cfg!(target_os = "macos") {
            Command::new("open").arg(url).spawn()
        } else {
            Command::new("xdg-open").arg(url).spawn()
        };
        if let Err(err) = result {
            warn!("could not open {url}: {err}");
        }
    }
}

/// Opens the file manager with `path` selected.
pub fn reveal_file(path: &Path) {
    let result = if cfg!(target_os = "windows") {
        explorer_select(path)
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg("-R").arg(path).spawn()
    } else {
        Command::new("xdg-open")
            .arg(path.parent().unwrap_or(path))
            .spawn()
    };
    if let Err(err) = result {
        warn!("could not reveal {}: {err}", path.display());
    }
}

/// Free and total bytes of the drive holding `path`, when the OS reports it.
#[cfg(windows)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::core::HSTRING;

    let mut probe = path;
    while !probe.is_dir() {
        probe = probe.parent()?;
    }
    let (mut free, mut total) = (0u64, 0u64);
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            &HSTRING::from(probe.as_os_str()),
            Some(&mut free),
            Some(&mut total),
            None,
        )
    };
    ok.ok().map(|()| (free, total))
}

// The statvfs fields are narrower than 64 bits on some architectures, so
// the conversions are not useless everywhere.
#[cfg(not(windows))]
#[allow(clippy::useless_conversion)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: a valid C string and a buffer of the right type; the struct
    // is only read after the call reported success.
    let stat = unsafe {
        if libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) != 0 {
            return None;
        }
        stat.assume_init()
    };
    let block = u64::from(stat.f_frsize);
    Some((
        u64::from(stat.f_bavail) * block,
        u64::from(stat.f_blocks) * block,
    ))
}

/// `explorer /select,"<path>"` must arrive as one raw argument; the default
/// quoting turns it into `"/select,C:\..."`, which Explorer ignores and
/// opens Documents instead.
#[cfg(windows)]
fn explorer_select(path: &Path) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()
}

/// File managers that implement `org.freedesktop.FileManager1` (Dolphin,
/// Nautilus, Nemo, Thunar) open the folder with the file selected. The call
/// goes through `dbus-send`, which every desktop session has; without it, or
/// without such a file manager, the folder is opened instead.
#[cfg(not(windows))]
fn explorer_select(path: &Path) -> std::io::Result<std::process::Child> {
    let uri = format!("file://{}", percent_encode(&path.to_string_lossy()));
    let shown = Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
        ])
        .arg(format!("array:string:{uri}"))
        .arg("string:")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if shown.is_ok_and(|s| s.success()) {
        // Nothing left to wait on; hand back a finished child.
        return Command::new("true").spawn();
    }
    Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .spawn()
}

/// Percent-encodes a path for a `file://` URI, keeping the separators.
#[cfg(not(windows))]
fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            // A comma would split the dbus-send array argument.
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
