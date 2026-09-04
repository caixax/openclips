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

#[cfg(not(windows))]
pub fn play_clip_saved() {
    warn!("clip sound is not available on this platform");
}
