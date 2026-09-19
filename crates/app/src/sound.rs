//! The short confirmation sound played when a clip is saved. Off by
//! default; the WAV is embedded so nothing has to be found at run time.

use tracing::warn;

const CLIP_SAVED: &[u8] = include_bytes!("../assets/sounds/clip-saved.wav");

/// Plays the clip saved sound without blocking. Failures are logged once
/// per call and never surface, the clip itself is what matters.
#[cfg(windows)]
pub fn play_clip_saved() {
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
    use windows::core::PCWSTR;

    // SAFETY: the buffer is static and outlives the asynchronous playback,
    // which is what SND_MEMORY with SND_ASYNC requires.
    let ok = unsafe {
        PlaySoundW(
            PCWSTR(CLIP_SAVED.as_ptr().cast()),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        )
    };
    if !ok.as_bool() {
        warn!("the clip sound could not be played");
    }
}

/// No sound API is linked in: the WAV is written to the runtime directory
/// once and handed to whichever command line player the sound server of the
/// session ships (PipeWire, PulseAudio, ALSA, in that order).
#[cfg(not(windows))]
pub fn play_clip_saved() {
    use std::process::{Command, Stdio};

    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let file = dir.join("openclips-clip-saved.wav");
    if !file.is_file()
        && let Err(err) = std::fs::write(&file, CLIP_SAVED)
    {
        warn!("the clip sound could not be written: {err}");
        return;
    }
    for player in ["pw-play", "paplay", "aplay"] {
        let spawned = Command::new(player)
            .arg(&file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = spawned {
            // Reaped off the UI thread so no zombie is left behind.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return;
        }
    }
    warn!("the clip sound could not be played: no pw-play, paplay or aplay");
}
