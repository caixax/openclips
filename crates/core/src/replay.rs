//! The rolling replay buffer: a bounded, in memory ring of encoded frames.
//!
//! Video frames are evicted one GOP at a time so that the oldest retained
//! frame is always a keyframe. The buffer therefore holds between
//! `max_duration` and `max_duration + one GOP` of footage, bounded
//! additionally by `max_bytes`. Audio packets follow the video: whatever is
//! older than the oldest video frame is dropped.

use std::collections::VecDeque;
use std::time::Duration;

use crate::media::{AudioPacket, AudioTrackInfo, EncodedFrame, StreamInfo, Timestamp};

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
    pub audio_packets: usize,
    /// Footage available from the oldest keyframe to the newest frame.
    pub duration: Duration,
    /// True when recent keyframes are so small that the picture is almost
    /// certainly black or empty, which points at a capture problem.
    pub looks_blank: bool,
}

/// Keyframes of a real picture at HD sizes are tens to hundreds of
/// kilobytes; a flat black frame encodes to a few kilobytes at most.
pub const BLANK_KEYFRAME_MAX_BYTES: usize = 12 * 1024;
/// Number of consecutive keyframes that must look blank before flagging.
pub const BLANK_KEYFRAMES_REQUIRED: usize = 3;

/// One audio track's worth of packets covering a snapshot.
#[derive(Debug, Clone)]
pub struct AudioSnapshot {
    pub info: AudioTrackInfo,
    pub packets: Vec<AudioPacket>,
}

/// The frames chosen for a clip, always starting at a keyframe.
#[derive(Debug, Clone)]
pub struct ReplaySnapshot {
    pub frames: Vec<EncodedFrame>,
    pub stream: StreamInfo,
    pub audio: Vec<AudioSnapshot>,
    pub duration: Duration,
    /// True when the buffer held less footage than was requested.
    pub truncated: bool,
}

impl ReplaySnapshot {
    pub fn origin(&self) -> Timestamp {
        self.frames.first().map(|f| f.pts).unwrap_or_default()
    }
}

#[derive(Debug)]
struct AudioTrack {
    info: AudioTrackInfo,
    packets: VecDeque<AudioPacket>,
    bytes: usize,
}

#[derive(Debug)]
pub struct ReplayBuffer {
    limits: ReplayLimits,
    stream: Option<StreamInfo>,
    frames: VecDeque<EncodedFrame>,
    audio: Vec<AudioTrack>,
    bytes: usize,
    dropped_leading: u64,
}

impl ReplayBuffer {
    pub fn new(limits: ReplayLimits) -> Self {
        Self {
            limits,
            stream: None,
            frames: VecDeque::new(),
            audio: Vec::new(),
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

    pub fn audio_tracks(&self) -> Vec<AudioTrackInfo> {
        self.audio.iter().map(|t| t.info.clone()).collect()
    }

    /// Registers or replaces a track. Replacing drops its packets.
    pub fn set_audio_track(&mut self, info: AudioTrackInfo) {
        if let Some(track) = self.audio.iter_mut().find(|t| t.info.index == info.index) {
            if track.info != info {
                self.bytes -= track.bytes;
                track.packets.clear();
                track.bytes = 0;
                track.info = info;
            }
            return;
        }
        self.audio.push(AudioTrack {
            info,
            packets: VecDeque::new(),
            bytes: 0,
        });
        self.audio.sort_by_key(|t| t.info.index);
    }

    /// Removes all audio tracks. Used when capture restarts with a
    /// different audio setup.
    pub fn clear_audio_tracks(&mut self) {
        for track in &self.audio {
            self.bytes -= track.bytes;
        }
        self.audio.clear();
    }

    pub fn clear(&mut self) {
        self.frames.clear();
        for track in &mut self.audio {
            track.packets.clear();
            track.bytes = 0;
        }
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

    pub fn push_audio(&mut self, packet: AudioPacket) {
        let Some(track) = self.audio.iter_mut().find(|t| t.info.index == packet.track) else {
            return;
        };
        track.bytes += packet.size();
        self.bytes += packet.size();
        track.packets.push_back(packet);
        self.trim_audio();
    }

    pub fn stats(&self) -> ReplayStats {
        ReplayStats {
            frames: self.frames.len(),
            bytes: self.bytes,
            keyframes: self.frames.iter().filter(|f| f.keyframe).count(),
            audio_packets: self.audio.iter().map(|t| t.packets.len()).sum(),
            duration: self.span(),
            looks_blank: self.looks_blank(),
        }
    }

    /// Selects the frames covering at least the last `wanted` of footage,
    /// starting at the latest keyframe that still satisfies that length,
    /// together with the audio packets inside that window.
    pub fn snapshot_last(&self, wanted: Duration) -> Option<ReplaySnapshot> {
        let stream = self.stream.clone()?;
        let last = self.frames.back()?;
        let target = last.pts.checked_sub_duration(wanted);

        let start = target.and_then(|target| {
            self.frames
                .iter()
                .enumerate()
                .rev()
                .find(|(_, f)| f.keyframe && f.pts <= target)
                .map(|(i, _)| i)
        });
        let start = start.unwrap_or(0);
        let frames: Vec<EncodedFrame> = self.frames.range(start..).cloned().collect();
        let first = frames.first()?;
        let duration = Self::extent(first, last, &stream);
        let end = Timestamp::from_nanos(first.pts.nanos() + duration.as_nanos() as u64);
        let audio = self
            .audio
            .iter()
            .map(|track| AudioSnapshot {
                info: track.info.clone(),
                packets: track
                    .packets
                    .iter()
                    .filter(|p| p.pts >= first.pts && p.pts < end)
                    .cloned()
                    .collect(),
            })
            .collect();
        Some(ReplaySnapshot {
            truncated: duration < wanted,
            frames,
            stream,
            audio,
            duration,
        })
    }

    /// The last few keyframes are all tiny, at a resolution where a real
    /// picture never is.
    fn looks_blank(&self) -> bool {
        let big_enough = self
            .stream
            .as_ref()
            .is_some_and(|s| s.width * s.height >= 640 * 360);
        if !big_enough {
            return false;
        }
        let recent: Vec<usize> = self
            .frames
            .iter()
            .rev()
            .filter(|f| f.keyframe)
            .take(BLANK_KEYFRAMES_REQUIRED)
            .map(|f| f.size())
            .collect();
        recent.len() == BLANK_KEYFRAMES_REQUIRED
            && recent.iter().all(|size| *size <= BLANK_KEYFRAME_MAX_BYTES)
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
        while let Some(next_gop) = self.second_keyframe_index() {
            let over_duration = self.duration_from(next_gop) >= self.limits.max_duration;
            let over_bytes = self.bytes > self.limits.max_bytes;
            if !over_duration && !over_bytes {
                break;
            }
            for _ in 0..next_gop {
                if let Some(frame) = self.frames.pop_front() {
                    self.bytes -= frame.size();
                }
            }
        }
        self.trim_audio();
    }

    /// Drops audio older than the oldest video frame. Without video, audio
    /// is bounded by the duration limit on its own.
    fn trim_audio(&mut self) {
        let floor = match self.frames.front() {
            Some(first) => first.pts,
            None => {
                let newest = self
                    .audio
                    .iter()
                    .filter_map(|t| t.packets.back().map(|p| p.pts))
                    .max();
                match newest.and_then(|n| n.checked_sub_duration(self.limits.max_duration)) {
                    Some(floor) => floor,
                    None => return,
                }
            }
        };
        for track in &mut self.audio {
            while let Some(packet) = track.packets.front() {
                if packet.pts >= floor {
                    break;
                }
                let size = packet.size();
                track.packets.pop_front();
                track.bytes -= size;
                self.bytes -= size;
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
    use crate::media::{AudioCodec, Timestamp, VideoCodec};

    const FPS: u64 = 60;
    const GOP: u64 = 60;
    const FRAME_NS: u64 = 1_000_000_000 / FPS;
    const AUDIO_NS: u64 = 21_333_333;

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

    fn track(index: u32) -> AudioTrackInfo {
        AudioTrackInfo {
            index,
            label: format!("track {index}"),
            codec: AudioCodec::Aac,
            sample_rate: 48_000,
            channels: 2,
            codec_data: Arc::from(vec![0x11, 0x90]),
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

    fn packet(track: u32, index: u64) -> AudioPacket {
        AudioPacket {
            track,
            pts: Timestamp::from_nanos(index * AUDIO_NS),
            duration: Some(Duration::from_nanos(AUDIO_NS)),
            data: Arc::from(vec![0u8; 100]),
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

    #[test]
    fn tiny_keyframes_flag_a_blank_capture() {
        let mut blank = buffer(30, usize::MAX);
        for i in 0..FPS * 4 {
            blank.push(frame(i, 2_000));
        }
        assert!(blank.stats().looks_blank, "four tiny keyframes look blank");

        let mut real = buffer(30, usize::MAX);
        for i in 0..FPS * 4 {
            let size = if i.is_multiple_of(GOP) {
                150_000
            } else {
                2_000
            };
            real.push(frame(i, size));
        }
        assert!(!real.stats().looks_blank, "real keyframes are large");

        let mut short = buffer(30, usize::MAX);
        fill(&mut short, FPS, 2_000);
        assert!(
            !short.stats().looks_blank,
            "one keyframe is not enough evidence"
        );
    }

    #[test]
    fn audio_follows_video_eviction() {
        let mut buffer = buffer(10, usize::MAX);
        buffer.set_audio_track(track(0));
        let mut audio_index = 0;
        for i in 0..FPS * 30 {
            buffer.push(frame(i, 10));
            while audio_index * AUDIO_NS <= i * FRAME_NS {
                buffer.push_audio(packet(0, audio_index));
                audio_index += 1;
            }
        }
        let oldest_video = buffer.frames.front().expect("frame").pts;
        let oldest_audio = buffer.audio[0].packets.front().expect("packet").pts;
        assert!(oldest_audio >= oldest_video);
        assert!(oldest_audio.saturating_sub(oldest_video) < Duration::from_millis(30));
        assert_eq!(buffer.stats().audio_packets, buffer.audio[0].packets.len());
    }

    #[test]
    fn snapshot_includes_audio_inside_window() {
        let mut buffer = buffer(30, usize::MAX);
        buffer.set_audio_track(track(0));
        buffer.set_audio_track(track(1));
        for i in 0..FPS * 20 {
            buffer.push(frame(i, 10));
        }
        for i in 0..1000 {
            buffer.push_audio(packet(0, i));
            buffer.push_audio(packet(1, i));
        }
        let snap = buffer
            .snapshot_last(Duration::from_secs(5))
            .expect("snapshot");
        assert_eq!(snap.audio.len(), 2);
        let origin = snap.origin();
        let end = origin.nanos() + snap.duration.as_nanos() as u64;
        for track in &snap.audio {
            assert!(!track.packets.is_empty());
            assert!(
                track
                    .packets
                    .iter()
                    .all(|p| p.pts >= origin && p.pts.nanos() < end)
            );
        }
        let expected = (snap.duration.as_nanos() as u64 / AUDIO_NS) as usize;
        assert!((snap.audio[0].packets.len() as i64 - expected as i64).abs() <= 1);
    }

    #[test]
    fn unknown_audio_track_is_ignored_and_tracks_can_be_replaced() {
        let mut buffer = buffer(30, usize::MAX);
        buffer.push_audio(packet(3, 0));
        assert_eq!(buffer.stats().audio_packets, 0);

        buffer.set_audio_track(track(0));
        buffer.push_audio(packet(0, 0));
        assert_eq!(buffer.stats().audio_packets, 1);
        buffer.set_audio_track(track(0));
        assert_eq!(buffer.stats().audio_packets, 1);
        buffer.set_audio_track(AudioTrackInfo {
            channels: 1,
            ..track(0)
        });
        assert_eq!(buffer.stats().audio_packets, 0);
        assert_eq!(buffer.stats().bytes, 0);
    }

    #[test]
    fn audio_without_video_is_bounded_by_duration() {
        let mut buffer = buffer(2, usize::MAX);
        buffer.set_audio_track(track(0));
        for i in 0..1000 {
            buffer.push_audio(packet(0, i));
        }
        let packets = buffer.audio[0].packets.len() as u64;
        assert!(packets * AUDIO_NS <= 2_100_000_000, "{packets}");
        assert!(packets * AUDIO_NS >= 1_900_000_000, "{packets}");
    }
}
