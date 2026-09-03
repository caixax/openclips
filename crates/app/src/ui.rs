use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use openclips_capture::platform::Platform;
use openclips_core::APP_VERSION;
use openclips_core::config::{AppPaths, Config};
use slint::{CloseRequestResponse, ComponentHandle, Weak};
use tracing::{error, info, warn};

use crate::engine::{BufferState, Engine, EngineStatus, RecordingState, file_name_of};
use crate::error::AppError;
use crate::hotkeys::{self, HotkeyAction, Hotkeys, PressedModifiers};
use crate::settings;
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
/// Displays are re-enumerated every this many status ticks.
const MONITOR_REFRESH_TICKS: u32 = 4;

pub struct Context {
    pub paths: AppPaths,
    pub config: Config,
    pub engine: Option<Engine>,
    pub startup_warning: String,
}

pub struct App {
    pub window: MainWindow,
    pub tray: TrayIcon,
    pub start_minimized: bool,
    _status_timer: slint::Timer,
}

/// State shared between UI callbacks on the UI thread.
struct Shared {
    paths: AppPaths,
    config: RefCell<Config>,
    engine: RefCell<Option<Engine>>,
    hotkeys: RefCell<Option<Hotkeys>>,
    ticks: RefCell<u32>,
}

type SharedRef = Rc<Shared>;

impl Shared {
    fn default_clips_dir(&self) -> PathBuf {
        self.paths.default_clips_dir.clone()
    }

    fn clips_dir(&self) -> PathBuf {
        self.config.borrow().clips_dir(&self.paths)
    }
}

pub fn build(ctx: Context) -> Result<App, AppError> {
    let window = MainWindow::new()?;
    let tray = TrayIcon::new()?;
    let start_minimized = ctx.config.general.start_minimized;
    let shared: SharedRef = Rc::new(Shared {
        paths: ctx.paths,
        config: RefCell::new(ctx.config),
        engine: RefCell::new(ctx.engine),
        hotkeys: RefCell::new(None),
        ticks: RefCell::new(0),
    });

    window.set_info(AppInfo {
        version: APP_VERSION.into(),
        platform: Platform::current().name().into(),
        config_path: shared.paths.config_file().display().to_string().into(),
        clips_dir: shared.clips_dir().display().to_string().into(),
        log_dir: shared.paths.log_dir.display().to_string().into(),
    });
    window.set_startup_warning(ctx.startup_warning.into());
    update_hotkey_hint(&window, &shared.config.borrow());

    wire_folders(&window, &shared);
    wire_window_lifecycle(&window, &tray);
    wire_actions(&window, &tray, &shared);
    wire_settings(&window, &shared);
    install_hotkeys(&window, &shared);
    let status_timer = start_status_timer(&window, &tray, &shared);

    if shared.config.borrow().replay.start_on_launch {
        run_engine(&shared, &window, |e| e.start_buffer());
    }
    refresh_status(&window, &tray, &shared);

    Ok(App {
        window,
        tray,
        start_minimized,
        _status_timer: status_timer,
    })
}

fn update_hotkey_hint(window: &MainWindow, config: &Config) {
    window.set_hotkey_hint(
        format!(
            "Press {} to save the last {} seconds, {} to start or stop the buffer, {} to start or stop a recording.",
            config.hotkeys.save_replay,
            config.replay.length_seconds,
            config.hotkeys.toggle_replay_buffer,
            config.hotkeys.toggle_recording
        )
        .into(),
    );
}

fn wire_folders(window: &MainWindow, shared: &SharedRef) {
    let config_dir = shared.paths.config_dir.clone();
    window.on_open_config_dir(move || shell::open_folder(&config_dir));
    let s = shared.clone();
    window.on_open_clips_dir(move || shell::open_folder(&s.clips_dir()));
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

fn wire_actions(window: &MainWindow, tray: &TrayIcon, shared: &SharedRef) {
    let (s, w) = (shared.clone(), window.as_weak());
    window.on_toggle_buffer(move || toggle_buffer(&s, &w));
    let (s, w) = (shared.clone(), window.as_weak());
    tray.on_toggle_buffer(move || toggle_buffer(&s, &w));

    let (s, w) = (shared.clone(), window.as_weak());
    window.on_save_clip(move || save_clip(&s, &w));
    let (s, w) = (shared.clone(), window.as_weak());
    tray.on_save_clip(move || save_clip(&s, &w));

    let (s, w) = (shared.clone(), window.as_weak());
    window.on_toggle_recording(move || toggle_recording(&s, &w));
    let (s, w) = (shared.clone(), window.as_weak());
    tray.on_toggle_recording(move || toggle_recording(&s, &w));

    let s = shared.clone();
    window.on_recording_finished(move || {
        if let Some(engine) = s.engine.borrow_mut().as_mut() {
            engine.recording_finished();
        }
    });
}

fn install_hotkeys(window: &MainWindow, shared: &SharedRef) {
    let (s, w) = (shared.clone(), window.as_weak());
    hotkeys::install_dispatch(move |action| match action {
        HotkeyAction::SaveReplay => save_clip(&s, &w),
        HotkeyAction::ToggleReplayBuffer => toggle_buffer(&s, &w),
        HotkeyAction::ToggleRecording => toggle_recording(&s, &w),
    });
    if let Some(problem) = register_hotkeys(shared) {
        warn!("{problem}");
        window.set_startup_warning(problem.into());
    }
}

/// (Re)registers the configured hotkeys. Returns a user facing problem
/// description when any binding could not be installed.
fn register_hotkeys(shared: &SharedRef) -> Option<String> {
    *shared.hotkeys.borrow_mut() = None;
    match hotkeys::register(&shared.config.borrow().hotkeys) {
        Ok(hotkeys) => {
            let problem = (!hotkeys.rejected.is_empty()).then(|| {
                hotkeys
                    .rejected
                    .iter()
                    .map(|(action, reason)| format!("{action:?}: {reason}"))
                    .collect::<Vec<_>>()
                    .join(". ")
            });
            *shared.hotkeys.borrow_mut() = Some(hotkeys);
            problem
        }
        Err(err) => Some(format!("Global hotkeys are unavailable: {err}")),
    }
}

fn wire_settings(window: &MainWindow, shared: &SharedRef) {
    let state = window.global::<SettingsState>();
    let monitors = shared
        .engine
        .borrow()
        .as_ref()
        .map(|e| e.monitors().to_vec())
        .unwrap_or_default();
    settings::populate(
        &state,
        &shared.config.borrow(),
        &monitors,
        &shared.default_clips_dir(),
    );

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_save(move || {
        if let Some(window) = w.upgrade() {
            save_settings(&s, &window);
        }
    });

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_revert(move || {
        if let Some(window) = w.upgrade() {
            let state = window.global::<SettingsState>();
            let monitors = current_monitors(&s);
            settings::populate(
                &state,
                &s.config.borrow(),
                &monitors,
                &s.default_clips_dir(),
            );
            state.set_message("".into());
        }
    });

    let w = window.as_weak();
    state.on_browse_clips_dir(move || {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<SettingsState>();
        let current = PathBuf::from(state.get_clips_dir().as_str());
        let mut dialog = rfd::FileDialog::new().set_title("Choose the clips folder");
        if current.is_dir() {
            dialog = dialog.set_directory(&current);
        }
        if let Some(dir) = dialog.pick_folder() {
            state.set_clips_dir(dir.display().to_string().into());
        }
    });

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_refresh_displays(move || {
        if let Some(window) = w.upgrade() {
            if let Some(engine) = s.engine.borrow_mut().as_mut() {
                engine.refresh_monitors();
            }
            let state = window.global::<SettingsState>();
            let monitors = current_monitors(&s);
            let selected = settings::selected_display(&state, &monitors);
            settings::set_monitors(&state, &monitors, &selected);
        }
    });

    let w = window.as_weak();
    state.on_key_captured(move |action, text, alt, control, shift, meta| {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<SettingsState>();
        let mods = PressedModifiers {
            alt,
            control,
            shift,
            meta,
        };
        if text.starts_with(char::from(slint::platform::Key::Escape)) && !alt && !control && !shift
        {
            state.set_listening_action(-1);
            return;
        }
        let Some(hotkey) = hotkeys::hotkey_from_press(&text, mods) else {
            return;
        };
        let binding = hotkey.to_string().into();
        match HotkeyAction::from_index(action) {
            Some(HotkeyAction::SaveReplay) => state.set_hotkey_save_replay(binding),
            Some(HotkeyAction::ToggleReplayBuffer) => state.set_hotkey_toggle_buffer(binding),
            Some(HotkeyAction::ToggleRecording) => state.set_hotkey_toggle_recording(binding),
            None => {}
        }
        state.set_listening_action(-1);
    });
}

fn current_monitors(shared: &SharedRef) -> Vec<openclips_core::capture::MonitorInfo> {
    shared
        .engine
        .borrow()
        .as_ref()
        .map(|e| e.monitors().to_vec())
        .unwrap_or_default()
}

fn save_settings(shared: &SharedRef, window: &MainWindow) {
    let state = window.global::<SettingsState>();
    let monitors = current_monitors(shared);
    let next = match settings::collect(
        &state,
        &shared.config.borrow(),
        &monitors,
        &shared.default_clips_dir(),
    ) {
        Ok(next) => next,
        Err(problem) => {
            state.set_message_is_error(true);
            state.set_message(problem.into());
            return;
        }
    };
    if let Err(err) = next.save(&shared.paths.config_file()) {
        state.set_message_is_error(true);
        state.set_message(format!("Could not save settings: {err}").into());
        return;
    }

    let mut rebind = shared.config.borrow().hotkeys_changed(&next);
    *shared.config.borrow_mut() = next.clone();
    let mut problems = Vec::new();
    if let Some(engine) = shared.engine.borrow_mut().as_mut() {
        match engine.apply_config(next.clone()) {
            Ok(needs_rebind) => rebind |= needs_rebind,
            Err(err) => problems.push(format!("Capture could not be restarted: {err}")),
        }
    }
    if rebind && let Some(problem) = register_hotkeys(shared) {
        problems.push(problem);
    }

    update_hotkey_hint(window, &next);
    let mut info = window.get_info();
    info.clips_dir = shared.clips_dir().display().to_string().into();
    window.set_info(info);
    settings::populate(&state, &next, &monitors, &shared.default_clips_dir());

    if problems.is_empty() {
        state.set_message_is_error(false);
        state.set_message("Settings saved.".into());
    } else {
        state.set_message_is_error(true);
        state.set_message(format!("Settings saved. {}", problems.join(" ")).into());
    }
    info!("settings saved");
}

fn start_status_timer(window: &MainWindow, tray: &TrayIcon, shared: &SharedRef) -> slint::Timer {
    let timer = slint::Timer::default();
    let (s, w, t) = (shared.clone(), window.as_weak(), tray.as_weak());
    timer.start(slint::TimerMode::Repeated, STATUS_REFRESH, move || {
        let (Some(window), Some(tray)) = (w.upgrade(), t.upgrade()) else {
            return;
        };
        let tick = {
            let mut ticks = s.ticks.borrow_mut();
            *ticks = ticks.wrapping_add(1);
            *ticks
        };
        if tick.is_multiple_of(MONITOR_REFRESH_TICKS) {
            poll_monitors(&s, &window);
        }
        refresh_status(&window, &tray, &s);
    });
    timer
}

fn poll_monitors(shared: &SharedRef, window: &MainWindow) {
    let changed = shared
        .engine
        .borrow_mut()
        .as_mut()
        .is_some_and(|e| e.refresh_monitors());
    if changed {
        let state = window.global::<SettingsState>();
        let monitors = current_monitors(shared);
        let selected = settings::selected_display(&state, &monitors);
        settings::set_monitors(&state, &monitors, &selected);
    }
}

fn run_engine(
    shared: &SharedRef,
    window: &MainWindow,
    action: impl FnOnce(&mut Engine) -> Result<(), AppError>,
) {
    let mut slot = shared.engine.borrow_mut();
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

fn toggle_buffer(shared: &SharedRef, window: &Weak<MainWindow>) {
    if let Some(window) = window.upgrade() {
        run_engine(shared, &window, |e| e.toggle_buffer());
    }
}

fn toggle_recording(shared: &SharedRef, window: &Weak<MainWindow>) {
    let Some(strong) = window.upgrade() else {
        return;
    };
    let weak = window.clone();
    run_engine(shared, &strong, move |e| {
        e.toggle_recording(Box::new(move |result| {
            let _ = weak.upgrade_in_event_loop(move |window| {
                window.invoke_recording_finished();
                match result {
                    Ok(clip) => {
                        window.set_last_recording(clip.path.display().to_string().into());
                        window.set_recording_message(
                            format!(
                                "Saved {} ({}, {:.1} MB)",
                                file_name_of(&clip.path),
                                format_duration(clip.duration),
                                clip.bytes as f64 / (1024.0 * 1024.0)
                            )
                            .into(),
                        );
                    }
                    Err(err) => {
                        window.set_recording_message(format!("Recording failed: {err}").into())
                    }
                }
            });
        }))
    });
}

fn save_clip(shared: &SharedRef, window: &Weak<MainWindow>) {
    let Some(strong) = window.upgrade() else {
        return;
    };
    let slot = shared.engine.borrow();
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
                        file_name_of(&clip.path),
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

fn refresh_status(window: &MainWindow, tray: &TrayIcon, shared: &SharedRef) {
    let mut slot = shared.engine.borrow_mut();
    let Some(engine) = slot.as_mut() else {
        window.set_buffer_active(false);
        window.set_buffer_status("Unavailable".into());
        tray.set_buffer_active(false);
        tray.set_recording_active(false);
        return;
    };
    let status = engine.status();
    let buffering = status.buffer == BufferState::Running;
    window.set_buffer_active(buffering);
    tray.set_buffer_active(buffering);
    window.set_buffer_status(describe_buffer_state(&status).into());
    window.set_buffer_detail(describe_buffer(&status).into());
    window.set_encoder_name(format!("{} ({})", status.encoder.kind.label(), status.backend).into());
    if let BufferState::Failed(reason) = &status.buffer {
        window.set_capture_error(reason.clone().into());
    }
    window.set_capture_notice(status.notice.clone().unwrap_or_default().into());

    let recording = matches!(
        status.recording,
        RecordingState::Starting | RecordingState::Active { .. }
    );
    window.set_recording_active(recording);
    tray.set_recording_active(recording);
    window.set_recording_status(describe_recording(&status.recording).into());
    if let RecordingState::Failed(reason) = &status.recording {
        window.set_recording_message(format!("Recording failed: {reason}").into());
    }
}

fn describe_buffer_state(status: &EngineStatus) -> String {
    match &status.buffer {
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

fn describe_recording(state: &RecordingState) -> String {
    match state {
        RecordingState::Idle => "Not recording".to_owned(),
        RecordingState::Starting => "Starting...".to_owned(),
        RecordingState::Active { path, duration } => {
            format!(
                "Recording {} ({})",
                file_name_of(path),
                format_duration(*duration)
            )
        }
        RecordingState::Finishing => "Finishing file...".to_owned(),
        RecordingState::Failed(_) => "Failed".to_owned(),
    }
}

fn format_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}
