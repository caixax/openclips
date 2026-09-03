//! Clip naming and output locations.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Broken down local time supplied by the caller so that this module stays
/// free of a time zone dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl LocalDateTime {
    pub fn date_string(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    pub fn time_string(&self) -> String {
        format!("{:02}-{:02}-{:02}", self.hour, self.minute, self.second)
    }
}

/// Expands a file name pattern such as `{game} {date} {time}` and makes the
/// result safe for use as a file name on every supported platform.
pub fn clip_file_name(pattern: &str, game: &str, when: &LocalDateTime) -> String {
    let game = if game.trim().is_empty() {
        "Clip"
    } else {
        game.trim()
    };
    let expanded = pattern
        .replace("{game}", game)
        .replace("{date}", &when.date_string())
        .replace("{time}", &when.time_string());
    let mut name = sanitize_file_name(&expanded);
    if name.is_empty() {
        name = format!("Clip {} {}", when.date_string(), when.time_string());
    }
    format!("{name}.mp4")
}

/// Picks a path under `dir` that does not exist yet by appending a counter.
pub fn unique_path(dir: &Path, file_name: &str) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file_name);
    let ext = Path::new(file_name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("mp4");
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).{ext}")))
        .find(|p| !p.exists())
        .unwrap_or(candidate)
}

pub fn sanitize_file_name(input: &str) -> String {
    const FORBIDDEN: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    let cleaned: String = input
        .chars()
        .map(|c| {
            if c.is_whitespace() {
                ' '
            } else if FORBIDDEN.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Summary of a clip that was written to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipFile {
    pub path: PathBuf,
    pub duration: Duration,
    pub bytes: u64,
    pub created: SystemTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn when() -> LocalDateTime {
        LocalDateTime {
            year: 2026,
            month: 9,
            day: 3,
            hour: 21,
            minute: 5,
            second: 9,
        }
    }

    #[test]
    fn expands_pattern() {
        let name = clip_file_name("{game} {date} {time}", "Half-Life 2", &when());
        assert_eq!(name, "Half-Life 2 2026-09-03 21-05-09.mp4");
    }

    #[test]
    fn empty_game_falls_back_to_clip() {
        let name = clip_file_name("{game} {date}", "  ", &when());
        assert_eq!(name, "Clip 2026-09-03.mp4");
    }

    #[test]
    fn sanitizes_forbidden_characters() {
        let name = clip_file_name("{game}", "Game: The <Best>?", &when());
        assert_eq!(name, "Game_ The _Best__.mp4");
        assert_eq!(sanitize_file_name("  trailing dots... "), "trailing dots");
        assert_eq!(sanitize_file_name("a\tb\nc"), "a b c");
    }

    #[test]
    fn empty_pattern_still_yields_a_name() {
        let name = clip_file_name("", "Game", &when());
        assert_eq!(name, "Clip 2026-09-03 21-05-09.mp4");
    }

    #[test]
    fn unique_path_appends_counter() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(unique_path(dir.path(), "a.mp4"), dir.path().join("a.mp4"));
        std::fs::write(dir.path().join("a.mp4"), b"x").expect("write");
        assert_eq!(
            unique_path(dir.path(), "a.mp4"),
            dir.path().join("a (2).mp4")
        );
        std::fs::write(dir.path().join("a (2).mp4"), b"x").expect("write");
        assert_eq!(
            unique_path(dir.path(), "a.mp4"),
            dir.path().join("a (3).mp4")
        );
    }
}
