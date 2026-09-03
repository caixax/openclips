//! The rolling replay buffer: a bounded, in memory ring of encoded frames.
//!
//! Frames are evicted one GOP at a time so that the oldest retained frame is
//! always a keyframe. The buffer therefore holds between `max_duration` and
//! `max_duration + one GOP` of footage, bounded additionally by `max_bytes`.

use std::collections::VecDeque;
use std::time::Duration;

use crate::media::{EncodedFrame, StreamInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayLimits {
    pub max_duration: Duration,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplayStats {
    pub frames: usize,
    pub bytes: usize,
    pub keyframes: usize,
    /// Footage available from the oldest keyframe to the newest frame.
    pub duration: Duration,
}

/// The frames chosen for a clip, always starting at a keyframe.
#[derive(Debug, Clone)]
pub struct ReplaySnapshot {
    pub frames: Vec<EncodedFrame>,
    pub stream: StreamInfo,
    pub duration: Duration,
    /// True when the buffer held less footage than was requested.
    pub truncated: bool,
}

#[derive(Debug)]
pub struct ReplayBuffer {
    limits: ReplayLimits,
    stream: Option<StreamInfo>,
    frames: VecDeque<EncodedFrame>,
    bytes: usize,
    dropped_leading: u64,
}

impl ReplayBuffer {
    pub fn new(limits: ReplayLimits) -> Self {
        Self {
            limits,
            stream: None,
            frames: VecDeque::new(),
            bytes: 0,
            dropped_leading: 0,
        }
    }

    pub fn limits(&self) -> ReplayLimits {
        self.limits
    }

    pub fn set_limits(&mut self, limits: ReplayLimits) {
        self.limits = limits;
        self.evict();
    }

    pub fn stream(&self) -> Option<&StreamInfo> {
        self.stream.as_ref()
    }

    pub fn set_stream(&mut self, stream: StreamInfo) {
        if self.stream.as_ref() != Some(&stream) {
            self.clear();
            self.stream = Some(stream);
        }
    }

    pub fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }

    /// Frames received before the first keyframe cannot start a clip and are
    /// counted rather than stored.
    pub fn dropped_leading_frames(&self) -> u64 {
        self.dropped_leading
    }

    pub fn push(&mut self, frame: EncodedFrame) {
        if self.frames.is_empty() && !frame.keyframe {
            self.dropped_leading += 1;
            return;
        }
        self.bytes += frame.size();
        self.frames.push_back(frame);
        self.evict();
    }

    pub fn stats(&self) -> ReplayStats {
        ReplayStats {
            frames: self.frames.len(),
            bytes: self.bytes,
            keyframes: self.frames.iter().filter(|f| f.keyframe).count(),
            duration: self.span(),
        }
    }

    /// Selects the frames covering at least the last `wanted` of footage,
    /// starting at the latest keyframe that still satisfies that length.
    pub fn snapshot_last(&self, wanted: Duration) -> Option<ReplaySnapshot> {
        let stream = self.stream.clone()?;
        let last = self.frames.back()?;
        let target = last.pts.checked_sub_duration(wanted);

        let start = match target {
            Some(target) => self
                .frames
                .iter()
                .enumerate()
                .rev()
                .find(|(_, f)| f.keyframe && f.pts <= target)
                .map(|(i, _)| i),
            None => None,
        };
        let start = start.unwrap_or(0);
        let frames: Vec<EncodedFrame> = self.frames.range(start..).cloned().collect();
        let first = frames.first()?;
        let duration = Self::extent(first, last, &stream);
        Some(ReplaySnapshot {
            truncated: duration < wanted,
            frames,
            stream,
            duration,
        })
    }

    fn span(&self) -> Duration {
        match (self.frames.front(), self.frames.back(), &self.stream) {
            (Some(first), Some(last), Some(stream)) => Self::extent(first, last, stream),
            _ => Duration::ZERO,
        }
    }

    fn extent(first: &EncodedFrame, last: &EncodedFrame, stream: &StreamInfo) -> Duration {
        let tail = last.duration.unwrap_or_else(|| stream.frame_duration());
        last.pts.saturating_sub(first.pts) + tail
    }

    fn evict(&mut self) {
        loop {
            let Some(next_gop) = self.second_keyframe_index() else {
                return;
            };
            let over_duration = self.duration_from(next_gop) >= self.limits.max_duration;
            let over_bytes = self.bytes > self.limits.max_bytes;
            if !over_duration && !over_bytes {
                return;
            }
            for _ in 0..next_gop {
                if let Some(frame) = self.frames.pop_front() {
                    self.bytes -= frame.size();
                }
            }
        }
    }

    fn second_keyframe_index(&self) -> Option<usize> {
        self.frames
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, f)| f.keyframe)
            .map(|(i, _)| i)
    }

    fn duration_from(&self, index: usize) -> Duration {
        match (self.frames.get(index), self.frames.back(), &self.stream) {
            (Some(first), Some(last), Some(stream)) => Self::extent(first, last, stream),
            _ => Duration::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::media::{Timestamp, VideoCodec};

    const FPS: u64 = 60;
    const GOP: u64 = 60;
    const FRAME_NS: u64 = 1_000_000_000 / FPS;

    fn stream() -> StreamInfo {
        StreamInfo {
            codec: VideoCodec::H264,
            width: 1920,
            height: 1080,
            fps_num: FPS as u32,
            fps_den: 1,
            encoder: "test".to_owned(),
        }
    }

    fn frame(index: u64, size: usize) -> EncodedFrame {
        EncodedFrame {
            pts: Timestamp::from_nanos(index * FRAME_NS),
            dts: None,
            duration: Some(Duration::from_nanos(FRAME_NS)),
            keyframe: index.is_multiple_of(GOP),
            data: Arc::from(vec![0u8; size]),
        }
    }

    fn buffer(seconds: u64, max_bytes: usize) -> ReplayBuffer {
        let mut buffer = ReplayBuffer::new(ReplayLimits {
            max_duration: Duration::from_secs(seconds),
            max_bytes,
        });
        buffer.set_stream(stream());
        buffer
    }

    fn fill(buffer: &mut ReplayBuffer, frames: u64, size: usize) {
        for i in 0..frames {
            buffer.push(frame(i, size));
        }
    }

    #[test]
    fn drops_frames_until_first_keyframe() {
        let mut buffer = buffer(10, usize::MAX);
        for i in 30..60 {
            buffer.push(frame(i, 10));
        }
        assert_eq!(buffer.stats().frames, 0);
        assert_eq!(buffer.dropped_leading_frames(), 30);
        buffer.push(frame(60, 10));
        assert_eq!(buffer.stats().frames, 1);
    }

    #[test]
    fn duration_is_bounded_to_whole_gops() {
        let mut buffer = buffer(10, usize::MAX);
        fill(&mut buffer, FPS * 60, 10);
        let stats = buffer.stats();
        assert!(stats.duration >= Duration::from_secs(10), "{stats:?}");
        assert!(stats.duration <= Duration::from_secs(11), "{stats:?}");
        assert!(buffer.frames.front().is_some_and(|f| f.keyframe));
    }

    #[test]
    fn bytes_are_bounded_to_whole_gops() {
        let gop_bytes = 10 * GOP as usize;
        let mut buffer = buffer(600, gop_bytes * 3);
        fill(&mut buffer, FPS * 60, 10);
        let stats = buffer.stats();
        assert!(stats.bytes <= gop_bytes * 3, "{stats:?}");
        assert!(stats.bytes > gop_bytes * 2, "{stats:?}");
        assert!(buffer.frames.front().is_some_and(|f| f.keyframe));
    }

    #[test]
    fn snapshot_starts_at_keyframe_and_covers_request() {
        let mut buffer = buffer(30, usize::MAX);
        fill(&mut buffer, FPS * 25 + 17, 10);
        let snap = buffer
            .snapshot_last(Duration::from_secs(10))
            .expect("snapshot");
        assert!(snap.frames[0].keyframe);
        assert!(!snap.truncated);
        assert!(
            snap.duration >= Duration::from_secs(10),
            "{:?}",
            snap.duration
        );
        assert!(
            snap.duration < Duration::from_secs(11),
            "{:?}",
            snap.duration
        );
        let last = snap.frames.last().expect("last");
        assert_eq!(last.pts, Timestamp::from_nanos((FPS * 25 + 16) * FRAME_NS));
    }

    #[test]
    fn snapshot_is_truncated_when_buffer_is_short() {
        let mut buffer = buffer(30, usize::MAX);
        fill(&mut buffer, FPS * 4, 10);
        let snap = buffer
            .snapshot_last(Duration::from_secs(10))
            .expect("snapshot");
        assert!(snap.truncated);
        assert_eq!(snap.frames.len(), (FPS * 4) as usize);
        let error = snap.duration.abs_diff(Duration::from_secs(4));
        assert!(error < Duration::from_millis(1), "{:?}", snap.duration);
    }

    #[test]
    fn snapshot_on_empty_buffer_is_none() {
        let buffer = buffer(30, usize::MAX);
        assert!(buffer.snapshot_last(Duration::from_secs(5)).is_none());
    }

    #[test]
    fn changing_stream_clears_frames() {
        let mut buffer = buffer(30, usize::MAX);
        fill(&mut buffer, 120, 10);
        assert_eq!(buffer.stats().frames, 120);
        buffer.set_stream(stream());
        assert_eq!(buffer.stats().frames, 120);
        buffer.set_stream(StreamInfo {
            width: 1280,
            ..stream()
        });
        assert_eq!(buffer.stats().frames, 0);
        assert_eq!(buffer.stats().bytes, 0);
    }

    #[test]
    fn shrinking_limits_evicts_immediately() {
        let mut buffer = buffer(60, usize::MAX);
        fill(&mut buffer, FPS * 50, 10);
        buffer.set_limits(ReplayLimits {
            max_duration: Duration::from_secs(5),
            max_bytes: usize::MAX,
        });
        let stats = buffer.stats();
        assert!(stats.duration <= Duration::from_secs(6), "{stats:?}");
    }
}
