//! Diagnostic: starts capture on the backend's worker thread the way the app
//! does, reports how long the start took, and optionally cancels it part way
//! to check that a stop during a start returns promptly.
//!
//! ```text
//! cargo run -p openclips-capture --example start_check -- [cancel_after_ms]
//! ```

use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openclips_capture::{CaptureError, FrameSink};
use openclips_core::capture::{CaptureSettings, choose_encoder};
use openclips_core::config::{AudioConfig, CaptureConfig, EncoderPreference};
use openclips_core::media::{AudioPacket, AudioTrackInfo, EncodedFrame, StreamInfo};

struct Counter(Mutex<u64>);

impl FrameSink for Counter {
    fn on_stream(&self, _: StreamInfo) {}

    fn on_frame(&self, _: EncodedFrame) {
        *self.0.lock().expect("lock") += 1;
    }

    fn on_audio_track(&self, _: AudioTrackInfo) {}

    fn on_audio(&self, _: AudioPacket) {}

    fn on_error(&self, error: CaptureError) {
        eprintln!("capture error: {error}");
    }
}

fn main() {
    let cancel_after: Option<u64> = std::env::args().nth(1).and_then(|s| s.parse().ok());
    let _log = openclips_core::logging::init(&std::env::temp_dir().join("openclips-start-check"))
        .expect("logging");
    let mut backend = openclips_capture::create_backend().expect("backend");
    let encoder = choose_encoder(backend.available_encoders(), EncoderPreference::Auto)
        .cloned()
        .expect("encoder");
    let settings =
        CaptureSettings::from_config(&CaptureConfig::default(), &AudioConfig::default(), encoder);
    let sink = Arc::new(Counter(Mutex::new(0)));
    let (tx, rx) = channel();
    let started = Instant::now();
    backend.start_in_background(
        &settings,
        sink.clone(),
        Box::new(move |result| {
            let _ = tx.send(result);
        }),
    );
    println!(
        "start_in_background returned after {:?} (starting={})",
        started.elapsed(),
        backend.is_starting()
    );
    if let Some(ms) = cancel_after {
        std::thread::sleep(Duration::from_millis(ms));
        let stopping = Instant::now();
        backend.stop();
        println!("stop() took {:?}", stopping.elapsed());
    }
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => println!("started after {:?}", started.elapsed()),
        Ok(Err(err)) => println!("start ended with: {err} (after {:?})", started.elapsed()),
        Err(_) => println!("no result within a minute"),
    }
    if cancel_after.is_none() {
        std::thread::sleep(Duration::from_secs(2));
        println!(
            "running={} frames={}",
            backend.is_running(),
            sink.0.lock().expect("lock")
        );
        backend.stop();
    }
    println!("running after stop={}", backend.is_running());
}
