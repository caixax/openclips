#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod error;
mod hotkeys;
mod shell;
mod ui;

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
    info!(
        version = APP_VERSION,
        platform = Platform::current().name(),
        "starting OpenClips"
    );

    let (config, startup_warning) = load_config(&paths);
    let clips_dir = config.clips_dir(&paths);

    let (engine, engine_warning) = match Engine::new(config.clone(), clips_dir.clone()) {
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
        clips_dir,
        engine,
        startup_warning,
    })?;
    if !app.config.general.start_minimized {
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
