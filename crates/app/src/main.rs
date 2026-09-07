#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bootstrap;
mod crash;
mod discord;
mod engine;
mod error;
mod games;
mod gpu;
mod hotkeys;
mod i18n;
mod instance;
mod library;
mod player;
mod settings;
mod shell;
mod sound;
mod startup;
mod steam;
mod toast;
mod ui;
mod updater;

use std::process::ExitCode;

use openclips_capture::platform::Platform;
use openclips_core::config::{AppPaths, Config};
use openclips_core::{APP_VERSION, logging};
use tracing::{error, info, warn};

use crate::engine::Engine;
use crate::error::AppError;

/// Every encoded frame and audio packet is its own allocation that lives
/// for the length of the buffer (up to twenty minutes) among short lived
/// ones. The system heap fragments under that pattern and the process
/// grows past the buffer's cap; mimalloc keeps the size classes apart.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("fatal: {err}");
            eprintln!("openclips: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), AppError> {
    let paths = AppPaths::discover()?;
    let _log_guard = logging::init(&paths.log_dir)?;
    crash::install(&paths.data_dir);
    info!(
        "crash dumps go to {}",
        crash::dump_dir(&paths.data_dir).display()
    );
    let runtime = match bootstrap::locate() {
        Ok(runtime) => runtime,
        Err(message) => {
            error!("{message}");
            rfd::MessageDialog::new()
                .set_title("OpenClips")
                .set_level(rfd::MessageLevel::Error)
                .set_description(&message)
                .show();
            return Err(AppError::Runtime(message));
        }
    };
    info!("GStreamer runtime: {}", runtime.bin.display());
    let minimized_flag = std::env::args().any(|a| a == "--minimized");
    let show_flag = std::env::args().any(|a| a == "--show");
    info!(
        version = APP_VERSION,
        platform = Platform::current().name(),
        "starting OpenClips"
    );

    let Some(instance) = instance::claim() else {
        info!("OpenClips is already running; asked it to show its window");
        return Ok(());
    };

    gpu::raise_gpu_priority();
    let first_run = !paths.config_file().exists();
    let (mut config, startup_warning) = load_config(&paths);
    if first_run {
        if let Some(language) = startup::installer_language() {
            info!(
                "first start, using the installer language {}",
                language.code()
            );
            config.general.language = language;
        }
        config.general.intro_done = false;
        if let Err(err) = config.save(&paths.config_file()) {
            warn!("could not store the first start settings: {err}");
        }
    }
    i18n::set_language(config.general.language);
    if config.updates.check && updater::apply_pending_at_start(&paths) {
        info!("handing over to the installer");
        return Ok(());
    }
    if config.general.launch_on_startup
        && !startup::is_enabled()
        && let Err(err) = startup::apply(true, config.general.start_minimized)
    {
        warn!("could not refresh the launch on startup entry: {err}");
    }

    let (engine, engine_warning) = match Engine::new(config.clone(), paths.clone()) {
        Ok(engine) => (Some(engine), None),
        Err(err) => {
            error!("capture is unavailable: {err}");
            (
                None,
                Some(
                    i18n::tr("Capture is unavailable: {error}")
                        .replace("{error}", &err.to_string()),
                ),
            )
        }
    };
    // Translated here, after the language is known.
    let startup_warning = startup_warning
        .map(|err| i18n::tr("Settings were not applied: {error}").replace("{error}", &err));
    let startup_warning = [startup_warning, engine_warning]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");

    let app = ui::build(ui::Context {
        paths,
        config,
        engine,
        startup_warning,
        instance,
    })?;
    // Only the Windows startup launch (which passes --minimized when the
    // user asked for it) opens in the tray; a launch by hand shows the window.
    if show_flag || !minimized_flag {
        app.show_window()?;
    }
    app.show_tray()?;

    slint::run_event_loop_until_quit()?;
    info!("event loop finished, shutting down");
    app.shutdown();
    Ok(())
}

/// A broken config file must not stop the app from starting, but the user
/// has to be told that their edits were ignored. Returns the error text,
/// translated by the caller once the language is known.
fn load_config(paths: &AppPaths) -> (Config, Option<String>) {
    let path = paths.config_file();
    match Config::load_or_create(&path) {
        Ok(config) => (config, None),
        Err(err) => {
            warn!("{err}; falling back to default settings");
            (Config::default(), Some(err.to_string()))
        }
    }
}
