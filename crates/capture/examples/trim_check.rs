//! Diagnostic: cuts a clip with both trim paths and reports what came out.
//!
//! ```text
//! cargo run -p openclips-capture --example trim_check -- <input.mp4> <start_s> <end_s> <out_dir>
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use openclips_capture::TrimJob;
use openclips_core::trim::{TrimMode, TrimRange};

fn main() {
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(args.next().expect("input path"));
    let start: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let end: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(6.0);
    let out_dir = PathBuf::from(args.next().unwrap_or_else(|| ".".to_owned()));

    let backend = openclips_capture::create_backend().expect("backend");
    let tools = backend.media_tools();
    let info = tools.probe(&input).expect("probe");
    println!(
        "input: {:?} {}x{} audio={}",
        info.duration, info.width, info.height, info.has_audio
    );
    let keyframes = tools.keyframes(&input).expect("keyframes");
    println!(
        "keyframes: {} (first {:?})",
        keyframes.len(),
        keyframes.iter().take(4).collect::<Vec<_>>()
    );

    let range = TrimRange::new(
        Duration::from_secs_f64(start),
        Duration::from_secs_f64(end),
        info.duration,
    )
    .expect("range");
    println!("snapped: {:?}", range.snapped_to_keyframes(&keyframes));

    for (mode, name) in [
        (TrimMode::StreamCopy, "copy.mp4"),
        (TrimMode::FrameAccurate, "exact.mp4"),
    ] {
        let output = out_dir.join(name);
        let _ = std::fs::remove_file(&output);
        let job = TrimJob {
            input: input.clone(),
            output: output.clone(),
            range,
            mode,
            video_bitrate_kbps: 12_000,
            audio_bitrate_kbps: 160,
        };
        let started = Instant::now();
        match tools.trim(&job) {
            Ok(clip) => match tools.probe(&clip.path) {
                Ok(probed) => println!(
                    "{mode:?}: {} bytes in {:.2} s, file duration {:?}, audio={}",
                    clip.bytes,
                    started.elapsed().as_secs_f64(),
                    probed.duration,
                    probed.has_audio
                ),
                Err(err) => println!(
                    "{mode:?}: wrote {} bytes in {:.2} s but the file does not probe: {err}",
                    clip.bytes,
                    started.elapsed().as_secs_f64()
                ),
            },
            Err(err) => println!("{mode:?}: failed: {err}"),
        }
    }
}
