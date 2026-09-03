//! Optional, user triggered lookup of game names in the Steam catalog. Only
//! used to suggest a name for an executable the bundled database does not
//! know; never contacted without the user pressing the button.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use tracing::info;

const APP_LIST_URL: &str = "https://api.steampowered.com/ISteamApps/GetAppList/v2/";

#[derive(Debug, Deserialize)]
struct AppList {
    applist: Apps,
}

#[derive(Debug, Deserialize)]
struct Apps {
    apps: Vec<App>,
}

#[derive(Debug, Deserialize)]
struct App {
    name: String,
}

/// Downloads the catalog (or reads the cached copy) and returns app names.
pub fn app_names(cache: &Path) -> Result<Vec<String>, String> {
    let text = match std::fs::read_to_string(cache) {
        Ok(text) => text,
        Err(_) => {
            info!("downloading the Steam app list");
            let text = ureq::get(APP_LIST_URL)
                .config()
                .timeout_global(Some(Duration::from_secs(60)))
                .build()
                .call()
                .map_err(|e| format!("Steam request failed: {e}"))?
                .body_mut()
                .read_to_string()
                .map_err(|e| format!("Steam response could not be read: {e}"))?;
            if let Some(dir) = cache.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(cache, &text);
            text
        }
    };
    let list: AppList =
        serde_json::from_str(&text).map_err(|e| format!("Steam list is malformed: {e}"))?;
    Ok(list.applist.apps.into_iter().map(|a| a.name).collect())
}

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn exe_stem(exe: &str) -> String {
    let stem = exe.rsplit_once('.').map(|(s, _)| s).unwrap_or(exe);
    let stem = stem
        .to_lowercase()
        .replace("-win64-shipping", "")
        .replace("-win32-shipping", "")
        .replace("_dx12", "")
        .replace("_dx11", "")
        .replace("64", "");
    normalize(&stem)
}

/// Picks the catalog name that best matches an executable: an exact match
/// on the normalized stem, otherwise the shortest name starting with it.
pub fn suggest_name<'a>(exe: &str, names: &'a [String]) -> Option<&'a String> {
    let stem = exe_stem(exe);
    if stem.len() < 3 {
        return None;
    }
    let mut best: Option<&String> = None;
    for name in names {
        let normalized = normalize(name);
        if normalized == stem {
            return Some(name);
        }
        if normalized.starts_with(&stem) && best.is_none_or(|b| name.len() < b.len()) {
            best = Some(name);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        [
            "Rocket League",
            "Rocket League Sideswipe",
            "Dead Space",
            "Dead Space 2",
            "Half-Life 2",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn exact_normalized_match_wins() {
        assert_eq!(
            suggest_name("RocketLeague.exe", &names()).map(String::as_str),
            Some("Rocket League")
        );
        assert_eq!(suggest_name("hl2.exe", &names()), None);
        assert_eq!(
            suggest_name("deadspace-win64-shipping.exe", &names()).map(String::as_str),
            Some("Dead Space")
        );
    }

    #[test]
    fn prefix_match_prefers_shortest() {
        assert_eq!(
            suggest_name("rocket.exe", &names()).map(String::as_str),
            Some("Rocket League")
        );
        assert_eq!(suggest_name("x.exe", &names()), None);
    }
}
