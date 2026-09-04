# OpenClips

A lightweight, open source game clip recorder for Windows. Keep a rolling buffer of recent gameplay in memory, press a hotkey, and the last N seconds land on your disk as an MP4. That is the whole product.

OpenClips replaces tools like ShadowPlay, ReLive and Medal with a strict philosophy:

- No cloud. No account. No telemetry. No ads. Nothing phones home.
- Everything stays local. A clip is just an MP4 file in a folder you chose.
- Low overhead. Video is encoded on the GPU (NVENC, Quick Sync, AMF) with a software fallback.
- Instant. The app lives in the tray and reacts immediately to a hotkey.

## Status

OpenClips is feature complete for its first release on Windows. The table below is kept current as features land.

| Area | Status |
| --- | --- |
| Workspace, config file, logging, tray app skeleton | Done (Sprint 0) |
| Rolling replay buffer with GPU encode and Alt+8 save | Done (Sprint 1) |
| Display selection, buffer length, hotkey rebinding, full session recording | Done (Sprint 2) |
| Multi source audio (desktop loopback and microphones) | Done (Sprint 3) |
| Clip library with thumbnails and playback | Done (Sprint 4) |
| Trim editor (stream copy and frame accurate) | Done (Sprint 5) |
| Game detection, per game profiles, bundled games database | Done (Sprint 6) |
| Fullscreen reliability pass, installer, launch on startup | Done (Sprint 7) |
| Dark icon rail UI, quality presets, player controls, any number of save hotkeys | Done (Sprint 8) |
| Hotkeys with actions, per application audio tracks, editor track muting, compression, Edited folder, own title bar, NSIS installer | Done (Sprint 9) |
| Discord Rich Presence with on/off and game name options | Done (Sprint 9) |
| Linux backend (PipeWire and desktop portal) | Future |

What works today: the app starts capturing the selected display into memory as soon as it launches, `Alt+8` writes everything the buffer holds to `Videos\OpenClips`, `Alt+9` starts or stops the buffer, `Alt+0` starts or stops a full session recording (written to a `Recordings` subfolder), and the tray menu offers the same actions. Every hotkey in Settings is a key plus an action (save the last N seconds, start or stop the buffer, start or stop a recording), so one key can save the last 15 seconds and another the last two minutes. The Settings page covers display, encoder, frame rate, bitrate, buffer length (seconds or minutes), memory cap, output folders, hotkey rebinding and start up behaviour. Displays are re-enumerated while the app runs, and a capture of a display that goes away moves to the primary display. Audio comes from any combination of playback devices (captured through WASAPI loopback) and microphones, each with its own volume and mute, mixed into one AAC track or split into desktop and microphone tracks. A device that fails mid capture is dropped and capture continues without it. The Clips page lists every clip and recording with a thumbnail, date and duration, filters by game or title, plays clips in the app with audio, and can rename, delete (to the Recycle Bin) or reveal a file in Explorer. The player has a trim mode: drag the in and out handles on the timeline (or set them at the playhead), preview the selection, and save it as a new file. The fast mode copies the streams and cuts on keyframes; the exact mode re-encodes the selection for a frame accurate cut. Replacing the original is an explicit checkbox, off by default. Running games are detected from their executable through a bundled database of about 1900 titles plus your own profiles; clips are named and tagged with the game, cards show the game's icon (extracted from the executable itself), and in per game mode capture starts and stops with the game, with per game buffer length, subfolder and display overrides.

## Building

### Prerequisites

- Rust stable, 1.92 or newer (`rustup` recommended).
- On Windows: the MSVC build tools (Visual Studio 2022 Build Tools with the "Desktop development with C++" workload).
- GStreamer 1.28 or newer, MSVC x86_64 build, installed with the development files. Download the installer from <https://gstreamer.freedesktop.org/download/> and choose the "runtime and development" install type, or run it with `/TYPE=devel`. The default location is `C:\Program Files\gstreamer\1.0\msvc_x86_64`.

  The build finds GStreamer through `pkg-config`. `.cargo/config.toml` points `PKG_CONFIG_PATH` and `PKG_CONFIG` at the default install location; set those two variables yourself if GStreamer lives elsewhere.

  At run time no `PATH` changes are needed: the GStreamer imports are delay loaded and the app looks for the runtime in a `gstreamer` folder next to the executable, then in `GSTREAMER_1_0_ROOT_MSVC_X86_64`, then in the default install locations, and shows a clear message if none exists.

  Plugins used: `d3d11` (screen capture), `nvcodec`, `qsv`, `amfcodec` and `mediafoundation` (hardware encoders), `x264` (software fallback), `videoparsersbad` (`h264parse`), `isomp4` (MP4 muxing), `app` (`appsink` and `appsrc`). All of them ship with the official installer.

### Build and run

```text
cargo build --release
cargo run --release
```

The binary is `target/release/openclips.exe`. To stage a distributable folder and zip, optionally with the GStreamer runtime bundled so it runs on a machine without GStreamer, use:

```text
powershell -File scripts\package.ps1 -BundleRuntime
```

`packaging\openclips.nsi` builds a Windows installer from that staged folder with NSIS 3 (`makensis packaging\openclips.nsi`, or `scripts\package.ps1 -BundleRuntime -Installer` to do both steps). The installer is per user (no administrator prompt), installs under `%LOCALAPPDATA%\Programs\OpenClips`, offers a desktop shortcut and a "launch when Windows starts" option; the same option lives in the app's Settings page and uses the per user Run key. Uninstalling leaves clips, settings and logs in place.

Run the checks the CI runs with:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Set `OPENCLIPS_LOG=debug` (any `tracing` filter directive) for verbose logs. Logs go to stderr and to a daily file under the local data directory.

## Configuration

Settings live in a single human readable TOML file, created with defaults on first start:

```text
Windows:  %APPDATA%\OpenClips\config\config.toml
Linux:    ~/.config/OpenClips/config.toml
```

Clips default to `Videos\OpenClips`. Logs default to `%LOCALAPPDATA%\OpenClips\data\logs`. The library index lives in `%LOCALAPPDATA%\OpenClips\data\library.json` and thumbnails in `%LOCALAPPDATA%\OpenClips\cache\thumbnails`; both are rebuilt from the clip files when missing. A malformed config file is reported in the app and ignored for that session; it is never overwritten with defaults.

The keys that matter today:

```toml
[capture]
encoder = "auto"        # auto, nvenc, quick_sync, amf, software
fps = 60
bitrate_kbps = 20000
show_cursor = false
api = "desktop_duplication"   # or graphics_capture

[capture.display]
kind = "primary"        # or kind = "monitor", id = "\\\\.\\DISPLAY2"

[replay]
start_on_launch = true
length_seconds = 30     # 5 to 1200
memory_cap_mb = 1024

[output]
# clips_dir = "D:\\Clips"
file_name_pattern = "{game} {date} {time}"

[audio]
enabled = true
separate_tracks = false   # true keeps microphones on a second track
bitrate_kbps = 160

[[audio.sources]]
id = "default"            # "default" follows the Windows default device
name = "Default output"
kind = "output"           # output = loopback of a playback device, input = microphone
enabled = true
volume = 1.0              # 0.0 to 2.0
muted = false

[games]
scope = "global"          # global = always capture, per_game = only while a known game runs

[[games.profiles]]
exe = "hl2.exe"           # lower case executable name
name = ""                 # empty = name from the bundled database
action = "buffer"         # buffer, recording or ignore
# replay_length_seconds = 120
# subfolder = "Half-Life"
# display = { kind = "primary" }

[discord]
enabled = true
show_game = true
# Application ID from discord.com/developers; empty sends nothing.
client_id = ""

# Every hotkey is a key plus an action: save_replay, toggle_replay_buffer
# or toggle_recording. For save_replay, seconds = 0 saves the whole buffer.
[[hotkeys.bindings]]
binding = "Alt+8"
action = "save_replay"
seconds = 0

[[hotkeys.bindings]]
binding = "Alt+F1"
action = "save_replay"
seconds = 15

[[hotkeys.bindings]]
binding = "Alt+9"
action = "toggle_replay_buffer"

[[hotkeys.bindings]]
binding = "Alt+0"
action = "toggle_recording"
```

Hotkeys are written as `Modifier+Key`, for example `Ctrl+Shift+F9`, `Alt+Numpad5` or `PrintScreen`. Older config files (`save_replay`, `toggle_replay_buffer`, `toggle_recording` keys or a `[[hotkeys.save]]` list) are migrated into `bindings` on load.

## Folders, editing and compression

Everything lives under one root folder (Settings, Storage; type a path or browse). Replay clips go to `Clips`, full recordings to `Recordings`, and anything the editor produces to `Edited`; each subfolder name can be changed or emptied to use the root. Files that were already in the root keep showing up in the gallery, which can also be filtered by these three kinds.

Saving from the editor asks whether to write a new file into `Edited` or replace the original. The editor lists the audio tracks of the clip and any track switched off is left out of the saved file. Compress (the bolt button) re-encodes the whole clip at 1080p or 720p with a lower bitrate into `Edited`, leaving the original untouched.

### Per application audio

Under Settings, Audio, an application (`discord.exe`, a browser, a music player) can get its own audio track. It is captured with the Windows process loopback API, so the track only contains that program, and the first such application is excluded from the default desktop output so it is not recorded twice. Because the buffer runs continuously the program has to be running when capture starts; the app watches for it and restarts capture on its own when it opens or closes (never while a recording is active). In the editor that track can then be muted, for example to drop a voice chat from a clip. Separate tracks for desktop and microphone work the same way.

## Discord

With Discord running, OpenClips can show "Clipping <game>" (with "Replay buffer on" or "Recording" underneath and a running timer) as your Discord activity, the way Medal does. It is on by default and can be switched off under Settings, Discord, where the game name can also be hidden. Discord only accepts activities from a registered application, so paste an Application ID from <https://discord.com/developers/applications> (create an application called OpenClips) into the same section; without an id nothing is sent. In that application, upload `crates/app/assets/discord-logo.png` as the App Icon and again under Rich Presence, Art Assets with the name `logo`, which is the image the activity shows. Presence runs on its own thread and reconnects quietly when Discord starts later.

## Architecture

The code is a Cargo workspace with three crates and a strict dependency direction: `app` depends on `core` and `capture`, `capture` depends on `core`, and `core` depends on nothing platform specific.

```text
crates/
  core/      Platform independent domain logic: config, errors, logging,
             the replay ring buffer, clip naming, encoder selection rules,
             the clip library index.
  capture/   The platform abstraction. Defines the CaptureBackend, FrameSink,
             ClipWriter, Recorder, MediaTools and Player traits and hosts
             the Windows GStreamer implementation. A Linux backend (pipewiresrc through the
             xdg-desktop-portal ScreenCast portal, VAAPI encode) slots in
             here without touching the other crates.
  app/       The Slint UI, tray icon, global hotkeys, the engine that
             wires capture into the buffer and the buffer into clip files,
             the library service and the in app player.
```

The capture backend and the UI never talk to each other directly. Everything flows through `core`.

### How the replay buffer works

The Windows backend runs one GStreamer pipeline:

```text
d3d11screencapturesrc -> d3d11convert -> videorate -> hardware encoder -> h264parse -> appsink
wasapi2src (loopback) -> volume -\
wasapi2src (microphone) -> volume -> audiomixer -> AAC encoder -> appsink
```

Frames stay on the GPU until they are encoded, and `videorate` pins them to an exact frame grid so that buffer math and the container frame rate are reliable. Audio and video share the pipeline clock, and both are handed to `core` as running time, so they line up in the clip. Every encoded access unit and audio packet is handed to `core`, which keeps them in a ring bounded by both duration and bytes; audio is trimmed to the oldest video frame. The ring evicts whole groups of pictures, so the oldest frame it holds is always a keyframe, and every keyframe carries its parameter sets. Nothing touches the disk until you press the hotkey.

Saving a clip snapshots the frames covering the last N seconds (starting at the newest keyframe that still satisfies N), and a second short lived pipeline muxes them into an MP4 on a worker thread:

```text
appsrc -> h264parse -> mp4mux -> filesink
```

A session recording taps the same encoded stream: a long lived `appsrc -> h264parse -> mp4mux -> filesink` pipeline receives every frame from the next keyframe on. The file is written as fragmented MP4 and finalised into a regular MP4 when the recording stops, so a crash mid recording leaves a playable file behind. Capture runs whenever the buffer or a recording needs it and stops when neither does.

Encoders are tried in order (NVENC, Quick Sync, AMF, Media Foundation, x264) and the first one that delivers a frame wins. A fallback is reported in the UI rather than hidden. Two hard learned rules shape this: the real pipeline is the probe, because a throwaway test encode was found to break the following NVENC session, and Media Foundation elements are kept out of the path until every vendor encoder has failed, because loading one into the process makes NVENC session creation fail with `NV_ENC_ERR_INVALID_VERSION`. The AAC encoder order follows the same rule: `avenc_aac`, then `voaacenc`, and `mfaacenc` only as a last resort.

### Library and playback

The library is an index of the files in the clips folder. On start and after every save it scans the folder, reads duration and dimensions with the GStreamer discoverer and renders a thumbnail with a short decode pipeline, all on a worker thread. Playback uses `playbin3` with an `appsink` video sink: decoded RGBA frames (scaled to at most 1280 pixels wide) are handed to the Slint image element, audio plays through the default output, and seeking is frame accurate.

### Trimming

Both trim paths are a seek with a segment: the pipeline prerolls, a seek with start and stop selects the range, and the muxer receives exactly that segment before end of stream. The fast path demuxes and remuxes the encoded streams (`qtdemux -> h264parse -> mp4mux`, audio alongside) with a keyframe snapping seek, so it finishes in a fraction of a second and the start lands on the previous keyframe (one second apart at the default settings). The exact path decodes with `uridecodebin`, re-encodes with the best available hardware encoder and AAC, and uses an accurate seek so the first frame is the one you picked.

### Fullscreen games and black clips

Desktop Duplication captures everything the display shows, including borderless and exclusive fullscreen games, and follows alt-tab and resolution changes: a mode change renegotiates the stream, the ring buffer restarts at the new size, and a capture error (a display going away, a driver reset) triggers an automatic restart, at most three times a minute before the failure is shown. Because a black clip must never be produced silently, the ring buffer watches the size of recent keyframes: a real picture at HD sizes never encodes below a few kilobytes per keyframe, so three tiny keyframes in a row raise a visible warning with the two fixes that work in practice (borderless windowed mode, or the Windows Graphics Capture method in Settings).

### Game detection

Every two seconds the app lists running processes (tool help snapshot) and the foreground window's process, and matches executable names against your profiles and the bundled database in `crates/core/assets/games.json`. That file is generated from the public seed list with `cargo run -p openclips-core --example build_games_db`, which drops launchers, installers and runtimes, normalizes names and resolves shared binaries. Icons are pulled from the game's own executable through the shell (`SHGetFileInfo`) and cached as PNG, so no icon database ships with the app. The optional Steam lookup only runs when you press its button and only suggests names for executables without one.

### Recovery

A recording interrupted by a crash leaves a `.mp4.part` file that is still playable thanks to the fragmented layout. On the next start the library renames such files (once they are older than a minute) to `<name> (recovered).mp4` so they appear in the gallery.

### Stack

- **Rust** for the whole application.
- **GStreamer** (via `gstreamer-rs`) for capture, encoding, muxing and, later, trimming.
- **Slint** for the UI and the tray icon. Slint is used under its royalty free desktop license.
- **Font Awesome Free** icons (CC BY 4.0), embedded as path data in `crates/app/ui/icons.slint`. See `crates/app/assets/icons`.
- The application icon (`crates/app/assets/icon.ico`, embedded into the executable through `winresource`, also used by the tray, the window and the installer) is the Font Awesome scissors on the accent colour; `discord-logo.png` is the same mark at 1024 px for Discord.

## License

OpenClips is licensed under the MIT License. See [LICENSE](LICENSE).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).
