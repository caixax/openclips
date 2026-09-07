//! Diagnostic: fills a replay buffer from the display for a few seconds and
//! writes the last seconds as a clip the way the app's save hotkey does, so
//! the clip muxer can be exercised outside the app.
//!
//! ```text
//! cargo run -p openclips-capture --example clip_check -- <seconds> <out.mp4>
//! ```

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openclips_capture::{CaptureError, FrameSink};
use openclips_core::capture::{CaptureSettings, choose_encoder};
use openclips_core::config::{AudioConfig, CaptureConfig, EncoderPreference};
use openclips_core::media::{AudioPacket, AudioTrackInfo, EncodedFrame, StreamInfo};
use openclips_core::replay::{ReplayBuffer, ReplayLimits};

struct Ring(Mutex<ReplayBuffer>);

impl FrameSink for Ring {
    fn on_stream(&self, info: StreamInfo) {
        println!("stream: {info:?}");
        self.0.lock().expect("lock").set_stream(info);
    }

    fn on_frame(&self, frame: EncodedFrame) {
        self.0.lock().expect("lock").push(frame);
    }

    fn on_audio_track(&self, info: AudioTrackInfo) {
        self.0.lock().expect("lock").set_audio_track(info);
    }

    fn on_audio(&self, packet: AudioPacket) {
        self.0.lock().expect("lock").push_audio(packet);
    }

    fn on_error(&self, error: CaptureError) {
        eprintln!("capture error: {error}");
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(6);
    let path = PathBuf::from(args.next().unwrap_or_else(|| "clip_check.mp4".to_owned()));
    let _log = openclips_core::logging::init(&std::env::temp_dir().join("openclips-clip-check"))
        .expect("logging");
    let mut backend = openclips_capture::create_backend().expect("backend");
    let encoder = choose_encoder(backend.available_encoders(), EncoderPreference::Auto)
        .cloned()
        .expect("encoder");
    // OPENCLIPS_STRETCH=1 scales to the desktop size like the app's option.
    let capture = CaptureConfig {
        stretch: std::env::var("OPENCLIPS_STRETCH").as_deref() == Ok("1"),
        ..CaptureConfig::default()
    };
    let settings = CaptureSettings::from_config(&capture, &AudioConfig::default(), encoder);
    let ring = Arc::new(Ring(Mutex::new(ReplayBuffer::new(ReplayLimits {
        max_duration: Duration::from_secs(30),
        max_bytes: 512 * 1024 * 1024,
    }))));
    backend
        .start(&settings, ring.clone())
        .expect("start capture");
    std::thread::sleep(Duration::from_secs(seconds));
    let snapshot = ring
        .0
        .lock()
        .expect("lock")
        .snapshot_last(Duration::from_secs(seconds))
        .expect("snapshot");
    println!(
        "snapshot: {} frames, {:?}, {} audio track(s), stream {}x{} @ {}/{}",
        snapshot.frames.len(),
        snapshot.duration,
        snapshot.audio.len(),
        snapshot.stream.width,
        snapshot.stream.height,
        snapshot.stream.fps_num,
        snapshot.stream.fps_den
    );
    match backend.clip_writer().write_clip(&snapshot, &path) {
        Ok(clip) => println!(
            "clip written: {} ({} bytes)",
            clip.path.display(),
            clip.bytes
        ),
        Err(err) => println!("clip failed: {err}"),
    }
    backend.stop();
}
