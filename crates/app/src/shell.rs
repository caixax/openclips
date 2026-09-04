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
    let result = if cfg!(target_os = "windows") {
        Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn()
    } else {
        Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(err) = result {
        warn!("could not open {url}: {err}");
    }
}

/// Opens the file manager with `path` selected.
pub fn reveal_file(path: &Path) {
    let result = if cfg!(target_os = "windows") {
        Command::new("explorer.exe")
            .arg(format!("/select,{}", path.display()))
            .spawn()
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

#[cfg(not(windows))]
pub fn disk_space(_path: &Path) -> Option<(u64, u64)> {
    None
}
