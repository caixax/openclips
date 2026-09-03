use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use openclips_capture::platform::Platform;
use openclips_core::APP_VERSION;
use openclips_core::config::{AppPaths, Config};
use slint::{CloseRequestResponse, ComponentHandle, Weak};
use tracing::{error, info, warn};

use crate::engine::{BufferState, Engine, EngineStatus};
use crate::error::AppError;
use crate::hotkeys::{self, HotkeyAction, Hotkeys};
use crate::shell;

mod generated {
    #![allow(
        clippy::all,
        clippy::unwrap_used,
        clippy::todo,
        missing_debug_implementations
    )]
    slint::include_modules!();
}
pub use generated::*;

const STATUS_REFRESH: Duration = Duration::from_millis(500);

pub struct Context {
    pub paths: AppPaths,
    pub config: Config,
    pub clips_dir: PathBuf,
    pub engine: Option<Engine>,
    pub startup_warning: String,
}

pub struct App {
    pub window: MainWindow,
    pub tray: TrayIcon,
    pub config: Config,
    _hotkeys: Option<Hotkeys>,
    _status_timer: slint::Timer,
}

type SharedEngine = Rc<RefCell<Option<Engine>>>;

pub fn build(ctx: Context) -> Result<App, AppError> {
    let window = MainWindow::new()?;
    let tray = TrayIcon::new()?;
    let engine: SharedEngine = Rc::new(RefCell::new(ctx.engine));

    window.set_info(AppInfo {
        version: APP_VERSION.into(),
        platform: Platform::current().name().into(),
        config_path: ctx.paths.config_file().display().to_string().into(),
        clips_dir: ctx.clips_dir.display().to_string().into(),
        log_dir: ctx.paths.log_dir.display().to_string().into(),
    });
    window.set_startup_warning(ctx.startup_warning.into());
    window.set_hotkey_hint(
        format!(
            "Press {} to save the last {} seconds.",
            ctx.config.hotkeys.save_replay, ctx.config.replay.length_seconds
        )
        .into(),
    );

    wire_folders(&window, &ctx.paths, &ctx.clips_dir);
    wire_window_lifecycle(&window, &tray);
    wire_actions(&window, &tray, &engine);

    let hotkeys = install_hotkeys(&ctx.config, &window, &engine);
    let status_timer = start_status_timer(&window, &tray, &engine);

    if ctx.config.replay.start_on_launch {
        run_engine(&engine, &window, |e| e.start_buffer());
    }
    refresh_status(&window, &tray, &engine);

    Ok(App {
        window,
        tray,
        config: ctx.config,
        _hotkeys: hotkeys,
        _status_timer: status_timer,
    })
}

fn wire_folders(window: &MainWindow, paths: &AppPaths, clips_dir: &Path) {
    let config_dir = paths.config_dir.clone();
    window.on_open_config_dir(move || shell::open_folder(&config_dir));
    let clips_dir = clips_dir.to_path_buf();
    window.on_open_clips_dir(move || shell::open_folder(&clips_dir));
}

fn wire_window_lifecycle(window: &MainWindow, tray: &TrayIcon) {
    window.window().on_close_requested(|| {
        info!("window closed, staying in the tray");
        CloseRequestResponse::HideWindow
    });

    let weak = window.as_weak();
    tray.on_open_window(move || {
        if let Some(window) = weak.upgrade()
            && let Err(err) = window.show()
        {
            error!("could not show the main window: {err}");
        }
    });
    tray.on_quit(|| {
        info!("quit requested from the tray");
        if let Err(err) = slint::quit_event_loop() {
            error!("could not stop the event loop: {err}");
        }
    });
}

fn wire_actions(window: &MainWindow, tray: &TrayIcon, engine: &SharedEngine) {
    let (e, w) = (engine.clone(), window.as_weak());
    window.on_toggle_buffer(move || toggle_buffer(&e, &w));
    let (e, w) = (engine.clone(), window.as_weak());
    tray.on_toggle_buffer(move || toggle_buffer(&e, &w));

    let (e, w) = (engine.clone(), window.as_weak());
    window.on_save_clip(move || save_clip(&e, &w));
    let (e, w) = (engine.clone(), window.as_weak());
    tray.on_save_clip(move || save_clip(&e, &w));
}

fn install_hotkeys(config: &Config, window: &MainWindow, engine: &SharedEngine) -> Option<Hotkeys> {
    let (e, w) = (engine.clone(), window.as_weak());
    let dispatch = move |action: HotkeyAction| match action {
        HotkeyAction::SaveReplay => save_clip(&e, &w),
        HotkeyAction::ToggleReplayBuffer => toggle_buffer(&e, &w),
        HotkeyAction::ToggleRecording => info!("full session recording is not implemented yet"),
    };
    match hotkeys::install(&config.hotkeys, dispatch) {
        Ok(hotkeys) => Some(hotkeys),
        Err(err) => {
            warn!("global hotkeys are unavailable: {err}");
            window.set_startup_warning(format!("Global hotkeys are unavailable: {err}").into());
            None
        }
    }
}

fn start_status_timer(window: &MainWindow, tray: &TrayIcon, engine: &SharedEngine) -> slint::Timer {
    let timer = slint::Timer::default();
    let (e, w, t) = (engine.clone(), window.as_weak(), tray.as_weak());
    timer.start(slint::TimerMode::Repeated, STATUS_REFRESH, move || {
        if let (Some(window), Some(tray)) = (w.upgrade(), t.upgrade()) {
            refresh_status(&window, &tray, &e);
        }
    });
    timer
}

fn run_engine(
    engine: &SharedEngine,
    window: &MainWindow,
    action: impl FnOnce(&mut Engine) -> Result<(), AppError>,
) {
    let mut slot = engine.borrow_mut();
    let Some(engine) = slot.as_mut() else {
        window.set_capture_error("Capture is unavailable on this system.".into());
        return;
    };
    match action(engine) {
        Ok(()) => window.set_capture_error("".into()),
        Err(err) => {
            error!("{err}");
            window.set_capture_error(err.to_string().into());
        }
    }
}

fn toggle_buffer(engine: &SharedEngine, window: &Weak<MainWindow>) {
    if let Some(window) = window.upgrade() {
        run_engine(engine, &window, |e| e.toggle_buffer());
    }
}

fn save_clip(engine: &SharedEngine, window: &Weak<MainWindow>) {
    let Some(strong) = window.upgrade() else {
        return;
    };
    let slot = engine.borrow();
    let Some(engine) = slot.as_ref() else {
        strong.set_capture_error("Capture is unavailable on this system.".into());
        return;
    };
    strong.set_save_status("Saving clip...".into());
    let weak = window.clone();
    engine.save_clip(Box::new(move |result| {
        let _ = weak.upgrade_in_event_loop(move |window| match result {
            Ok(clip) => {
                window.set_last_clip(clip.path.display().to_string().into());
                window.set_save_status(
                    format!(
                        "Saved {} ({:.1} s, {:.1} MB)",
                        clip.path
                            .file_name()
                            .map(|n| n.to_string_lossy())
                            .unwrap_or_default(),
                        clip.duration.as_secs_f64(),
                        clip.bytes as f64 / (1024.0 * 1024.0)
                    )
                    .into(),
                );
            }
            Err(err) => window.set_save_status(format!("Could not save clip: {err}").into()),
        });
    }));
}

fn refresh_status(window: &MainWindow, tray: &TrayIcon, engine: &SharedEngine) {
    let mut slot = engine.borrow_mut();
    let Some(engine) = slot.as_mut() else {
        window.set_buffer_active(false);
        window.set_buffer_status("Unavailable".into());
        tray.set_buffer_active(false);
        return;
    };
    let status = engine.status();
    let active = status.state == BufferState::Running;
    window.set_buffer_active(active);
    tray.set_buffer_active(active);
    window.set_buffer_status(describe_state(&status).into());
    window.set_buffer_detail(describe_buffer(&status).into());
    window.set_encoder_name(format!("{} ({})", status.encoder.kind.label(), status.backend).into());
    if let BufferState::Failed(reason) = &status.state {
        window.set_capture_error(reason.clone().into());
    }
    window.set_capture_notice(status.notice.clone().unwrap_or_default().into());
}

fn describe_state(status: &EngineStatus) -> String {
    match &status.state {
        BufferState::Stopped => "Stopped".to_owned(),
        BufferState::Running => "Recording into memory".to_owned(),
        BufferState::Failed(_) => "Failed".to_owned(),
    }
}

fn describe_buffer(status: &EngineStatus) -> String {
    let stats = status.stats;
    let available = stats
        .duration
        .as_secs_f64()
        .min(status.replay_length.as_secs_f64());
    let resolution = status
        .stream
        .as_ref()
        .map(|s| {
            format!(
                ", {}x{} @ {} fps",
                s.width,
                s.height,
                s.fps_num / s.fps_den.max(1)
            )
        })
        .unwrap_or_default();
    format!(
        "{available:.0} of {} s buffered, {:.0} MB in memory{resolution}",
        status.replay_length.as_secs(),
        stats.bytes as f64 / (1024.0 * 1024.0)
    )
}
