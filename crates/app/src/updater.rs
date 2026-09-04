//! Automatic updates from GitHub releases. The check runs once at start on
//! a background thread; a newer installer is downloaded, verified and kept
//! for the next start, so playing is never interrupted. The user can also
//! install right away from the banner.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use openclips_core::APP_VERSION;
use openclips_core::config::{AppPaths, UpdatesConfig};
use openclips_core::update::{
    PENDING_FILE_NAME, PendingUpdate, Version, installer_asset, sha256_for,
};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

const API_TIMEOUT_SECONDS: u64 = 20;
const MAX_INSTALLER_BYTES: u64 = 400 * 1024 * 1024;

/// What the check found, reported to the UI thread.
#[derive(Debug, Clone)]
pub enum UpdateEvent {
    /// A newer installer is being downloaded in the background.
    Downloading { version: String },
    /// The installer is on disk and verified; it runs on the next start.
    Ready(PendingUpdate),
    /// A newer version exists but this copy is portable, so only a link.
    Available { version: String, url: String },
}

pub fn updates_dir(paths: &AppPaths) -> PathBuf {
    paths.data_dir.join("updates")
}

fn pending_path(paths: &AppPaths) -> PathBuf {
    updates_dir(paths).join(PENDING_FILE_NAME)
}

/// The installer registers an uninstaller next to the executable; a copy
/// without one is the portable zip and must not be replaced silently.
pub fn is_installed_copy() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("Uninstall.exe").exists()))
        .unwrap_or(false)
}

/// Runs a pending installer if one is waiting and still newer than this
/// build. Returns true when the installer was started and the caller must
/// exit; the installer restarts the app when it is done.
pub fn apply_pending_at_start(paths: &AppPaths) -> bool {
    let path = pending_path(paths);
    let Some(pending) = PendingUpdate::load(&path) else {
        return false;
    };
    if !pending.is_newer() {
        info!("dropping stale update {}", pending.version);
        forget_pending(paths, &pending);
        return false;
    }
    if !pending.installer.exists() || !hash_matches(&pending.installer, &pending.sha256) {
        warn!("pending installer is missing or corrupt, checking again later");
        forget_pending(paths, &pending);
        return false;
    }
    match launch_installer(&pending.installer) {
        Ok(()) => {
            info!("installing update {}", pending.version);
            let _ = std::fs::remove_file(&path);
            true
        }
        Err(err) => {
            warn!("could not start the installer: {err}");
            false
        }
    }
}

/// Starts the installer of an update the user accepted from the banner.
pub fn install_now(paths: &AppPaths, pending: &PendingUpdate) -> Result<(), String> {
    if !hash_matches(&pending.installer, &pending.sha256) {
        forget_pending(paths, pending);
        return Err("the downloaded installer is corrupt, it will be fetched again".to_owned());
    }
    launch_installer(&pending.installer)?;
    let _ = std::fs::remove_file(pending_path(paths));
    Ok(())
}

fn launch_installer(installer: &Path) -> Result<(), String> {
    Command::new(installer)
        .args(["/S", "/UPDATE"])
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn forget_pending(paths: &AppPaths, pending: &PendingUpdate) {
    let _ = std::fs::remove_file(pending_path(paths));
    let _ = std::fs::remove_file(&pending.installer);
}

/// Checks GitHub once, on a background thread. `report` is called from that
/// thread; the caller hops back to the UI.
pub fn spawn_check(
    paths: AppPaths,
    config: UpdatesConfig,
    report: impl Fn(UpdateEvent) + Send + 'static,
) {
    if !config.check {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("update-check".to_owned())
        .spawn(move || {
            if let Err(err) = check(&paths, &config, &report) {
                warn!("update check failed: {err}");
            }
        });
    if let Err(err) = spawned {
        warn!("update check thread could not start: {err}");
    }
}

struct Release {
    version: Version,
    tag: String,
    url: String,
    assets: Vec<(String, String)>,
}

fn check(
    paths: &AppPaths,
    config: &UpdatesConfig,
    report: &impl Fn(UpdateEvent),
) -> Result<(), String> {
    let release = latest_release(config.effective_repo())?;
    let current = Version::current();
    if release.version <= current {
        info!("up to date ({current}, latest {})", release.tag);
        return Ok(());
    }
    let version = release.version.to_string();
    if !is_installed_copy() {
        report(UpdateEvent::Available {
            version,
            url: release.url,
        });
        return Ok(());
    }
    let pending_file = pending_path(paths);
    if let Some(pending) = PendingUpdate::load(&pending_file)
        && pending.version == version
        && pending.installer.exists()
    {
        report(UpdateEvent::Ready(pending));
        return Ok(());
    }

    let names: Vec<&str> = release.assets.iter().map(|(n, _)| n.as_str()).collect();
    let asset = installer_asset(names.iter().copied())
        .ok_or_else(|| format!("release {} has no installer", release.tag))?
        .to_owned();
    let download_url = release
        .assets
        .iter()
        .find(|(n, _)| *n == asset)
        .map(|(_, u)| u.clone())
        .ok_or_else(|| "installer asset has no url".to_owned())?;
    let expected = release
        .assets
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("SHA256SUMS.txt"))
        .map(|(_, u)| fetch_text(u))
        .transpose()?
        .and_then(|sums| sha256_for(&sums, &asset))
        .ok_or_else(|| {
            format!(
                "release {} has no SHA256SUMS.txt entry for {asset}",
                release.tag
            )
        })?;

    report(UpdateEvent::Downloading {
        version: version.clone(),
    });
    let dir = updates_dir(paths);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(&asset);
    let partial = dir.join(format!("{asset}.part"));
    let actual = download(&download_url, &partial)?;
    if actual != expected {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("checksum mismatch for {asset}"));
    }
    std::fs::rename(&partial, &target).map_err(|e| e.to_string())?;
    let pending = PendingUpdate {
        version,
        installer: target,
        sha256: expected,
        release_url: release.url,
    };
    pending.save(&pending_file).map_err(|e| e.to_string())?;
    info!(
        "update {} downloaded, installs on next start",
        pending.version
    );
    report(UpdateEvent::Ready(pending));
    Ok(())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(API_TIMEOUT_SECONDS)))
        .user_agent(format!("OpenClips/{APP_VERSION}"))
        .build()
        .into()
}

fn fetch_text(url: &str) -> Result<String, String> {
    agent()
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())
}

fn latest_release(repo: &str) -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let text = fetch_text(&url)?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let tag = json["tag_name"]
        .as_str()
        .ok_or("release has no tag")?
        .to_owned();
    let version: Version = tag.parse().map_err(|e| format!("{e}"))?;
    let assets = json["assets"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    Some((
                        a["name"].as_str()?.to_owned(),
                        a["browser_download_url"].as_str()?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        version,
        url: json["html_url"]
            .as_str()
            .unwrap_or("https://github.com")
            .to_owned(),
        tag,
        assets,
    })
}

/// Streams `url` into `path` and returns the SHA-256 of what was written.
fn download(url: &str, path: &Path) -> Result<String, String> {
    let mut response = agent().get(url).call().map_err(|e| e.to_string())?;
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > MAX_INSTALLER_BYTES {
            return Err("installer is unexpectedly large".to_owned());
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read]).map_err(|e| e.to_string())?;
    }
    file.flush().map_err(|e| e.to_string())?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_matches(path: &Path, expected: &str) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buffer[..n]),
            Err(_) => return false,
        }
    }
    format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(expected)
}
