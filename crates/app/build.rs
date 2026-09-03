/// GStreamer DLLs are delay loaded so the process can start without them on
/// PATH and point the loader at the installed runtime first (see
/// `src/bootstrap.rs`). The list mirrors the imports of the binary.
const DELAY_LOADED: &[&str] = &[
    "gstreamer-1.0-0.dll",
    "gstbase-1.0-0.dll",
    "gstapp-1.0-0.dll",
    "gstpbutils-1.0-0.dll",
    "gobject-2.0-0.dll",
    "glib-2.0-0.dll",
    "gio-2.0-0.dll",
];

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    if let Err(err) = slint_build::compile_with_config("ui/app.slint", config) {
        panic!("failed to compile Slint UI: {err}");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        for dll in DELAY_LOADED {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
}
