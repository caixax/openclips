//! Diagnostic: captures the primary display into a session recording for a
//! few seconds and then exits without finalising the file, the way a crash
//! would. Inspect the `.mp4.part` left behind to see how much survived.
//!
//! ```text
//! cargo run -p openclips-capture --example record_check -- <seconds> <out.mp4>
//! ```

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openclips_capture::{CaptureError, FrameSink, RecordingSession};
use openclips_core::capture::{CaptureSettings, choose_encoder};
use openclips_core::config::{CaptureConfig, EncoderPreference};
use openclips_core::media::{EncodedFrame, StreamInfo};

struct Sink {
    recorder: Arc<dyn openclips_capture::Recorder>,
    path: PathBuf,
    stream: Mutex<Option<StreamInfo>>,
    session: Mutex<Option<Box<dyn RecordingSession>>>,
    frames: Mutex<u64>,
}

impl FrameSink for Sink {
    fn on_stream(&self, info: StreamInfo) {
        *self.stream.lock().expect("lock") = Some(info);
    }

    fn on_frame(&self, frame: EncodedFrame) {
        let mut session = self.session.lock().expect("lock");
        if session.is_none() {
            if !frame.keyframe {
                return;
            }
            let stream = self.stream.lock().expect("lock").clone().expect("stream");
            *session = Some(
                self.recorder
                    .start(&stream, &self.path)
                    .expect("start recording"),
            );
        }
        if let Some(s) = session.as_mut() {
            s.push(&frame).expect("push");
            *self.frames.lock().expect("lock") += 1;
        }
    }

    fn on_error(&self, error: CaptureError) {
        eprintln!("capture error: {error}");
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);
    let path = PathBuf::from(args.next().unwrap_or_else(|| "record_check.mp4".to_owned()));

    let mut backend = openclips_capture::create_backend().expect("backend");
    let encoder = choose_encoder(backend.available_encoders(), EncoderPreference::Auto)
        .cloned()
        .expect("encoder");
    let settings = CaptureSettings::from_config(&CaptureConfig::default(), encoder, None);
    let sink = Arc::new(Sink {
        recorder: backend.recorder(),
        path,
        stream: Mutex::new(None),
        session: Mutex::new(None),
        frames: Mutex::new(0),
    });
    backend
        .start(&settings, sink.clone())
        .expect("start capture");
    std::thread::sleep(Duration::from_secs(seconds));
    println!("frames pushed: {}", sink.frames.lock().expect("lock"));
    std::process::exit(0);
}
