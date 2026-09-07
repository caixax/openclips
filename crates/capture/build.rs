/// GStreamer's Direct3D 11 helper library (`gstd3d11-1.0`) has no Rust
/// binding; game capture declares the few entry points it uses by hand (see
/// `src/windows/game_capture/gpu.rs`) and links the library found through
/// pkg-config, the same way the GStreamer crates find theirs.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if let Err(err) = pkg_config::Config::new().probe("gstreamer-d3d11-1.0") {
        panic!("the GStreamer D3D11 library (gstreamer-d3d11-1.0) was not found: {err}");
    }
}
