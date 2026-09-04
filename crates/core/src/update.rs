//! Update model: version numbers, the record of a downloaded installer
//! waiting for the next start, and helpers around GitHub release assets.
//! Fetching and installing live in the app crate.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// GitHub repository (`owner/name`) whose releases carry the installer.
pub const GITHUB_REPO: &str = "openclips/openclips";

/// File name the app stores next to a downloaded installer.
pub const PENDING_FILE_NAME: &str = "pending.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn current() -> Self {
        crate::APP_VERSION.parse().unwrap_or(Version {
            major: 0,
            minor: 0,
            patch: 0,
        })
    }
}

impl FromStr for Version {
    type Err = CoreError;

    /// Accepts `1.2.3`, `v1.2.3` and `1.2.3-beta` (the suffix is ignored).
    fn from_str(text: &str) -> Result<Self> {
        let text = text.trim().trim_start_matches(['v', 'V']);
        let core = text.split(['-', '+']).next().unwrap_or(text);
        let mut parts = core.split('.').map(|p| p.parse::<u32>());
        let mut next = || {
            parts
                .next()
                .and_then(|p| p.ok())
                .ok_or_else(|| CoreError::InvalidVersion(text.to_owned()))
        };
        Ok(Version {
            major: next()?,
            minor: next()?,
            patch: next()?,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// An installer that has been downloaded and verified, to be run on the
/// next start (or right away if the user asks).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUpdate {
    pub version: String,
    pub installer: PathBuf,
    pub sha256: String,
    pub release_url: String,
}

impl PendingUpdate {
    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| CoreError::WriteFile {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| CoreError::Serialize {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        std::fs::write(path, text).map_err(|source| CoreError::WriteFile {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn version(&self) -> Option<Version> {
        self.version.parse().ok()
    }

    /// True when this update is newer than the running program.
    pub fn is_newer(&self) -> bool {
        self.version()
            .is_some_and(|version| version > Version::current())
    }
}

/// Picks the Windows installer out of a release's asset names.
pub fn installer_asset<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    names
        .into_iter()
        .find(|name| name.to_lowercase().ends_with("-setup.exe"))
}

/// Parses a `SHA256SUMS.txt` file (`<hash>  <file name>` per line) and
/// returns the hash for `file_name`, lower case.
pub fn sha256_for(sums: &str, file_name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name.eq_ignore_ascii_case(file_name) && hash.len() == 64).then(|| hash.to_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_orders_versions() {
        let a: Version = "v1.2.3".parse().expect("parse");
        let b: Version = "1.10.0-beta".parse().expect("parse");
        assert!(b > a);
        assert_eq!(a.to_string(), "1.2.3");
        assert!("1.2".parse::<Version>().is_err());
        assert!("x.y.z".parse::<Version>().is_err());
        assert_eq!(Version::current().to_string(), crate::APP_VERSION);
    }

    #[test]
    fn finds_installer_and_hash() {
        let names = [
            "OpenClips-0.2.0-win64.zip",
            "OpenClips-0.2.0-setup.exe",
            "SHA256SUMS.txt",
        ];
        assert_eq!(installer_asset(names), Some("OpenClips-0.2.0-setup.exe"));
        let sums = format!(
            "{}  OpenClips-0.2.0-win64.zip\n{}  OpenClips-0.2.0-setup.exe\n",
            "a".repeat(64),
            "B".repeat(64)
        );
        assert_eq!(
            sha256_for(&sums, "openclips-0.2.0-setup.exe"),
            Some("b".repeat(64))
        );
        assert_eq!(sha256_for(&sums, "other.exe"), None);
    }

    #[test]
    fn pending_round_trip_and_newer_check() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("updates").join(PENDING_FILE_NAME);
        let pending = PendingUpdate {
            version: "99.0.0".to_owned(),
            installer: dir.path().join("setup.exe"),
            sha256: "0".repeat(64),
            release_url: "https://example.invalid/release".to_owned(),
        };
        pending.save(&path).expect("save");
        let loaded = PendingUpdate::load(&path).expect("load");
        assert_eq!(loaded, pending);
        assert!(loaded.is_newer());
        let old = PendingUpdate {
            version: "0.0.1".to_owned(),
            ..pending
        };
        assert!(!old.is_newer());
    }
}
