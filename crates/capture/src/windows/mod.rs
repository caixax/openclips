//! The Windows side of the GStreamer backend: DXGI desktop duplication and
//! Windows Graphics Capture through `d3d11screencapturesrc`, game capture
//! through the OBS hook, hardware encoding on the D3D11 device and WASAPI
//! audio capture. Everything after the encoder is shared (see `crate::gst`).

mod audio;
mod game_capture;
mod icons;
mod monitors;
mod processes;
mod video;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gstreamer as gst;
use openclips_core::capture::{
    AudioDeviceInfo, AudioSourceSettings, CaptureSettings, EncoderKind, MonitorInfo,
};
use tracing::info;

use crate::backend::{FrameSink, IconExtractor, ProcessWatcher};
use crate::error::CaptureError;
use crate::gst::encoders::EncoderSpec;
use crate::gst::{Platform, VideoHead, make};

#[derive(Clone)]
pub struct Native {
    /// The signed OBS hook binaries, located once. `None` means game
    /// capture is unavailable on this install.
    hooks: Option<game_capture::Hooks>,
    processes: Arc<processes::ToolHelpWatcher>,
    icons: Arc<icons::ShellIconExtractor>,
}

impl Platform for Native {
    const NAME: &'static str = "Windows (GStreamer, D3D11)";

    const REQUIRED_ELEMENTS: &'static [&'static str] =
        &["d3d11screencapturesrc", "d3d11convert", "wasapi2src"];

    // Media Foundation elements must never run before NVENC in the same
    // process: once an MF encoder has been loaded, NVENC session creation
    // fails with NV_ENC_ERR_INVALID_VERSION. That is why `mfh264enc` sits
    // behind every vendor encoder and `mfaacenc` behind the other AAC ones.
    const ENCODERS: &'static [EncoderSpec] = &[
        EncoderSpec {
            kind: EncoderKind::Nvenc,
            element: "nvd3d11h264enc",
            gpu_input: true,
        },
        EncoderSpec {
            kind: EncoderKind::Nvenc,
            element: "nvh264enc",
            gpu_input: false,
        },
        EncoderSpec {
            kind: EncoderKind::QuickSync,
            element: "qsvh264enc",
            gpu_input: true,
        },
        EncoderSpec {
            kind: EncoderKind::Amf,
            element: "amfh264enc",
            gpu_input: true,
        },
        EncoderSpec {
            kind: EncoderKind::MediaFoundation,
            element: "mfh264enc",
            gpu_input: false,
        },
        EncoderSpec {
            kind: EncoderKind::Software,
            element: "x264enc",
            gpu_input: false,
        },
    ];

    const AAC_ENCODERS: &'static [&'static str] = &["avenc_aac", "voaacenc", "mfaacenc"];

    fn new() -> Result<Self, CaptureError> {
        let hooks = match game_capture::Hooks::locate() {
            Ok(hooks) => Some(hooks),
            Err(err) => {
                info!("game capture unavailable: {err}");
                None
            }
        };
        Ok(Self {
            hooks,
            processes: Arc::new(processes::ToolHelpWatcher),
            icons: Arc::new(icons::ShellIconExtractor),
        })
    }

    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        Ok(monitors::enumerate().into_iter().map(|m| m.info).collect())
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
        encoder: EncoderSpec,
        sink: Arc<dyn FrameSink>,
        cancel: &AtomicBool,
    ) -> Result<VideoHead, CaptureError> {
        video::build_head(settings, encoder, self.hooks.as_ref(), sink, cancel)
    }

    fn streaming_thread_started() {
        video::raise_streaming_thread();
    }

    /// The decoder is the D3D11 one when the hardware has it, so the
    /// picture stays on the GPU for the scale and the colour conversion and
    /// only the small RGBA result comes back; a software decoder's frames
    /// are uploaded first and take the same path.
    fn player_video_chain(max_width: i32) -> Result<Vec<gst::Element>, CaptureError> {
        let caps = gst::Caps::builder("video/x-raw")
            .features(["memory:D3D11Memory"])
            .field("format", "RGBA")
            .field("width", gst::IntRange::new(16, max_width))
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        let filter = make("capsfilter")?;
        gst::prelude::ObjectExt::set_property(&filter, "caps", &caps);
        Ok(vec![
            make("d3d11upload")?,
            make("d3d11convert")?,
            filter,
            make("d3d11download")?,
        ])
    }

    fn process_watcher(&self) -> Arc<dyn ProcessWatcher> {
        self.processes.clone()
    }

    fn icon_extractor(&self) -> Arc<dyn IconExtractor> {
        self.icons.clone()
    }

    fn game_capture_available(&self) -> bool {
        self.hooks.is_some()
    }
}
