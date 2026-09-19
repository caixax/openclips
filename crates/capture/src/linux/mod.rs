//! The Linux side of the GStreamer backend: the screen through the
//! ScreenCast portal and PipeWire on Wayland and through `ximagesrc` on X11,
//! sound through the PulseAudio protocol (which PipeWire speaks
//! too), monitors through XRandR and processes through `/proc`. Everything
//! after the encoder is shared (see `crate::gst`).

mod audio;
mod monitors;
mod portal;
mod processes;
mod video;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gstreamer as gst;
use gstreamer::prelude::*;
use openclips_core::capture::{
    AudioDeviceInfo, AudioSourceSettings, CaptureSettings, EncoderKind, MonitorInfo,
};

use crate::backend::{FrameSink, IconExtractor, ProcessWatcher};
use crate::error::CaptureError;
use crate::gst::encoders::EncoderSpec;
use crate::gst::{Platform, VideoHead, make};

#[derive(Clone)]
pub struct Native {
    processes: Arc<processes::ProcWatcher>,
    icons: Arc<NoIcons>,
}

/// Executables carry no icon on Linux; the game icons come from elsewhere
/// (Steam's cache, desktop entries) and are not wired up yet.
struct NoIcons;

impl IconExtractor for NoIcons {
    fn extract_png(&self, exe: &Path, _output: &Path) -> Result<(), CaptureError> {
        Err(CaptureError::Media {
            path: exe.to_path_buf(),
            reason: "executables have no embedded icon on Linux".to_owned(),
        })
    }
}

impl Platform for Native {
    const NAME: &'static str = "Linux (GStreamer)";

    // The screen source (`pipewiresrc` or `ximagesrc`) is checked when a
    // capture starts, because which one is needed depends on the session.
    const REQUIRED_ELEMENTS: &'static [&'static str] = &["videoconvert", "videoscale", "pulsesrc"];

    // All of them take system memory for now; the GPU paths (DMABuf into
    // VA-API, GL or CUDA memory into NVENC) come with the portal source.
    const ENCODERS: &'static [EncoderSpec] = &[
        EncoderSpec {
            kind: EncoderKind::Nvenc,
            element: "nvh264enc",
            gpu_input: false,
        },
        EncoderSpec {
            kind: EncoderKind::QuickSync,
            element: "qsvh264enc",
            gpu_input: false,
        },
        EncoderSpec {
            kind: EncoderKind::Vaapi,
            element: "vah264enc",
            gpu_input: false,
        },
        EncoderSpec {
            kind: EncoderKind::Vaapi,
            element: "vah264lpenc",
            gpu_input: false,
        },
        EncoderSpec {
            kind: EncoderKind::Vaapi,
            element: "vaapih264enc",
            gpu_input: false,
        },
        EncoderSpec {
            kind: EncoderKind::Software,
            element: "x264enc",
            gpu_input: false,
        },
        EncoderSpec {
            kind: EncoderKind::Software,
            element: "openh264enc",
            gpu_input: false,
        },
    ];

    const AAC_ENCODERS: &'static [&'static str] = &["fdkaacenc", "avenc_aac", "voaacenc", "faac"];

    fn new() -> Result<Self, CaptureError> {
        Ok(Self {
            processes: Arc::new(processes::ProcWatcher),
            icons: Arc::new(NoIcons),
        })
    }

    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        Ok(monitors::enumerate())
    }

    fn list_audio_devices(&self) -> Result<Vec<AudioDeviceInfo>, CaptureError> {
        audio::list_devices()
    }

    fn audio_source(
        &self,
        source: &AudioSourceSettings,
        name: &str,
    ) -> Result<gst::Element, CaptureError> {
        audio::make_source(source, name)
    }

    fn video_head(
        &self,
        settings: &CaptureSettings,
        _encoder: EncoderSpec,
        _sink: Arc<dyn FrameSink>,
        cancel: &AtomicBool,
    ) -> Result<VideoHead, CaptureError> {
        video::build_head(settings, cancel)
    }

    /// Scales before converting, so the colour conversion runs on the small
    /// picture.
    fn player_video_chain(max_width: i32) -> Result<Vec<gst::Element>, CaptureError> {
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", gst::IntRange::new(16, max_width))
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        let filter = make("capsfilter")?;
        filter.set_property("caps", &caps);
        Ok(vec![make("videoscale")?, make("videoconvert")?, filter])
    }

    fn process_watcher(&self) -> Arc<dyn ProcessWatcher> {
        self.processes.clone()
    }

    fn icon_extractor(&self) -> Arc<dyn IconExtractor> {
        self.icons.clone()
    }
}
