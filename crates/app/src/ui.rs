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
use crate::games::GameService;
use crate::hotkeys::{self, HotkeyAction, Hotkeys, PressedModifiers};
use crate::library::{LibraryService, format_duration};
use crate::player::PlayerController;
use crate::settings;
use crate::shell;
use openclips_capture::TrimJob;
use openclips_core::config::GameProfile;
use openclips_core::games::auto_capture;
use openclips_core::trim::{TrimMode, TrimRange, trimmed_path};
use slint::{Image, Model, ModelRc, SharedString, VecModel};

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
    library: RefCell<Option<LibraryService>>,
    player: RefCell<Option<PlayerController>>,
    games: RefCell<Option<GameService>>,
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
        library: RefCell::new(None),
        player: RefCell::new(None),
        games: RefCell::new(None),
        ticks: RefCell::new(0),
    });
    init_library_and_player(&window, &shared);
    init_games(&shared);

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
    wire_library(&window, &shared);
    wire_player(&window, &shared);
    wire_games(&window, &shared);
    install_hotkeys(&window, &shared);
    let status_timer = start_status_timer(&window, &tray, &shared);

    let start_now = {
        let config = shared.config.borrow();
        config.replay.start_on_launch
            && config.games.scope == openclips_core::config::CaptureScope::Global
    };
    if start_now {
        run_engine(&shared, &window, |e| e.start_buffer());
    }
    poll_games(&shared, &window);
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
    let audio_devices = current_audio_devices(shared);
    settings::populate(
        &state,
        &shared.config.borrow(),
        &monitors,
        &audio_devices,
        &shared.default_clips_dir(),
    );
    refresh_game_rows(window, shared);

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
            let audio_devices = current_audio_devices(&s);
            settings::populate(
                &state,
                &s.config.borrow(),
                &monitors,
                &audio_devices,
                &s.default_clips_dir(),
            );
            refresh_game_rows(&window, &s);
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

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_refresh_audio_devices(move || {
        if let Some(window) = w.upgrade() {
            let state = window.global::<SettingsState>();
            let devices = current_audio_devices(&s);
            settings::refresh_audio_sources(&state, &s.config.borrow(), &devices);
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

fn current_audio_devices(shared: &SharedRef) -> Vec<openclips_core::capture::AudioDeviceInfo> {
    shared
        .engine
        .borrow()
        .as_ref()
        .map(|e| e.list_audio_devices())
        .unwrap_or_default()
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
    let mut problems = Vec::new();

    if shared.config.borrow().general.launch_on_startup != next.general.launch_on_startup
        && let Err(err) = crate::startup::apply(next.general.launch_on_startup)
    {
        problems.push(err);
    }
    let mut rebind = shared.config.borrow().hotkeys_changed(&next);
    *shared.config.borrow_mut() = next.clone();
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
    if let Some(library) = shared.library.borrow_mut().as_mut() {
        library.set_dirs(&shared.paths, &next);
    }
    refresh_library_ui(window, shared);
    let mut info = window.get_info();
    info.clips_dir = shared.clips_dir().display().to_string().into();
    window.set_info(info);
    let audio_devices = current_audio_devices(shared);
    settings::populate(
        &state,
        &next,
        &monitors,
        &audio_devices,
        &shared.default_clips_dir(),
    );
    refresh_game_rows(window, shared);

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
            poll_games(&s, &window);
        }
        refresh_status(&window, &tray, &s);
        let changed = s.library.borrow_mut().as_mut().is_some_and(|l| l.poll());
        if changed {
            refresh_library_ui(&window, &s);
        }
        if let Some(player) = s.player.borrow_mut().as_mut() {
            player.tick(&window.global::<PlayerState>());
        }
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
                        window.invoke_library_changed();
                        if let Some(game) = &clip.game {
                            window.invoke_clip_saved(
                                clip.path.display().to_string().into(),
                                game.clone().into(),
                            );
                        }
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
                window.invoke_library_changed();
                if let Some(game) = &clip.game {
                    window.invoke_clip_saved(
                        clip.path.display().to_string().into(),
                        game.clone().into(),
                    );
                }
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
    let mut notice = status.notice.clone().unwrap_or_default();
    if status.blank {
        if !notice.is_empty() {
            notice.push(' ');
        }
        notice.push_str(
            "The capture looks black. If the game runs in exclusive fullscreen, switch it to borderless windowed, or choose Windows Graphics Capture under Settings.",
        );
    }
    window.set_capture_notice(notice.into());

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
    let audio = match status.audio_tracks {
        0 => String::new(),
        1 => ", 1 audio track".to_owned(),
        n => format!(", {n} audio tracks"),
    };
    format!(
        "{available:.0} of {} s buffered, {:.0} MB in memory{resolution}{audio}",
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

fn init_library_and_player(window: &MainWindow, shared: &SharedRef) {
    let engine = shared.engine.borrow();
    let Some(engine) = engine.as_ref() else {
        window.global::<LibraryState>().set_message(
            "The clip library needs the media framework, which is unavailable.".into(),
        );
        return;
    };
    let library = LibraryService::new(&shared.paths, &shared.config.borrow(), engine.media_tools());
    *shared.library.borrow_mut() = Some(library);
    match PlayerController::new(window, |sink| engine.create_player(sink)) {
        Ok(player) => *shared.player.borrow_mut() = Some(player),
        Err(err) => {
            warn!("player unavailable: {err}");
            window
                .global::<PlayerState>()
                .set_message(format!("Playback is unavailable: {err}").into());
        }
    }
}

fn wire_library(window: &MainWindow, shared: &SharedRef) {
    let state = window.global::<LibraryState>();
    refresh_library_ui(window, shared);

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_refresh(move || {
        if let Some(window) = w.upgrade() {
            if let Some(library) = s.library.borrow_mut().as_mut() {
                library.refresh();
            }
            refresh_library_ui(&window, &s);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_filter_changed(move || {
        if let Some(window) = w.upgrade() {
            refresh_library_ui(&window, &s);
        }
    });
    let s = shared.clone();
    state.on_open_folder(move || shell::open_folder(&s.clips_dir()));
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_open_clip(move |id| {
        if let Some(window) = w.upgrade() {
            open_clip(&window, &s, &id);
        }
    });

    let (s, w) = (shared.clone(), window.as_weak());
    window.on_library_changed(move || {
        if let Some(window) = w.upgrade() {
            if let Some(library) = s.library.borrow_mut().as_mut() {
                library.refresh();
            }
            refresh_library_ui(&window, &s);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    window.on_navigated(move |page| {
        if page != NavPage::Player
            && let Some(window) = w.upgrade()
            && let Some(player) = s.player.borrow_mut().as_mut()
        {
            player.stop(&window.global::<PlayerState>());
        }
    });
}

fn refresh_library_ui(window: &MainWindow, shared: &SharedRef) {
    let state = window.global::<LibraryState>();
    let library = shared.library.borrow();
    let Some(library) = library.as_ref() else {
        return;
    };
    let games = library.games();
    let mut names: Vec<SharedString> = vec!["All games".into()];
    names.extend(games.iter().map(|g| SharedString::from(g.as_str())));
    let selected = state.get_game_index().max(0) as usize;
    let filter = (selected > 0)
        .then(|| games.get(selected - 1).cloned())
        .flatten();
    if names.len() != state.get_games().row_count() {
        state.set_games(ModelRc::new(VecModel::from(names)));
        state.set_game_index(
            filter
                .as_ref()
                .and_then(|f| games.iter().position(|g| g == f))
                .map(|i| i as i32 + 1)
                .unwrap_or(0),
        );
    }
    let search = state.get_search();
    let cards: Vec<ClipCard> = library
        .cards(filter.as_deref(), &search)
        .into_iter()
        .map(|c| {
            let thumbnail = c
                .thumbnail
                .as_ref()
                .and_then(|p| Image::load_from_path(p).ok());
            let icon = shared
                .games
                .borrow()
                .as_ref()
                .and_then(|g| g.icon_for_name(&c.game, &shared.config.borrow().games))
                .and_then(|p| Image::load_from_path(&p).ok());
            ClipCard {
                id: c.id.into(),
                title: c.title.into(),
                game: c.game.into(),
                date: c.date.into(),
                duration: c.duration.into(),
                has_thumbnail: thumbnail.is_some(),
                thumbnail: thumbnail.unwrap_or_default(),
                has_icon: icon.is_some(),
                icon: icon.unwrap_or_default(),
            }
        })
        .collect();
    let total = cards.len();
    state.set_clips(ModelRc::new(VecModel::from(cards)));
    state.set_summary(
        match total {
            0 => String::new(),
            1 => "1 clip".to_owned(),
            n => format!("{n} clips"),
        }
        .into(),
    );
}

fn open_clip(window: &MainWindow, shared: &SharedRef, id: &str) {
    let record = shared
        .library
        .borrow()
        .as_ref()
        .and_then(|l| l.record(id).cloned());
    let Some(record) = record else {
        return;
    };
    let state = window.global::<PlayerState>();
    state.set_title(record.title.clone().into());
    state.set_path(record.path.display().to_string().into());
    state.set_details(
        format!(
            "{}  |  {}x{}  |  {:.1} MB  |  {}",
            record
                .game
                .clone()
                .unwrap_or_else(|| record.kind.label().to_owned()),
            record.width,
            record.height,
            record.bytes as f64 / (1024.0 * 1024.0),
            record.path.display()
        )
        .into(),
    );
    state.set_renaming(false);
    state.set_confirm_delete(false);
    state.set_message("".into());
    state.set_editing(false);
    state.set_trim_busy(false);
    state.set_trim_message("".into());
    state.set_trim_in(0.0);
    state.set_trim_out(record.duration().as_secs_f32());
    update_trim_texts(&state);
    window.set_page(NavPage::Player);
    if let Some(player) = shared.player.borrow_mut().as_mut() {
        player.open(id, &record.path, &state);
    } else {
        state.set_message("Playback is unavailable on this system.".into());
    }
}

fn wire_player(window: &MainWindow, shared: &SharedRef) {
    let state = window.global::<PlayerState>();
    wire_editor(window, shared);

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_toggle_play(move || {
        if let Some(window) = w.upgrade()
            && let Some(player) = s.player.borrow_mut().as_mut()
        {
            player.toggle(&window.global::<PlayerState>());
        }
    });
    let s = shared.clone();
    state.on_seek(move |seconds| {
        if let Some(player) = s.player.borrow_mut().as_mut() {
            player.seek(seconds);
        }
    });
    let s = shared.clone();
    state.on_volume_changed(move |percent| {
        if let Some(player) = s.player.borrow_mut().as_mut() {
            player.set_volume(percent);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_back(move || {
        if let Some(window) = w.upgrade() {
            if let Some(player) = s.player.borrow_mut().as_mut() {
                player.stop(&window.global::<PlayerState>());
            }
            window.set_page(NavPage::Clips);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_reveal(move || {
        if let Some(window) = w.upgrade() {
            let path = PathBuf::from(window.global::<PlayerState>().get_path().as_str());
            shell::reveal_file(&path);
        }
        let _ = &s;
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_rename(move || {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<PlayerState>();
        let title = state.get_rename_text().trim().to_owned();
        let id = s
            .player
            .borrow()
            .as_ref()
            .and_then(|p| p.current().map(str::to_owned));
        let Some(id) = id else {
            return;
        };
        let result = s
            .library
            .borrow_mut()
            .as_mut()
            .map(|l| l.rename(&id, &title))
            .unwrap_or_else(|| Err("Library unavailable.".to_owned()));
        match result {
            Ok(()) => {
                state.set_title(title.into());
                state.set_renaming(false);
                let record = s
                    .library
                    .borrow()
                    .as_ref()
                    .and_then(|l| l.record(&id).cloned());
                if let Some(record) = record {
                    state.set_path(record.path.display().to_string().into());
                }
                refresh_library_ui(&window, &s);
            }
            Err(message) => state.set_message(message.into()),
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_delete(move || {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<PlayerState>();
        let id = s
            .player
            .borrow()
            .as_ref()
            .and_then(|p| p.current().map(str::to_owned));
        let Some(id) = id else {
            return;
        };
        if let Some(player) = s.player.borrow_mut().as_mut() {
            player.stop(&state);
        }
        let result = s
            .library
            .borrow_mut()
            .as_mut()
            .map(|l| l.delete(&id))
            .unwrap_or_else(|| Err("Library unavailable.".to_owned()));
        state.set_confirm_delete(false);
        match result {
            Ok(()) => {
                refresh_library_ui(&window, &s);
                window.set_page(NavPage::Clips);
            }
            Err(message) => state.set_message(message.into()),
        }
    });
}

fn format_precise(seconds: f32) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let (m, s, ms) = (
        total_ms / 60_000,
        (total_ms % 60_000) / 1000,
        total_ms % 1000,
    );
    format!("{m}:{s:02}.{ms:03}")
}

fn update_trim_texts(state: &PlayerState<'_>) {
    let (start, end) = (state.get_trim_in(), state.get_trim_out());
    state.set_trim_in_text(format_precise(start).into());
    state.set_trim_out_text(format_precise(end).into());
    let length = (end - start).max(0.0);
    state.set_trim_summary(format!("Selection: {}", format_precise(length)).into());
}

fn current_clip_id(shared: &SharedRef) -> Option<String> {
    shared
        .player
        .borrow()
        .as_ref()
        .and_then(|p| p.current().map(str::to_owned))
}

fn wire_editor(window: &MainWindow, shared: &SharedRef) {
    let state = window.global::<PlayerState>();

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_set_in(move || {
        if let Some(window) = w.upgrade() {
            let state = window.global::<PlayerState>();
            let position = s
                .player
                .borrow()
                .as_ref()
                .and_then(|p| p.position())
                .map(|p| p.as_secs_f32())
                .unwrap_or(state.get_position());
            state.set_trim_in(position.min(state.get_trim_out()));
            update_trim_texts(&state);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_set_out(move || {
        if let Some(window) = w.upgrade() {
            let state = window.global::<PlayerState>();
            let position = s
                .player
                .borrow()
                .as_ref()
                .and_then(|p| p.position())
                .map(|p| p.as_secs_f32())
                .unwrap_or(state.get_position());
            state.set_trim_out(position.max(state.get_trim_in()));
            update_trim_texts(&state);
        }
    });
    let w = window.as_weak();
    state.on_set_in_at(move |seconds| {
        if let Some(window) = w.upgrade() {
            let state = window.global::<PlayerState>();
            state.set_trim_in(seconds.clamp(0.0, state.get_trim_out()));
            update_trim_texts(&state);
        }
    });
    let w = window.as_weak();
    state.on_set_out_at(move |seconds| {
        if let Some(window) = w.upgrade() {
            let state = window.global::<PlayerState>();
            state.set_trim_out(seconds.clamp(state.get_trim_in(), state.get_duration().max(0.0)));
            update_trim_texts(&state);
        }
    });
    let w = window.as_weak();
    state.on_trim_changed(move || {
        if let Some(window) = w.upgrade() {
            update_trim_texts(&window.global::<PlayerState>());
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_preview_selection(move || {
        if let Some(window) = w.upgrade()
            && let Some(player) = s.player.borrow_mut().as_mut()
        {
            let state = window.global::<PlayerState>();
            player.preview(
                Duration::from_secs_f32(state.get_trim_in().max(0.0)),
                Duration::from_secs_f32(state.get_trim_out().max(0.0)),
                &state,
            );
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_save_trim(move || {
        if let Some(window) = w.upgrade() {
            save_trim(&window, &s);
        }
    });
}

fn save_trim(window: &MainWindow, shared: &SharedRef) {
    let state = window.global::<PlayerState>();
    let Some(id) = current_clip_id(shared) else {
        state.set_trim_message("Open a clip first.".into());
        return;
    };
    let record = shared
        .library
        .borrow()
        .as_ref()
        .and_then(|l| l.record(&id).cloned());
    let Some(record) = record else {
        return;
    };
    let duration = if record.duration().is_zero() {
        Duration::from_secs_f32(state.get_duration().max(0.0))
    } else {
        record.duration()
    };
    let range = match TrimRange::new(
        Duration::from_secs_f32(state.get_trim_in().max(0.0)),
        Duration::from_secs_f32(state.get_trim_out().max(0.0)),
        duration,
    ) {
        Ok(range) => range,
        Err(err) => {
            state.set_trim_message(err.to_string().into());
            return;
        }
    };
    if range.is_whole(duration) {
        state.set_trim_message("The selection covers the whole clip; nothing to trim.".into());
        return;
    }
    let mode = if state.get_trim_mode_index() == 1 {
        TrimMode::FrameAccurate
    } else {
        TrimMode::StreamCopy
    };
    let overwrite = state.get_trim_overwrite();
    let output = if overwrite {
        record.path.with_extension("mp4.trimmed")
    } else {
        trimmed_path(&record.path, &record.title)
    };
    let (video_bitrate, audio_bitrate) = {
        let config = shared.config.borrow();
        (config.capture.bitrate_kbps, config.audio.bitrate_kbps)
    };
    let tools = shared.engine.borrow().as_ref().map(|e| e.media_tools());
    let Some(tools) = tools else {
        state.set_trim_message("Trimming is unavailable on this system.".into());
        return;
    };

    if overwrite && let Some(player) = shared.player.borrow_mut().as_mut() {
        player.stop(&state);
    }
    state.set_trim_busy(true);
    state.set_trim_message(
        match mode {
            TrimMode::StreamCopy => "Cutting...".to_owned(),
            TrimMode::FrameAccurate => {
                "Re-encoding the selection, this takes a moment...".to_owned()
            }
        }
        .into(),
    );
    let job = TrimJob {
        input: record.path.clone(),
        output: output.clone(),
        range,
        mode,
        video_bitrate_kbps: video_bitrate,
        audio_bitrate_kbps: audio_bitrate,
    };
    let original = record.path.clone();
    let weak = window.as_weak();
    let spawned = std::thread::Builder::new()
        .name("trim".to_owned())
        .spawn(move || {
            let result = tools
                .trim(&job)
                .map_err(|e| e.to_string())
                .and_then(|clip| {
                    if overwrite {
                        std::fs::rename(&clip.path, &original)
                            .map_err(|e| format!("could not replace the original: {e}"))?;
                    }
                    Ok(clip)
                });
            let _ = weak.upgrade_in_event_loop(move |window| {
                let state = window.global::<PlayerState>();
                state.set_trim_busy(false);
                match result {
                    Ok(clip) => {
                        let name = if overwrite {
                            "the original file".to_owned()
                        } else {
                            file_name_of(&clip.path)
                        };
                        state.set_trim_message(
                            format!(
                                "Saved {} ({}, {:.1} MB)",
                                name,
                                format_duration(clip.duration),
                                clip.bytes as f64 / (1024.0 * 1024.0)
                            )
                            .into(),
                        );
                        window.invoke_library_changed();
                    }
                    Err(err) => state.set_trim_message(format!("Trim failed: {err}").into()),
                }
            });
        });
    if let Err(err) = spawned {
        state.set_trim_busy(false);
        state.set_trim_message(format!("Could not start the trim: {err}").into());
    }
}

fn init_games(shared: &SharedRef) {
    let engine = shared.engine.borrow();
    let Some(engine) = engine.as_ref() else {
        return;
    };
    let service = GameService::new(
        &shared.paths,
        engine.process_watcher(),
        engine.icon_extractor(),
    );
    *shared.games.borrow_mut() = Some(service);
}

/// Polls the process list and pushes the result into the engine and the UI.
fn poll_games(shared: &SharedRef, window: &MainWindow) {
    let (active, auto, running_text) = {
        let mut games = shared.games.borrow_mut();
        let Some(games) = games.as_mut() else {
            return;
        };
        let config = shared.config.borrow();
        games.refresh(&config.games);
        let active = games.active().cloned();
        let auto = auto_capture(config.games.scope, active.as_ref());
        let names: Vec<String> = games.detected().iter().map(|g| g.name.clone()).collect();
        let text = if names.is_empty() {
            "Running now: no known game".to_owned()
        } else {
            format!("Running now: {}", names.join(", "))
        };
        (active, auto, text)
    };
    window.set_detected_game(
        active
            .as_ref()
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "No known game running".to_owned())
            .into(),
    );
    let state = window.global::<SettingsState>();
    if state.get_running_games() != running_text {
        state.set_running_games(running_text.into());
        refresh_running_known(window, shared);
    }
    run_engine(shared, window, |e| e.set_game_state(active, auto));
}

fn refresh_running_known(window: &MainWindow, shared: &SharedRef) {
    let state = window.global::<SettingsState>();
    let games = shared.games.borrow();
    let Some(games) = games.as_ref() else {
        return;
    };
    let configured: Vec<String> = state
        .get_game_profiles()
        .iter()
        .map(|r| r.exe.to_lowercase())
        .collect();
    let names: Vec<SharedString> = games
        .detected()
        .iter()
        .filter(|g| !configured.contains(&g.exe))
        .map(|g| SharedString::from(format!("{} ({})", g.name, g.exe)))
        .collect();
    state.set_running_known(ModelRc::new(VecModel::from(names)));
    state.set_running_known_index(0);
}

fn refresh_game_rows(window: &MainWindow, shared: &SharedRef) {
    let state = window.global::<SettingsState>();
    let monitors = current_monitors(shared);
    let profiles = shared.config.borrow().games.profiles.clone();
    let games = shared.games.borrow();
    settings::set_game_profiles(&state, &profiles, &monitors, |exe| {
        games.as_ref().and_then(|g| g.cached_icon(exe))
    });
    drop(games);
    refresh_running_known(window, shared);
}

/// Replaces the rows with an edited list, keeping icons.
fn set_game_rows(window: &MainWindow, shared: &SharedRef, profiles: &[GameProfile]) {
    let state = window.global::<SettingsState>();
    let monitors = current_monitors(shared);
    let games = shared.games.borrow();
    settings::set_game_profiles(&state, profiles, &monitors, |exe| {
        games.as_ref().and_then(|g| g.cached_icon(exe))
    });
    drop(games);
    refresh_running_known(window, shared);
}

fn wire_games(window: &MainWindow, shared: &SharedRef) {
    let state = window.global::<SettingsState>();

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_add_running_game(move || {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<SettingsState>();
        let index = state.get_running_known_index().max(0) as usize;
        let monitors = current_monitors(&s);
        let mut profiles = settings::collect_game_profiles(&state, &monitors);
        let configured: Vec<String> = profiles.iter().map(|p| p.exe.clone()).collect();
        let candidate = s.games.borrow().as_ref().and_then(|g| {
            g.detected()
                .iter()
                .filter(|d| !configured.contains(&d.exe))
                .nth(index)
                .cloned()
        });
        let Some(game) = candidate else {
            return;
        };
        profiles.push(GameProfile {
            exe: game.exe.clone(),
            name: game.name.clone(),
            ..GameProfile::default()
        });
        set_game_rows(&window, &s, &profiles);
        state.set_games_message(format!("Added {}. Save to apply.", game.name).into());
    });

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_add_game(move || {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<SettingsState>();
        let exe = state.get_new_game_exe().trim().to_lowercase();
        if exe.is_empty() {
            state.set_games_message("Enter the executable name, for example game.exe.".into());
            return;
        }
        let exe = if exe.ends_with(".exe") {
            exe
        } else {
            format!("{exe}.exe")
        };
        let monitors = current_monitors(&s);
        let mut profiles = settings::collect_game_profiles(&state, &monitors);
        if profiles.iter().any(|p| p.exe == exe) {
            state.set_games_message(format!("{exe} is already in the list.").into());
            return;
        }
        let name = state.get_new_game_name().trim().to_owned();
        let name = if name.is_empty() {
            s.games
                .borrow()
                .as_ref()
                .and_then(|g| g.database().lookup(&exe).map(str::to_owned))
                .unwrap_or_default()
        } else {
            name
        };
        profiles.push(GameProfile {
            exe: exe.clone(),
            name,
            ..GameProfile::default()
        });
        set_game_rows(&window, &s, &profiles);
        state.set_new_game_exe("".into());
        state.set_new_game_name("".into());
        state.set_games_message(format!("Added {exe}. Save to apply.").into());
    });

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_remove_game(move |exe| {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<SettingsState>();
        let monitors = current_monitors(&s);
        let mut profiles = settings::collect_game_profiles(&state, &monitors);
        profiles.retain(|p| p.exe != exe.as_str());
        set_game_rows(&window, &s, &profiles);
        state.set_games_message("Removed. Save to apply.".into());
    });

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_suggest_steam_names(move || {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<SettingsState>();
        let monitors = current_monitors(&s);
        let profiles = settings::collect_game_profiles(&state, &monitors);
        let missing: Vec<String> = profiles
            .iter()
            .filter(|p| p.name.trim().is_empty())
            .map(|p| p.exe.clone())
            .collect();
        if missing.is_empty() {
            state.set_games_message("Every game in the list already has a name.".into());
            return;
        }
        state.set_steam_busy(true);
        state.set_games_message("Contacting Steam...".into());
        let cache = s.paths.cache_dir.join("steam_apps.json");
        let weak = window.as_weak();
        std::thread::spawn(move || {
            let result = crate::steam::app_names(&cache).map(|names| {
                missing
                    .iter()
                    .filter_map(|exe| {
                        crate::steam::suggest_name(exe, &names)
                            .map(|name| (exe.clone(), name.clone()))
                    })
                    .collect::<Vec<(String, String)>>()
            });
            let _ = weak.upgrade_in_event_loop(move |window| {
                let state = window.global::<SettingsState>();
                state.set_steam_busy(false);
                match result {
                    Ok(found) if found.is_empty() => {
                        state.set_games_message("Steam had no matching names.".into());
                    }
                    Ok(found) => {
                        let rows = state.get_game_profiles();
                        for i in 0..rows.row_count() {
                            if let Some(mut row) = rows.row_data(i)
                                && let Some((_, name)) =
                                    found.iter().find(|(exe, _)| *exe == row.exe.to_lowercase())
                            {
                                row.name = name.clone().into();
                                rows.set_row_data(i, row);
                            }
                        }
                        state.set_games_message(
                            format!("Filled {} name(s) from Steam. Save to apply.", found.len())
                                .into(),
                        );
                    }
                    Err(err) => state.set_games_message(err.into()),
                }
            });
        });
    });

    let (s, w) = (shared.clone(), window.as_weak());
    window.on_clip_saved(move |path, game| {
        if let Some(window) = w.upgrade() {
            if let Some(library) = s.library.borrow_mut().as_mut() {
                library.refresh();
                library.tag_game(std::path::Path::new(path.as_str()), &game);
            }
            refresh_library_ui(&window, &s);
        }
    });
}
