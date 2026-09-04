#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bootstrap;
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
    if first_run && let Some(language) = startup::installer_language() {
        info!(
            "first start, using the installer language {}",
            language.code()
        );
        config.general.language = language;
        if let Err(err) = config.save(&paths.config_file()) {
            warn!("could not store the installer language: {err}");
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
            (None, Some(format!("Capture is unavailable: {err}")))
        }
    };
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
    Ok(())
}

/// A broken config file must not stop the app from starting, but the user
/// has to be told that their edits were ignored.
fn load_config(paths: &AppPaths) -> (Config, Option<String>) {
    let path = paths.config_file();
    match Config::load_or_create(&path) {
        Ok(config) => (config, None),
        Err(err) => {
            warn!("{err}; falling back to default settings");
            (
                Config::default(),
                Some(format!("Settings were not applied: {err}")),
            )
        }
    }
}
