//! Cutting a clip file: either by copying the encoded streams between two
//! points (instant, keyframe aligned) or by decoding and re-encoding the
//! selection (exact frames).
//!
//! The selection is a segment seek on a source pipeline whose outputs are
//! appsinks. A muxer cannot survive a flushing seek once it has started
//! writing, so it lives in its own pipeline (the recording session) and is
//! fed with whatever the seeked source produces.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use openclips_core::clip::ClipFile;
use openclips_core::media::{
    AudioCodec, AudioPacket, AudioTrackInfo, EncodedFrame, StreamInfo, VideoCodec,
};
use openclips_core::trim::{TrimMode, TrimRange};

use super::audio;
use super::encoders::{self, EncoderTuning};
use super::pipeline::running_time;
use super::recording::Mp4Session;
use crate::backend::{RecordingSession, TrimJob};
use crate::error::CaptureError;

const PREROLL_TIMEOUT_SECONDS: u64 = 60;

fn media_error(path: &Path, reason: impl ToString) -> CaptureError {
    CaptureError::Media {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    }
}

fn make(name: &str) -> Result<gst::Element, CaptureError> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| CaptureError::MissingElement(name.to_owned()))
}

fn filesrc(path: &Path) -> Result<gst::Element, CaptureError> {
    gst::ElementFactory::make("filesrc")
        .property("location", path.to_string_lossy().as_ref())
        .build()
        .map_err(|_| CaptureError::MissingElement("filesrc".to_owned()))
}

fn h264_out_caps() -> gst::Caps {
    gst::Caps::builder("video/x-h264")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .build()
}

fn aac_out_caps() -> gst::Caps {
    gst::Caps::builder("audio/mpeg")
        .field("mpegversion", 4i32)
        .field("stream-format", "raw")
        .build()
}

fn add_and_link(
    pipeline: &gst::Pipeline,
    chain: &[gst::Element],
    input: &Path,
) -> Result<(), CaptureError> {
    let refs: Vec<&gst::Element> = chain.iter().collect();
    pipeline
        .add_many(&refs)
        .map_err(|e| media_error(input, e))?;
    gst::Element::link_many(&refs).map_err(|e| media_error(input, e))
}

/// Lists the keyframe timestamps of the first video stream without
/// decoding anything.
pub fn keyframes(path: &Path) -> Result<Vec<Duration>, CaptureError> {
    let src = filesrc(path)?;
    let demux = make("qtdemux")?;
    let parse = make("h264parse")?;
    let appsink = gst_app::AppSink::builder()
        .caps(&gst::Caps::builder("video/x-h264").build())
        .sync(false)
        .max_buffers(0)
        .build();
    let sink: gst::Element = appsink.clone().upcast();

    let pipeline = gst::Pipeline::with_name("openclips-keyframes");
    pipeline
        .add_many([&src, &demux, &parse, &sink])
        .map_err(|e| media_error(path, e))?;
    src.link(&demux).map_err(|e| media_error(path, e))?;
    gst::Element::link_many([&parse, &sink]).map_err(|e| media_error(path, e))?;
    let parse_pad = parse
        .static_pad("sink")
        .ok_or_else(|| media_error(path, "h264parse has no sink pad"))?;
    demux.connect_pad_added(move |_, pad| {
        if pad.name().starts_with("video") && !parse_pad.is_linked() {
            let _ = pad.link(&parse_pad);
        }
    });

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|_| media_error(path, "could not open the file"))?;
    let mut found = Vec::new();
    while let Ok(sample) = appsink.pull_sample() {
        if let Some(buffer) = sample.buffer()
            && !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT)
            && let Some(pts) = buffer.pts()
        {
            found.push(Duration::from_nanos(pts.nseconds()));
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
    found.sort();
    found.dedup();
    Ok(found)
}

/// A seeked source pipeline with one appsink per stream.
struct Source {
    pipeline: gst::Pipeline,
    video: gst_app::AppSink,
    audio: gst_app::AppSink,
    encoder: String,
}

impl Drop for Source {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn video_appsink() -> gst_app::AppSink {
    gst_app::AppSink::builder()
        .caps(&h264_out_caps())
        .sync(false)
        .max_buffers(0)
        .build()
}

fn audio_appsink() -> gst_app::AppSink {
    gst_app::AppSink::builder()
        .caps(&aac_out_caps())
        .sync(false)
        .max_buffers(0)
        .build()
}

/// filesrc -> qtdemux -> h264parse -> appsink, audio through aacparse.
fn copy_source(input: &Path) -> Result<Source, CaptureError> {
    let pipeline = gst::Pipeline::with_name("openclips-trim-copy");
    let src = filesrc(input)?;
    let demux = make("qtdemux")?;
    let vparse = make("h264parse")?;
    vparse.set_property("config-interval", -1i32);
    let vqueue = make("queue")?;
    let video = video_appsink();
    let aparse = make("aacparse")?;
    let aqueue = make("queue")?;
    let audio = audio_appsink();

    pipeline
        .add_many([&src, &demux])
        .map_err(|e| media_error(input, e))?;
    src.link(&demux).map_err(|e| media_error(input, e))?;
    add_and_link(
        &pipeline,
        &[vparse.clone(), vqueue, video.clone().upcast()],
        input,
    )?;
    add_and_link(
        &pipeline,
        &[aparse.clone(), aqueue, audio.clone().upcast()],
        input,
    )?;

    let vpad = vparse
        .static_pad("sink")
        .ok_or_else(|| media_error(input, "h264parse has no sink pad"))?;
    let apad = aparse
        .static_pad("sink")
        .ok_or_else(|| media_error(input, "aacparse has no sink pad"))?;
    demux.connect_pad_added(move |_, pad| {
        let name = pad.name();
        if name.starts_with("video") && !vpad.is_linked() {
            let _ = pad.link(&vpad);
        } else if name.starts_with("audio") && !apad.is_linked() {
            let _ = pad.link(&apad);
        }
    });

    Ok(Source {
        pipeline,
        video,
        audio,
        encoder: "copy".to_owned(),
    })
}

/// uridecodebin -> encoders -> appsinks.
fn reencode_source(job: &TrimJob) -> Result<Source, CaptureError> {
    let input = &job.input;
    let pipeline = gst::Pipeline::with_name("openclips-trim-encode");
    let uri = super::media::file_uri(input)?;
    let decode = gst::ElementFactory::make("uridecodebin")
        .property("uri", &uri)
        .build()
        .map_err(|_| CaptureError::MissingElement("uridecodebin".to_owned()))?;

    let encoder_info = encoders::discover()
        .into_iter()
        .next()
        .ok_or(CaptureError::NoEncoder)?;
    let spec = encoders::spec_for(&encoder_info.element)
        .ok_or_else(|| CaptureError::MissingElement(encoder_info.element.clone()))?;
    let vqueue = make("queue")?;
    let vconvert = make("videoconvert")?;
    let nv12 = make("capsfilter")?;
    nv12.set_property(
        "caps",
        gst::Caps::builder("video/x-raw")
            .field("format", "NV12")
            .build(),
    );
    let venc = make(spec.element)?;
    encoders::configure(
        &venc,
        spec,
        &EncoderTuning {
            bitrate_kbps: job.video_bitrate_kbps,
            keyframe_interval: 60,
        },
    );
    let vparse = make("h264parse")?;
    vparse.set_property("config-interval", -1i32);
    let profile = make("capsfilter")?;
    profile.set_property(
        "caps",
        gst::Caps::builder("video/x-h264")
            .field("profile", "high")
            .build(),
    );
    let video = video_appsink();

    let aqueue = make("queue")?;
    let aconvert = make("audioconvert")?;
    let aresample = make("audioresample")?;
    let aenc_name = audio::choose_encoder().ok_or(CaptureError::NoAudioEncoder)?;
    let aenc = make(aenc_name)?;
    super::props::set_number(&aenc, "bitrate", i64::from(job.audio_bitrate_kbps) * 1000);
    let aparse = make("aacparse")?;
    let audio = audio_appsink();

    pipeline.add(&decode).map_err(|e| media_error(input, e))?;
    add_and_link(
        &pipeline,
        &[
            vqueue.clone(),
            vconvert,
            nv12,
            venc,
            vparse,
            profile,
            video.clone().upcast(),
        ],
        input,
    )?;
    add_and_link(
        &pipeline,
        &[
            aqueue.clone(),
            aconvert,
            aresample,
            aenc,
            aparse,
            audio.clone().upcast(),
        ],
        input,
    )?;

    let vpad = vqueue
        .static_pad("sink")
        .ok_or_else(|| media_error(input, "queue has no sink pad"))?;
    let apad = aqueue
        .static_pad("sink")
        .ok_or_else(|| media_error(input, "queue has no sink pad"))?;
    decode.connect_pad_added(move |_, pad| {
        let kind = pad
            .current_caps()
            .and_then(|c| c.structure(0).map(|s| s.name().to_string()))
            .unwrap_or_default();
        if kind.starts_with("video/") && !vpad.is_linked() {
            let _ = pad.link(&vpad);
        } else if kind.starts_with("audio/") && !apad.is_linked() {
            let _ = pad.link(&apad);
        }
    });

    Ok(Source {
        pipeline,
        video,
        audio,
        encoder: encoder_info.element,
    })
}

fn preroll_and_seek(
    source: &Source,
    input: &Path,
    range: TrimRange,
    flags: gst::SeekFlags,
) -> Result<(), CaptureError> {
    source
        .pipeline
        .set_state(gst::State::Paused)
        .map_err(|_| media_error(input, "could not open the file"))?;
    let (result, _, _) = source
        .pipeline
        .state(gst::ClockTime::from_seconds(PREROLL_TIMEOUT_SECONDS));
    result.map_err(|_| media_error(input, "could not decode the file"))?;
    source
        .pipeline
        .seek(
            1.0,
            flags,
            gst::SeekType::Set,
            gst::ClockTime::from_nseconds(range.start.as_nanos() as u64),
            gst::SeekType::Set,
            gst::ClockTime::from_nseconds(range.end.as_nanos() as u64),
        )
        .map_err(|e| media_error(input, format!("could not seek: {e}")))?;
    source
        .pipeline
        .set_state(gst::State::Playing)
        .map(|_| ())
        .map_err(|_| media_error(input, "could not start the cut"))
}

fn stream_info(video: &gst_app::AppSink, encoder: &str) -> Option<StreamInfo> {
    let caps = video.static_pad("sink")?.current_caps()?;
    let s = caps.structure(0)?;
    let (fps_num, fps_den) = s
        .get::<gst::Fraction>("framerate")
        .map(|f| (f.numer().max(0) as u32, f.denom().max(1) as u32))
        .unwrap_or((0, 1));
    Some(StreamInfo {
        codec: VideoCodec::H264,
        width: s.get::<i32>("width").unwrap_or(0).max(0) as u32,
        height: s.get::<i32>("height").unwrap_or(0).max(0) as u32,
        fps_num,
        fps_den,
        encoder: encoder.to_owned(),
    })
}

fn audio_info(sink: &gst_app::AppSink) -> Option<AudioTrackInfo> {
    let pad = sink.static_pad("sink")?;
    if !pad.is_linked() {
        return None;
    }
    let caps = pad.current_caps()?;
    let s = caps.structure(0)?;
    let codec_data = s.get::<gst::Buffer>("codec_data").ok()?;
    let map = codec_data.map_readable().ok()?;
    Some(AudioTrackInfo {
        index: 0,
        label: "Audio".to_owned(),
        codec: AudioCodec::Aac,
        sample_rate: s.get::<i32>("rate").unwrap_or(audio::SAMPLE_RATE).max(1) as u32,
        channels: s.get::<i32>("channels").unwrap_or(audio::CHANNELS).max(1) as u32,
        codec_data: Arc::from(map.as_slice()),
    })
}

pub fn trim(job: &TrimJob) -> Result<ClipFile, CaptureError> {
    let input = &job.input;
    let (source, flags) = match job.mode {
        TrimMode::StreamCopy => (
            copy_source(input)?,
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_BEFORE,
        ),
        TrimMode::FrameAccurate => (
            reencode_source(job)?,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
        ),
    };
    preroll_and_seek(&source, input, job.range, flags)?;

    // Caps are only final once the first sample is out, so peek at the video
    // before building the muxer.
    let first = source
        .video
        .pull_sample()
        .map_err(|_| media_error(input, "the selection produced no video"))?;
    let stream = stream_info(&source.video, &source.encoder)
        .ok_or_else(|| media_error(input, "could not read the video format"))?;
    let audio = audio_info(&source.audio);
    let tracks: Vec<AudioTrackInfo> = audio.iter().cloned().collect();
    let mut session = Mp4Session::open(&stream, &tracks, &job.output, false)?;

    let mut frames = 0u64;
    let mut pending = Some(first);
    let mut started = false;
    loop {
        let sample = match pending.take() {
            Some(sample) => sample,
            None => match source.video.pull_sample() {
                Ok(sample) => sample,
                Err(_) => break,
            },
        };
        let Some(buffer) = sample.buffer() else {
            continue;
        };
        let keyframe = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
        if !started && !keyframe {
            continue;
        }
        started = true;
        let map = buffer.map_readable().map_err(|e| media_error(input, e))?;
        let frame = EncodedFrame {
            pts: running_time(&sample, buffer.pts()),
            dts: buffer.dts().map(|_| running_time(&sample, buffer.dts())),
            duration: buffer.duration().map(|d| d.into()),
            keyframe,
            data: Arc::from(map.as_slice()),
        };
        session.push(&frame)?;
        frames += 1;
    }
    if frames == 0 {
        return Err(media_error(input, "the selection produced no video"));
    }

    if audio.is_some() {
        while let Ok(sample) = source.audio.pull_sample() {
            let Some(buffer) = sample.buffer() else {
                continue;
            };
            let map = buffer.map_readable().map_err(|e| media_error(input, e))?;
            session.push_audio(&AudioPacket {
                track: 0,
                pts: running_time(&sample, buffer.pts()),
                duration: buffer.duration().map(|d| d.into()),
                data: Arc::from(map.as_slice()),
            })?;
        }
    }

    Box::new(session).finish()
}
