//! Shared media types exchanged between capture backends and the core.

use std::sync::Arc;
use std::time::Duration;

/// A presentation or decode timestamp in nanoseconds on the capture clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub const ZERO: Self = Self(0);

    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    pub fn from_duration(duration: Duration) -> Self {
        Self(duration.as_nanos() as u64)
    }

    pub const fn nanos(self) -> u64 {
        self.0
    }

    pub const fn as_duration(self) -> Duration {
        Duration::from_nanos(self.0)
    }

    pub const fn saturating_sub(self, other: Self) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(other.0))
    }

    pub fn checked_sub_duration(self, duration: Duration) -> Option<Self> {
        self.0.checked_sub(duration.as_nanos() as u64).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    H264,
}

impl VideoCodec {
    pub const fn name(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264",
        }
    }
}

/// Static properties of an encoded video stream, known once the first
/// frame has been produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub encoder: String,
}

impl StreamInfo {
    pub fn frame_duration(&self) -> Duration {
        if self.fps_num == 0 {
            return Duration::ZERO;
        }
        Duration::from_nanos(1_000_000_000u64 * u64::from(self.fps_den) / u64::from(self.fps_num))
    }
}

/// One encoded access unit. Keyframes are self contained: the backend must
/// guarantee that decoding can start at any frame with `keyframe == true`.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub pts: Timestamp,
    pub dts: Option<Timestamp>,
    pub duration: Option<Duration>,
    pub keyframe: bool,
    pub data: Arc<[u8]>,
}

impl EncodedFrame {
    pub fn size(&self) -> usize {
        self.data.len()
    }
}
