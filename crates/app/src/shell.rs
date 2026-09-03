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
