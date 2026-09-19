//! Diagnostic: plays a file through the in app player for a few seconds and
//! reports how many frames reached the sink and how much processor time the
//! process spent, to compare playback paths.
//!
//! ```text
//! cargo run -p openclips-capture --example play_check -- <clip.mp4> [seconds]
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use openclips_capture::PlayerSink;

struct Counter {
    frames: AtomicU64,
    bytes: AtomicU64,
}

impl PlayerSink for Counter {
    fn on_frame(&self, _width: u32, _height: u32, rgba: &[u8]) {
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(rgba.len() as u64, Ordering::Relaxed);
    }

    fn on_finished(&self) {}

    fn on_error(&self, message: String) {
        eprintln!("playback error: {message}");
    }
}

/// User plus kernel time of this process, in milliseconds.
#[cfg(windows)]
fn cpu_time_ms() -> u64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: plain query on the current process with valid out params.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    if ok.is_err() {
        return 0;
    }
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    (ticks(kernel) + ticks(user)) / 10_000
}

/// User plus kernel time of this process, in milliseconds, from
/// `/proc/self/stat` (fields 14 and 15, in clock ticks of 10 ms).
#[cfg(not(windows))]
fn cpu_time_ms() -> u64 {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return 0;
    };
    // The process name (field 2) may hold spaces; count from its closing
    // parenthesis.
    let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
        return 0;
    };
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let ticks = |index: usize| {
        fields
            .get(index)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    };
    (ticks(11) + ticks(12)) * 10
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("clip path"));
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);
    let backend = openclips_capture::create_backend().expect("backend");
    let sink = Arc::new(Counter {
        frames: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
    });
    let mut player = backend.create_player(sink.clone()).expect("player");
    player.load(&path).expect("load");
    player.play();
    let cpu_before = cpu_time_ms();
    // OPENCLIPS_MUTE_TRACK=<n> silences that audio track halfway through,
    // the way the editor's track toggles do, then restores it at the end.
    let muted: Option<usize> = std::env::var("OPENCLIPS_MUTE_TRACK")
        .ok()
        .and_then(|s| s.parse().ok());
    std::thread::sleep(Duration::from_secs(seconds / 2));
    if let Some(track) = muted {
        player.set_track_enabled(track, false);
        println!("track {track} muted at {:?}", player.position());
    }
    std::thread::sleep(Duration::from_secs(seconds - seconds / 2));
    if let Some(track) = muted {
        player.set_track_enabled(track, true);
    }
    let cpu = cpu_time_ms() - cpu_before;
    println!(
        "position {:?} of {:?}, playing {}",
        player.position(),
        player.duration(),
        player.is_playing()
    );
    let frames = sink.frames.load(Ordering::Relaxed);
    println!(
        "{seconds} s of playback: {frames} frames, {:.1} MB delivered, {cpu} ms of processor time ({:.0}% of one core)",
        sink.bytes.load(Ordering::Relaxed) as f64 / 1_048_576.0,
        cpu as f64 / (seconds as f64 * 10.0)
    );
    player.stop();
}
