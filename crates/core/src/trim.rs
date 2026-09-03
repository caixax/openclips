//! Trim ranges and the keyframe math behind the fast, copy only cut.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::clip::{sanitize_file_name, unique_path};
use crate::error::{CoreError, Result};

/// Shortest clip a trim may produce.
pub const MIN_TRIM_LENGTH: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrimMode {
    /// Copies the encoded streams and cuts on keyframes. Instant, but the
    /// start moves back to the previous keyframe.
    StreamCopy,
    /// Decodes and re-encodes so the cut lands on the exact frames.
    FrameAccurate,
}

impl TrimMode {
    pub const fn label(self) -> &'static str {
        match self {
            TrimMode::StreamCopy => "Fast (cuts on keyframes)",
            TrimMode::FrameAccurate => "Exact (re-encodes)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrimRange {
    pub start: Duration,
    pub end: Duration,
}

impl TrimRange {
    /// Validates a range against the clip duration.
    pub fn new(start: Duration, end: Duration, duration: Duration) -> Result<Self> {
        let invalid = |reason: &str| CoreError::InvalidTrim(reason.to_owned());
        if duration.is_zero() {
            return Err(invalid("the clip has no known duration"));
        }
        if end > duration + Duration::from_millis(50) {
            return Err(invalid("the out point is past the end of the clip"));
        }
        let end = end.min(duration);
        if start >= end {
            return Err(invalid("the in point must be before the out point"));
        }
        if end - start < MIN_TRIM_LENGTH {
            return Err(invalid("the selection is too short"));
        }
        Ok(Self { start, end })
    }

    pub fn length(&self) -> Duration {
        self.end - self.start
    }

    /// True when the range covers the whole clip, in which case trimming
    /// would only copy the file.
    pub fn is_whole(&self, duration: Duration) -> bool {
        self.start.is_zero() && self.end + Duration::from_millis(50) >= duration
    }

    /// The range the stream copy path will really produce: the start moves
    /// to the last keyframe at or before it, the end stays as requested
    /// (frames after the cut are simply not written).
    pub fn snapped_to_keyframes(&self, keyframes: &[Duration]) -> TrimRange {
        let start = keyframes
            .iter()
            .copied()
            .filter(|k| *k <= self.start)
            .max()
            .unwrap_or(Duration::ZERO);
        TrimRange {
            start,
            end: self.end,
        }
    }
}

/// Picks a name for the trimmed file next to the original.
pub fn trimmed_path(original: &Path, title: &str) -> PathBuf {
    let dir = original.parent().unwrap_or(Path::new("."));
    let base = sanitize_file_name(title);
    let base = if base.is_empty() {
        original
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Clip".to_owned())
    } else {
        base
    };
    unique_path(dir, &format!("{base} (trim).mp4"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: Duration = Duration::from_secs(1);

    #[test]
    fn validates_ranges() {
        let duration = 30 * SEC;
        assert!(TrimRange::new(5 * SEC, 10 * SEC, duration).is_ok());
        assert!(TrimRange::new(10 * SEC, 5 * SEC, duration).is_err());
        assert!(TrimRange::new(5 * SEC, 5 * SEC + Duration::from_millis(100), duration).is_err());
        assert!(TrimRange::new(5 * SEC, 31 * SEC, duration).is_err());
        assert!(TrimRange::new(5 * SEC, 10 * SEC, Duration::ZERO).is_err());
        let clamped = TrimRange::new(
            Duration::ZERO,
            30 * SEC + Duration::from_millis(20),
            duration,
        )
        .expect("ok");
        assert_eq!(clamped.end, duration);
        assert!(clamped.is_whole(duration));
        assert!(
            !TrimRange::new(SEC, duration, duration)
                .expect("ok")
                .is_whole(duration)
        );
    }

    #[test]
    fn snaps_start_to_previous_keyframe() {
        let keyframes = [Duration::ZERO, 2 * SEC, 4 * SEC, 6 * SEC];
        let range = TrimRange {
            start: 5 * SEC,
            end: 7 * SEC,
        };
        let snapped = range.snapped_to_keyframes(&keyframes);
        assert_eq!(snapped.start, 4 * SEC);
        assert_eq!(snapped.end, 7 * SEC);
        let exact = TrimRange {
            start: 2 * SEC,
            end: 3 * SEC,
        };
        assert_eq!(exact.snapped_to_keyframes(&keyframes).start, 2 * SEC);
        assert_eq!(range.snapped_to_keyframes(&[]).start, Duration::ZERO);
    }

    #[test]
    fn names_the_trimmed_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let original = dir.path().join("Clip 1.mp4");
        assert_eq!(
            trimmed_path(&original, "My: clip"),
            dir.path().join("My_ clip (trim).mp4")
        );
        assert_eq!(
            trimmed_path(&original, "  "),
            dir.path().join("Clip 1 (trim).mp4")
        );
        std::fs::write(dir.path().join("Clip 1 (trim).mp4"), b"x").expect("write");
        assert_eq!(
            trimmed_path(&original, ""),
            dir.path().join("Clip 1 (trim) (2).mp4")
        );
    }
}
