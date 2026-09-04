#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bootstrap;
mod discord;
mod engine;
mod error;
mod games;
mod hotkeys;
mod library;
mod player;
mod settings;
mod shell;
mod startup;
mod steam;
mod ui;
mod updater;

use std::process::ExitCode;

use openclips_capture::platform::Platform;
use openclips_core::config::{AppPaths, Config};
use openclips_core::{APP_VERSION, logging};
use slint::ComponentHandle;
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
    info!(
        version = APP_VERSION,
        platform = Platform::current().name(),
        "starting OpenClips"
    );

    let (config, startup_warning) = load_config(&paths);
    if config.updates.check && updater::apply_pending_at_start(&paths) {
        info!("handing over to the installer");
        return Ok(());
    }
    if config.general.launch_on_startup
        && !startup::is_enabled()
        && let Err(err) = startup::apply(true)
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
    })?;
    if !app.start_minimized && !minimized_flag {
        app.window.show()?;
    }
    app.tray.show()?;

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
