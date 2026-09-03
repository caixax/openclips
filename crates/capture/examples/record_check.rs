//! Diagnostic: captures the primary display into a session recording for a
//! few seconds and then exits without finalising the file, the way a crash
//! would. Inspect the `.mp4.part` left behind to see how much survived.
//!
//! ```text
//! cargo run -p openclips-capture --example record_check -- <seconds> <out.mp4> [finish]
//! ```

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openclips_capture::{CaptureError, FrameSink, RecordingSession};
use openclips_core::capture::{CaptureSettings, choose_encoder};
use openclips_core::config::{AudioConfig, CaptureConfig, EncoderPreference};
use openclips_core::media::{AudioPacket, AudioTrackInfo, EncodedFrame, StreamInfo};

struct Sink {
    recorder: Arc<dyn openclips_capture::Recorder>,
    path: PathBuf,
    stream: Mutex<Option<StreamInfo>>,
    audio: Mutex<Vec<AudioTrackInfo>>,
    session: Mutex<Option<Box<dyn RecordingSession>>>,
    frames: Mutex<u64>,
    packets: Mutex<u64>,
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
            let audio = self.audio.lock().expect("lock").clone();
            if audio.is_empty() {
                return;
            }
            println!("opening session with {} audio track(s)", audio.len());
            *session = Some(
                self.recorder
                    .start(&stream, &audio, &self.path)
                    .expect("start recording"),
            );
        }
        if let Some(s) = session.as_mut() {
            if *self.frames.lock().expect("lock") == 0 {
                println!("first video frame pts {:?}", frame.pts.as_duration());
            }
            s.push(&frame).expect("push");
            *self.frames.lock().expect("lock") += 1;
        }
    }

    fn on_audio_track(&self, info: AudioTrackInfo) {
        self.audio.lock().expect("lock").push(info);
    }

    fn on_audio(&self, packet: AudioPacket) {
        if *self.packets.lock().expect("lock") == 0 {
            println!(
                "first audio packet: track {} pts {:?}",
                packet.track,
                packet.pts.as_duration()
            );
        }
        if let Some(s) = self.session.lock().expect("lock").as_mut() {
            s.push_audio(&packet).expect("push audio");
            *self.packets.lock().expect("lock") += 1;
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
    let settings = CaptureSettings::from_config(
        &CaptureConfig::default(),
        &AudioConfig::default(),
        encoder,
        None,
    );
    let sink = Arc::new(Sink {
        recorder: backend.recorder(),
        path,
        stream: Mutex::new(None),
        audio: Mutex::new(Vec::new()),
        session: Mutex::new(None),
        frames: Mutex::new(0),
        packets: Mutex::new(0),
    });
    backend
        .start(&settings, sink.clone())
        .expect("start capture");
    let finish = args.next().is_some_and(|mode| mode == "finish");
    std::thread::sleep(Duration::from_secs(seconds));
    println!(
        "frames pushed: {}, audio packets pushed: {}",
        sink.frames.lock().expect("lock"),
        sink.packets.lock().expect("lock")
    );
    if finish {
        backend.stop();
        let session = sink.session.lock().expect("lock").take();
        match session.map(|s| s.finish()) {
            Some(Ok(clip)) => println!("finished: {} ({} bytes)", clip.path.display(), clip.bytes),
            Some(Err(err)) => println!("finish failed: {err}"),
            None => println!("no session was opened"),
        }
        return;
    }
    std::process::exit(0);
}
