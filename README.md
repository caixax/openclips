# OpenClips

A lightweight, open source game clip recorder for Windows. Keep a rolling
buffer of recent gameplay in memory, press a hotkey, and the last N seconds
land on your disk as an MP4. That is the whole product.

OpenClips replaces tools like ShadowPlay, ReLive and Medal with a strict
philosophy:

- No cloud. No account. No telemetry. No ads. Nothing phones home.
- Everything stays local. A clip is just an MP4 file in a folder you chose.
- Low overhead. Video is encoded on the GPU (NVENC, Quick Sync, AMF) with a
  software fallback.
- Instant. The app lives in the tray and reacts immediately to a hotkey.

## Status

OpenClips is in early development. The table below is kept current as
features land.

| Area | Status |
| --- | --- |
| Workspace, config file, logging, tray app skeleton | Done (Sprint 0) |
| Rolling replay buffer with GPU encode and Alt+8 save | Done (Sprint 1) |
| Display selection, buffer length, hotkey rebinding, full session recording | Done (Sprint 2) |
| Multi source audio (desktop loopback and microphones) | Planned (Sprint 3) |
| Clip library with thumbnails and playback | Planned (Sprint 4) |
| Trim editor (stream copy and frame accurate) | Planned (Sprint 5) |
| Game detection, per game profiles, bundled games database | Planned (Sprint 6) |
| Fullscreen reliability pass, installer, launch on startup | Planned (Sprint 7) |
| Linux backend (PipeWire and desktop portal) | Future |

What works today: the app starts capturing the selected display into memory
as soon as it launches, `Alt+8` writes the last N seconds to
`Videos\OpenClips`, `Alt+9` starts or stops the buffer, `Alt+0` starts or
stops a full session recording (written to a `Recordings` subfolder), and
the tray menu offers the same actions. The Settings page covers display,
encoder, frame rate, bitrate, buffer length (seconds or minutes), memory
cap, output folders, hotkey rebinding and start up behaviour. Displays are
re-enumerated while the app runs, and a capture of a display that goes away
moves to the primary display. Clips are video only until Sprint 3.

## Building

### Prerequisites

- Rust stable, 1.92 or newer (`rustup` recommended).
- On Windows: the MSVC build tools (Visual Studio 2022 Build Tools with the
  "Desktop development with C++" workload).
- GStreamer 1.28 or newer, MSVC x86_64 build, installed with the development
  files. Download the installer from
  <https://gstreamer.freedesktop.org/download/> and choose the "runtime and
  development" install type, or run it with `/TYPE=devel`. The default
  location is `C:\Program Files\gstreamer\1.0\msvc_x86_64`.

  The build finds GStreamer through `pkg-config`. `.cargo/config.toml`
  points `PKG_CONFIG_PATH` and `PKG_CONFIG` at the default install location;
  set those two variables yourself if GStreamer lives elsewhere.

  At run time the GStreamer DLLs must be on `PATH`:

  ```text
  PATH += C:\Program Files\gstreamer\1.0\msvc_x86_64\bin
  ```

  Plugins used: `d3d11` (screen capture), `nvcodec`, `qsv`, `amfcodec` and
  `mediafoundation` (hardware encoders), `x264` (software fallback),
  `videoparsersbad` (`h264parse`), `isomp4` (MP4 muxing), `app` (`appsink`
  and `appsrc`). All of them ship with the official installer.

### Build and run

```text
cargo build --release
cargo run --release
```

The binary is `target/release/openclips.exe`. Run the checks the CI runs with:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Set `OPENCLIPS_LOG=debug` (any `tracing` filter directive) for verbose logs.
Logs go to stderr and to a daily file under the local data directory.

## Configuration

Settings live in a single human readable TOML file, created with defaults on
first start:

```text
Windows:  %APPDATA%\OpenClips\config\config.toml
Linux:    ~/.config/OpenClips/config.toml
```

Clips default to `Videos\OpenClips`. Logs default to
`%LOCALAPPDATA%\OpenClips\data\logs`. A malformed config file is reported in
the app and ignored for that session; it is never overwritten with defaults.

The keys that matter today:

```toml
[capture]
encoder = "auto"        # auto, nvenc, quick_sync, amf, software
fps = 60
bitrate_kbps = 20000
show_cursor = false

[capture.display]
kind = "primary"        # or kind = "monitor", id = "\\\\.\\DISPLAY2"

[replay]
start_on_launch = true
length_seconds = 30     # 5 to 1200
memory_cap_mb = 1024

[output]
# clips_dir = "D:\\Clips"
file_name_pattern = "{game} {date} {time}"

[hotkeys]
save_replay = "Alt+8"
toggle_replay_buffer = "Alt+9"
toggle_recording = "Alt+0"
```

Hotkeys are written as `Modifier+Key`, for example `Ctrl+Shift+F9`,
`Alt+Numpad5` or `PrintScreen`.

## Architecture

The code is a Cargo workspace with three crates and a strict dependency
direction: `app` depends on `core` and `capture`, `capture` depends on `core`,
and `core` depends on nothing platform specific.

```text
crates/
  core/      Platform independent domain logic: config, errors, logging,
             the replay ring buffer, clip naming, encoder selection rules.
  capture/   The platform abstraction. Defines the CaptureBackend, FrameSink
             and ClipWriter traits and hosts the Windows GStreamer
             implementation. A Linux backend (pipewiresrc through the
             xdg-desktop-portal ScreenCast portal, VAAPI encode) slots in
             here without touching the other crates.
  app/       The Slint UI, tray icon, global hotkeys and the engine that
             wires capture into the buffer and the buffer into clip files.
```

The capture backend and the UI never talk to each other directly. Everything
flows through `core`.

### How the replay buffer works

The Windows backend runs one GStreamer pipeline:

```text
d3d11screencapturesrc -> d3d11convert -> videorate -> hardware encoder -> h264parse -> appsink
```

Frames stay on the GPU until they are encoded, and `videorate` pins them to
an exact frame grid so that buffer math and the container frame rate are
reliable. Every encoded access unit is handed to `core`, which keeps them in
a ring bounded by both duration and bytes. The ring evicts whole groups of pictures, so the oldest frame it holds
is always a keyframe, and every keyframe carries its parameter sets. Nothing
touches the disk until you press the hotkey.

Saving a clip snapshots the frames covering the last N seconds (starting at
the newest keyframe that still satisfies N), and a second short lived
pipeline muxes them into an MP4 on a worker thread:

```text
appsrc -> h264parse -> mp4mux -> filesink
```

A session recording taps the same encoded stream: a long lived
`appsrc -> h264parse -> mp4mux -> filesink` pipeline receives every frame
from the next keyframe on. The file is written as fragmented MP4 and
finalised into a regular MP4 when the recording stops, so a crash mid
recording leaves a playable file behind. Capture runs whenever the buffer or
a recording needs it and stops when neither does.

Encoders are tried in order (NVENC, Quick Sync, AMF, Media Foundation, x264)
and the first one that delivers a frame wins. A fallback is reported in the
UI rather than hidden. Running a throwaway test encode before the real
pipeline was found to break the following NVENC session, which is why the
real pipeline is the probe.

### Stack

- **Rust** for the whole application.
- **GStreamer** (via `gstreamer-rs`) for capture, encoding, muxing and,
  later, trimming.
- **Slint** for the UI and the tray icon. Slint is used under its
  royalty free desktop license.

## License

OpenClips is licensed under the MIT License. See [LICENSE](LICENSE).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).
