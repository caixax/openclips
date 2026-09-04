//! The UI layer. The window is created on demand and destroyed when it is
//! closed, so a copy living in the tray holds no Slint scene, renderer or
//! decoded thumbnails; capture, hotkeys, Discord presence, the library and
//! the sound keep running from `Shared`, which outlives every window.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use openclips_capture::platform::Platform;
use openclips_core::APP_VERSION;
use openclips_core::clip::ClipFile;
use openclips_core::config::{AppPaths, Config};
use slint::{CloseRequestResponse, ComponentHandle};
use tracing::{error, info, warn};

use crate::discord::{DiscordPresence, PresenceState};
use crate::engine::{BufferState, Engine, EngineStatus, RecordingState, file_name_of};
use crate::error::AppError;
use crate::games::GameService;
use crate::hotkeys::{self, HotkeyAction, Hotkeys, PressedModifiers};
use crate::library::{CardFilter, CardSort, LibraryService, format_duration, format_size};
use crate::player::PlayerController;
use crate::settings;
use crate::shell;
use crate::updater::{self, UpdateEvent};
use openclips_capture::TrimJob;
use openclips_core::config::GameProfile;
use openclips_core::config::{HotkeyActionKind, HotkeyBinding};
use openclips_core::games::auto_capture;
use openclips_core::library::ClipKind;
use openclips_core::library::ClipRecord;
use openclips_core::trim::{COMPRESS_PRESETS, TrimMode, TrimRange, edited_path};
use openclips_core::update::PendingUpdate;
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
/// The window is dropped a moment after the close request so the request
/// handler itself never runs inside a dying component.
const UNLOAD_DELAY: Duration = Duration::from_millis(50);
/// Quiet time after the last settings change before it is saved.
const AUTOSAVE_DELAY: Duration = Duration::from_millis(600);
/// How long the "saved" notice stays on the settings page.
const SAVED_NOTICE: Duration = Duration::from_millis(2500);

pub struct Context {
    pub paths: AppPaths,
    pub config: Config,
    pub engine: Option<Engine>,
    pub startup_warning: String,
    pub instance: crate::instance::Guard,
}

pub struct App {
    shared: SharedRef,
}

impl App {
    /// Creates the window if there is none and shows it.
    pub fn show_window(&self) -> Result<(), AppError> {
        show_window(&self.shared)
    }

    pub fn show_tray(&self) -> Result<(), AppError> {
        self.shared.tray.show()?;
        Ok(())
    }
}

/// Texts that outlive the window so a reopened window shows them again.
#[derive(Default)]
struct StatusTexts {
    save_status: String,
    last_clip: String,
    last_recording: String,
    recording_message: String,
}

/// State shared between UI callbacks on the UI thread. Everything here
/// survives closing the window.
struct Shared {
    paths: AppPaths,
    config: RefCell<Config>,
    engine: RefCell<Option<Engine>>,
    hotkeys: RefCell<Option<Hotkeys>>,
    library: RefCell<Option<LibraryService>>,
    player: RefCell<Option<PlayerController>>,
    games: RefCell<Option<GameService>>,
    ticks: RefCell<u32>,
    discord: DiscordPresence,
    pending_update: RefCell<Option<PendingUpdate>>,
    tray: TrayIcon,
    toast: crate::toast::Toast,
    instance: crate::instance::Guard,
    window: RefCell<Option<MainWindow>>,
    timer: RefCell<Option<slint::Timer>>,
    /// Pending settings save, restarted on every change.
    autosave: RefCell<Option<slint::Timer>>,
    /// Clears the "saved" notice again.
    message_timer: RefCell<Option<slint::Timer>>,
    startup_warning: RefCell<String>,
    update_banner: RefCell<Option<UpdateEvent>>,
    texts: RefCell<StatusTexts>,
    /// Detected game name and its icon file, for the top bar.
    game: RefCell<(String, Option<PathBuf>)>,
}

type SharedRef = Rc<Shared>;

impl Shared {
    fn default_clips_dir(&self) -> PathBuf {
        self.paths.default_clips_dir.clone()
    }

    fn clips_dir(&self) -> PathBuf {
        self.config.borrow().clips_dir(&self.paths)
    }

    /// Runs `f` against the window when one exists.
    fn with_window<R>(&self, f: impl FnOnce(&MainWindow) -> R) -> Option<R> {
        self.window.borrow().as_ref().map(f)
    }
}

thread_local! {
    /// Lets worker threads reach the UI state through the event loop.
    static SHARED: RefCell<Option<SharedRef>> = const { RefCell::new(None) };
}

/// Results that arrive from worker threads. They are handled on the UI
/// thread whether or not a window exists at that moment.
enum UiEvent {
    ClipSaved(Result<ClipFile, String>),
    RecordingDone(Result<ClipFile, String>),
    EditDone {
        result: Result<ClipFile, String>,
        overwrite: bool,
        original: PathBuf,
    },
    Update(UpdateEvent),
    Steam(Result<Vec<(String, String)>, String>),
}

fn post(event: UiEvent) {
    let queued = slint::invoke_from_event_loop(move || {
        let shared = SHARED.with(|slot| slot.borrow().clone());
        if let Some(shared) = shared {
            handle_event(&shared, event);
        }
    });
    if let Err(err) = queued {
        warn!("could not deliver a UI event: {err}");
    }
}

pub fn build(ctx: Context) -> Result<App, AppError> {
    let tray = TrayIcon::new()?;
    // The tray lives for the whole session; its menu is translated to the
    // language at startup (a language change takes effect on the next start).
    tray.global::<I18n>()
        .on_tr(|text| crate::i18n::tr(&text).into());
    let shared: SharedRef = Rc::new(Shared {
        paths: ctx.paths,
        config: RefCell::new(ctx.config),
        engine: RefCell::new(ctx.engine),
        hotkeys: RefCell::new(None),
        library: RefCell::new(None),
        player: RefCell::new(None),
        games: RefCell::new(None),
        ticks: RefCell::new(0),
        discord: DiscordPresence::start(),
        pending_update: RefCell::new(None),
        tray,
        toast: crate::toast::Toast::default(),
        instance: ctx.instance,
        window: RefCell::new(None),
        timer: RefCell::new(None),
        autosave: RefCell::new(None),
        message_timer: RefCell::new(None),
        startup_warning: RefCell::new(ctx.startup_warning),
        update_banner: RefCell::new(None),
        texts: RefCell::new(StatusTexts::default()),
        game: RefCell::new(("No game detected".to_owned(), None)),
    });
    SHARED.with(|slot| *slot.borrow_mut() = Some(shared.clone()));
    init_library(&shared);
    init_games(&shared);
    wire_tray(&shared);
    install_hotkeys(&shared);
    start_status_timer(&shared);

    let start_now = {
        let config = shared.config.borrow();
        config.replay.start_on_launch
            && config.games.scope == openclips_core::config::CaptureScope::Global
    };
    if start_now {
        run_engine(&shared, None, |e| e.start_buffer());
    }
    poll_games(&shared);
    refresh_status(&shared);

    let config = shared.config.borrow().updates.clone();
    updater::spawn_check(shared.paths.clone(), config, |event| {
        post(UiEvent::Update(event));
    });

    Ok(App { shared })
}

fn show_window(shared: &SharedRef) -> Result<(), AppError> {
    if shared.window.borrow().is_none() {
        let window = create_window(shared)?;
        *shared.window.borrow_mut() = Some(window);
        refresh_status(shared);
    }
    if let Some(window) = shared.window.borrow().as_ref() {
        window.show()?;
    }
    Ok(())
}

/// Builds the window and fills it from the shared state.
fn create_window(shared: &SharedRef) -> Result<MainWindow, AppError> {
    let window = MainWindow::new()?;
    // Every @tr-style string in the UI resolves through here against the
    // language set at startup (and rebuilt on change).
    window
        .global::<I18n>()
        .on_tr(|text| crate::i18n::tr(&text).into());
    window.set_info(AppInfo {
        version: APP_VERSION.into(),
        platform: Platform::current().name().into(),
        config_path: shared.paths.config_file().display().to_string().into(),
        clips_dir: shared.clips_dir().display().to_string().into(),
        log_dir: shared.paths.log_dir.display().to_string().into(),
    });
    window.set_startup_warning(shared.startup_warning.borrow().clone().into());
    {
        let config = shared.config.borrow();
        update_hotkey_labels(&window, &config);
        window
            .global::<Theme>()
            .set_animations(config.general.animations);
    }
    apply_texts(&window, shared);
    apply_game(&window, shared);

    wire_folders(&window, shared);
    wire_window_lifecycle(&window, shared);
    wire_title_bar(&window, shared);
    wire_actions(&window, shared);
    wire_settings(&window, shared);
    wire_library(&window, shared);
    create_player(&window, shared);
    wire_player(&window, shared);
    wire_games(&window, shared);
    wire_updates(&window, shared);
    if let Some(event) = shared.update_banner.borrow().clone() {
        apply_update_event(&window, event);
    }
    info!("window created");
    Ok(window)
}

/// Drops the window and the player so nothing of the UI stays in memory
/// while the app lives in the tray.
fn unload_window(shared: &SharedRef) {
    if let Some(window) = shared.window.borrow().as_ref()
        && let Some(player) = shared.player.borrow_mut().as_mut()
    {
        player.stop(&window.global::<PlayerState>());
    }
    *shared.player.borrow_mut() = None;
    if let Some(window) = shared.window.borrow_mut().take() {
        // The backend keeps a shown window alive on its own, so dropping the
        // handle is not enough: hide it first or the old window lingers next
        // to the rebuilt one.
        if let Err(err) = window.hide() {
            warn!("could not hide the window before unloading it: {err}");
        }
        drop(window);
        info!("window unloaded, capture keeps running in the tray");
    }
}

fn schedule_unload(shared: &SharedRef) {
    let s = shared.clone();
    slint::Timer::single_shot(UNLOAD_DELAY, move || unload_window(&s));
}

fn apply_texts(window: &MainWindow, shared: &SharedRef) {
    let texts = shared.texts.borrow();
    window.set_save_status(texts.save_status.clone().into());
    window.set_last_clip(texts.last_clip.clone().into());
    window.set_last_recording(texts.last_recording.clone().into());
    window.set_recording_message(texts.recording_message.clone().into());
}

fn apply_game(window: &MainWindow, shared: &SharedRef) {
    let (name, icon) = shared.game.borrow().clone();
    let icon = icon.and_then(|p| Image::load_from_path(&p).ok());
    window.set_has_game_icon(icon.is_some());
    window.set_game_icon(icon.unwrap_or_default());
    // Only the "no game" placeholder is translated; real game names are not.
    let name = if name == "No game detected" {
        crate::i18n::tr(&name)
    } else {
        name
    };
    window.set_detected_game(name.into());
}

fn handle_event(shared: &SharedRef, event: UiEvent) {
    match event {
        UiEvent::ClipSaved(result) => {
            match &result {
                Ok(clip) => {
                    let (sound, toast) = {
                        let general = &shared.config.borrow().general;
                        (general.clip_sound, general.clip_toast)
                    };
                    if sound {
                        crate::sound::play_clip_saved();
                    }
                    if toast {
                        let seconds = clip.duration.as_secs_f64().round() as u64;
                        let message = crate::i18n::tr("Clipped the last {seconds} seconds")
                            .replace("{seconds}", &seconds.to_string());
                        if let Err(err) =
                            shared.toast.show(&crate::i18n::tr("Clip saved"), &message)
                        {
                            warn!("could not show the clip notice: {err}");
                        }
                    }
                    let mut texts = shared.texts.borrow_mut();
                    texts.last_clip = clip.path.display().to_string();
                    texts.save_status = crate::i18n::tr("Saved {name} ({seconds} s, {mb} MB)")
                        .replace("{name}", &file_name_of(&clip.path))
                        .replace("{seconds}", &format!("{:.1}", clip.duration.as_secs_f64()))
                        .replace(
                            "{mb}",
                            &format!("{:.1}", clip.bytes as f64 / (1024.0 * 1024.0)),
                        );
                }
                Err(err) => {
                    shared.texts.borrow_mut().save_status =
                        crate::i18n::tr("Could not save clip: {error}")
                            .replace("{error}", &err.to_string());
                }
            }
            if let Ok(clip) = &result {
                index_new_file(shared, clip);
            }
            shared.with_window(|w| apply_texts(w, shared));
        }
        UiEvent::RecordingDone(result) => {
            if let Some(engine) = shared.engine.borrow_mut().as_mut() {
                engine.recording_finished();
            }
            match &result {
                Ok(clip) => {
                    let mut texts = shared.texts.borrow_mut();
                    texts.last_recording = clip.path.display().to_string();
                    texts.recording_message = crate::i18n::tr("Saved {name} ({duration}, {mb} MB)")
                        .replace("{name}", &file_name_of(&clip.path))
                        .replace("{duration}", &format_duration(clip.duration))
                        .replace(
                            "{mb}",
                            &format!("{:.1}", clip.bytes as f64 / (1024.0 * 1024.0)),
                        );
                }
                Err(err) => {
                    shared.texts.borrow_mut().recording_message =
                        crate::i18n::tr("Recording failed: {error}")
                            .replace("{error}", &err.to_string());
                }
            }
            if let Ok(clip) = &result {
                index_new_file(shared, clip);
            }
            shared.with_window(|w| apply_texts(w, shared));
        }
        UiEvent::EditDone {
            result,
            overwrite,
            original,
        } => {
            let message = match &result {
                Ok(clip) => {
                    let name = if overwrite {
                        crate::i18n::tr("the original file")
                    } else {
                        file_name_of(&clip.path)
                    };
                    crate::i18n::tr("Saved {name} ({duration}, {mb} MB)")
                        .replace("{name}", &name)
                        .replace("{duration}", &format_duration(clip.duration))
                        .replace(
                            "{mb}",
                            &format!("{:.1}", clip.bytes as f64 / (1024.0 * 1024.0)),
                        )
                }
                Err(err) => {
                    crate::i18n::tr("Edit failed: {error}").replace("{error}", &err.to_string())
                }
            };
            if let Ok(clip) = &result {
                let path = if overwrite {
                    original
                } else {
                    clip.path.clone()
                };
                let tracks = clip.audio_tracks.clone();
                if let Some(library) = shared.library.borrow_mut().as_mut() {
                    library.refresh();
                    library.tag_tracks(&path, &tracks);
                }
            }
            shared.with_window(|w| {
                let state = w.global::<PlayerState>();
                state.set_trim_busy(false);
                state.set_trim_message(message.into());
                refresh_library_ui(w, shared);
            });
        }
        UiEvent::Update(event) => {
            if let UpdateEvent::Ready(pending) = &event {
                *shared.pending_update.borrow_mut() = Some(pending.clone());
            }
            *shared.update_banner.borrow_mut() = Some(event.clone());
            shared.with_window(|w| apply_update_event(w, event));
        }
        UiEvent::Steam(result) => {
            shared.with_window(|w| apply_steam_result(w, result));
        }
    }
}

/// Puts a freshly written clip or recording into the library with its
/// game and track names, and refreshes the gallery when it is visible.
fn index_new_file(shared: &SharedRef, clip: &ClipFile) {
    if let Some(library) = shared.library.borrow_mut().as_mut() {
        library.refresh();
        library.tag_tracks(&clip.path, &clip.audio_tracks);
        if let Some(game) = &clip.game {
            library.tag_game(&clip.path, game);
        }
    }
    shared.with_window(|w| refresh_library_ui(w, shared));
}

fn update_hotkey_labels(window: &MainWindow, config: &Config) {
    let hotkeys = &config.hotkeys;
    match hotkeys.primary_save() {
        Some(primary) => {
            window.set_save_keys(settings::key_parts(primary.binding));
            window.set_save_label(primary.describe().into());
        }
        None => {
            window.set_save_keys(keys_of(None));
            window.set_save_label("no save hotkey set".into());
        }
    }
    window.set_buffer_keys(keys_of(
        hotkeys.first_of(HotkeyActionKind::ToggleReplayBuffer),
    ));
    window.set_recording_keys(keys_of(hotkeys.first_of(HotkeyActionKind::ToggleRecording)));
    window.set_quality_label(
        settings::quality_label(config.capture.fps, config.capture.bitrate_kbps).into(),
    );
}

fn keys_of(binding: Option<&HotkeyBinding>) -> ModelRc<SharedString> {
    match binding {
        Some(b) => settings::key_parts(b.binding),
        None => ModelRc::new(VecModel::from(vec![SharedString::from("none")])),
    }
}

/// The update banner: install now, wait, or open the release page.
fn wire_updates(window: &MainWindow, shared: &SharedRef) {
    let (s, w) = (shared.clone(), window.as_weak());
    window.on_install_update(move || {
        let Some(window) = w.upgrade() else {
            return;
        };
        let pending = s.pending_update.borrow().clone();
        let Some(pending) = pending else {
            return;
        };
        match updater::install_now(&s.paths, &pending) {
            Ok(()) => {
                info!("installing update {} now", pending.version);
                if let Err(err) = slint::quit_event_loop() {
                    error!("could not stop the event loop: {err}");
                }
            }
            Err(err) => {
                window.set_update_message(format!("Update failed: {err}").into());
                window.set_update_ready(false);
            }
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    window.on_dismiss_update(move || {
        *s.update_banner.borrow_mut() = None;
        if let Some(window) = w.upgrade() {
            window.set_update_message("".into());
        }
    });
    let w = window.as_weak();
    window.on_open_update_page(move || {
        if let Some(window) = w.upgrade() {
            shell::open_url(&window.get_update_page());
        }
    });
}

fn apply_update_event(window: &MainWindow, event: UpdateEvent) {
    match event {
        UpdateEvent::Downloading { version } => {
            window.set_update_ready(false);
            window.set_update_page("".into());
            window.set_update_message(
                format!("OpenClips {version} is being downloaded in the background.").into(),
            );
        }
        UpdateEvent::Ready(pending) => {
            window.set_update_page(pending.release_url.clone().into());
            window.set_update_message(
                format!(
                    "OpenClips {} is ready. It installs on the next start, or now if you prefer.",
                    pending.version
                )
                .into(),
            );
            window.set_update_ready(true);
        }
        UpdateEvent::Available { version, url } => {
            window.set_update_ready(false);
            window.set_update_page(url.into());
            window.set_update_message(
                format!("OpenClips {version} is available. This portable copy does not update itself; download it from the release page.").into(),
            );
        }
    }
}

fn wire_folders(window: &MainWindow, shared: &SharedRef) {
    let config_dir = shared.paths.config_dir.clone();
    window.on_open_config_dir(move || shell::open_folder(&config_dir));
    let s = shared.clone();
    window.on_open_clips_dir(move || shell::open_folder(&s.clips_dir()));
    let logs = shared.paths.log_dir.clone();
    window.on_open_logs_dir(move || shell::open_folder(&logs));
}

/// Closing hides the window and then drops it; the tray brings it back.
fn wire_window_lifecycle(window: &MainWindow, shared: &SharedRef) {
    let s = shared.clone();
    window.window().on_close_requested(move || {
        info!("window closed, staying in the tray");
        schedule_unload(&s);
        CloseRequestResponse::HideWindow
    });
}

fn wire_tray(shared: &SharedRef) {
    let s = shared.clone();
    shared.tray.on_open_window(move || {
        if let Err(err) = show_window(&s) {
            error!("could not show the main window: {err}");
        }
    });
    let s = shared.clone();
    shared.tray.on_toggle_buffer(move || toggle_buffer(&s));
    let s = shared.clone();
    shared.tray.on_save_clip(move || save_clip(&s, None));
    let s = shared.clone();
    shared
        .tray
        .on_toggle_recording(move || toggle_recording(&s));
    shared.tray.on_quit(|| {
        info!("quit requested from the tray");
        if let Err(err) = slint::quit_event_loop() {
            error!("could not stop the event loop: {err}");
        }
    });
}

/// The window draws its own title bar; moving and the three buttons go
/// through the OS so they behave like a native frame.
fn wire_title_bar(window: &MainWindow, shared: &SharedRef) {
    use slint::winit_030::WinitWindowAccessor;

    let w = window.as_weak();
    window.on_window_drag(move || {
        if let Some(window) = w.upgrade() {
            // winit tracks the system move and delivers the button release
            // when it ends, so the UI never gets stuck in a pressed state.
            window.window().with_winit_window(|winit| {
                if let Err(err) = winit.drag_window() {
                    warn!("could not start moving the window: {err}");
                }
            });
        }
    });
    let w = window.as_weak();
    window.on_window_minimize(move || {
        if let Some(window) = w.upgrade() {
            window.window().set_minimized(true);
        }
    });
    let w = window.as_weak();
    window.on_window_maximize(move || {
        if let Some(window) = w.upgrade() {
            let maximized = window.window().is_maximized();
            window.window().set_maximized(!maximized);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    window.on_window_close(move || {
        if let Some(window) = w.upgrade() {
            info!("window closed, staying in the tray");
            let _ = window.hide();
            schedule_unload(&s);
        }
    });
}

fn wire_actions(window: &MainWindow, shared: &SharedRef) {
    let s = shared.clone();
    window.on_toggle_buffer(move || toggle_buffer(&s));
    let s = shared.clone();
    window.on_save_clip(move || save_clip(&s, None));
    let s = shared.clone();
    window.on_toggle_recording(move || toggle_recording(&s));
}

fn install_hotkeys(shared: &SharedRef) {
    let s = shared.clone();
    hotkeys::install_dispatch(move |action| match action {
        HotkeyAction::SaveReplay { index } => save_clip(&s, Some(index)),
        HotkeyAction::ToggleReplayBuffer => toggle_buffer(&s),
        HotkeyAction::ToggleRecording => toggle_recording(&s),
    });
    if let Some(problem) = register_hotkeys(shared) {
        warn!("{problem}");
        let mut warning = shared.startup_warning.borrow_mut();
        if !warning.is_empty() {
            warning.push('\n');
        }
        warning.push_str(&problem);
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
                    .map(|(action, reason)| format!("{}: {reason}", action.label()))
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

    let s = shared.clone();
    state.on_changed(move || schedule_autosave(&s));

    let w = window.as_weak();
    state.on_browse_clips_dir(move || {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<SettingsState>();
        let current = PathBuf::from(state.get_clips_dir().as_str());
        let mut dialog =
            rfd::FileDialog::new().set_title(crate::i18n::tr("Choose the clips folder"));
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

    let (s, w) = (shared.clone(), window.as_weak());
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
        if action >= settings::SAVE_ACTION_BASE {
            settings::set_hotkey_binding(
                &state,
                (action - settings::SAVE_ACTION_BASE) as usize,
                hotkey,
            );
            schedule_autosave(&s);
        }
        state.set_listening_action(-1);
    });

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_add_hotkey(move || {
        if let Some(window) = w.upgrade() {
            settings::add_hotkey(&window.global::<SettingsState>());
            schedule_autosave(&s);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_remove_hotkey(move |index| {
        if let Some(window) = w.upgrade() {
            settings::remove_hotkey(&window.global::<SettingsState>(), index.max(0) as usize);
            schedule_autosave(&s);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_add_app_audio(move |exe| {
        if let Some(window) = w.upgrade() {
            let state = window.global::<SettingsState>();
            settings::add_app_source(&state, &exe);
            state.set_new_app_exe("".into());
            schedule_autosave(&s);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_remove_audio_source(move |id| {
        if let Some(window) = w.upgrade() {
            settings::remove_audio_source(&window.global::<SettingsState>(), &id);
            schedule_autosave(&s);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_refresh_apps(move || {
        if let Some(window) = w.upgrade() {
            refresh_app_candidates(&window, &s);
        }
    });
    refresh_app_candidates(window, shared);
}

/// Fills the list of running executables offered as application audio
/// sources. Windows system processes are left out to keep the list short.
fn refresh_app_candidates(window: &MainWindow, shared: &SharedRef) {
    const SKIP: [&str; 14] = [
        "svchost.exe",
        "csrss.exe",
        "conhost.exe",
        "dwm.exe",
        "winlogon.exe",
        "services.exe",
        "lsass.exe",
        "smss.exe",
        "wininit.exe",
        "fontdrvhost.exe",
        "sihost.exe",
        "taskhostw.exe",
        "runtimebroker.exe",
        "openclips.exe",
    ];
    let running = shared
        .engine
        .borrow()
        .as_ref()
        .and_then(|e| e.process_watcher().running().ok())
        .unwrap_or_default();
    let mut names: Vec<String> = running
        .into_iter()
        .map(|p| p.exe)
        .filter(|exe| !SKIP.contains(&exe.as_str()) && !exe.is_empty())
        .collect();
    names.sort();
    names.dedup();
    let names: Vec<SharedString> = names.into_iter().map(SharedString::from).collect();
    let state = window.global::<SettingsState>();
    state.set_app_candidates(ModelRc::new(VecModel::from(names)));
    state.set_app_candidate_index(0);
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
    // Refilling the page after a save reports changes too; they are no-ops.
    if next == *shared.config.borrow() {
        return;
    }
    if let Err(err) = next.save(&shared.paths.config_file()) {
        state.set_message_is_error(true);
        state.set_message(format!("Could not save settings: {err}").into());
        return;
    }
    let mut problems = Vec::new();

    let startup_changed = {
        let current = &shared.config.borrow().general;
        current.launch_on_startup != next.general.launch_on_startup
            || (next.general.launch_on_startup
                && current.start_minimized != next.general.start_minimized)
    };
    if startup_changed
        && let Err(err) =
            crate::startup::apply(next.general.launch_on_startup, next.general.start_minimized)
    {
        problems.push(err);
    }
    let mut rebind = shared.config.borrow().hotkeys_changed(&next);
    let language_changed = shared.config.borrow().general.language != next.general.language;
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

    update_hotkey_labels(window, &next);
    window
        .global::<Theme>()
        .set_animations(next.general.animations);
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
        state.set_message(crate::i18n::tr("Settings saved.").into());
        let s = shared.clone();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::SingleShot, SAVED_NOTICE, move || {
            s.with_window(|w| w.global::<SettingsState>().set_message("".into()));
        });
        *shared.message_timer.borrow_mut() = Some(timer);
    } else {
        state.set_message_is_error(true);
        state.set_message(
            crate::i18n::tr("Settings saved. {problems}")
                .replace("{problems}", &problems.join(" "))
                .into(),
        );
    }
    info!("settings saved");

    // The language is applied by rebuilding the window with the new catalog.
    // Deferred so it runs after this callback returns, not mid frame.
    if language_changed {
        crate::i18n::set_language(next.general.language);
        let s = shared.clone();
        slint::Timer::single_shot(UNLOAD_DELAY, move || reload_window(&s));
    }
}

/// Saves the settings page shortly after the last change, so typing or a
/// quick series of toggles ends in one save and one capture restart.
fn schedule_autosave(shared: &SharedRef) {
    let s = shared.clone();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::SingleShot, AUTOSAVE_DELAY, move || {
        let window = s.window.borrow();
        if let Some(window) = window.as_ref() {
            save_settings(&s, window);
        }
    });
    *shared.autosave.borrow_mut() = Some(timer);
}

/// Rebuilds the open window so every translated string is re-evaluated in the
/// current language.
fn reload_window(shared: &SharedRef) {
    if shared.window.borrow().is_none() {
        return;
    }
    unload_window(shared);
    if let Err(err) = show_window(shared) {
        error!("could not rebuild the window after a language change: {err}");
    }
}

fn start_status_timer(shared: &SharedRef) {
    let timer = slint::Timer::default();
    let s = shared.clone();
    timer.start(slint::TimerMode::Repeated, STATUS_REFRESH, move || {
        let tick = {
            let mut ticks = s.ticks.borrow_mut();
            *ticks = ticks.wrapping_add(1);
            *ticks
        };
        if tick.is_multiple_of(MONITOR_REFRESH_TICKS) {
            poll_monitors(&s);
            poll_games(&s);
        }
        if s.instance.take_show_request() {
            info!("another launch asked for the window");
            if let Err(err) = show_window(&s) {
                error!("could not show the main window: {err}");
            }
        }
        refresh_status(&s);
        let changed = s.library.borrow_mut().as_mut().is_some_and(|l| l.poll());
        if changed {
            s.with_window(|w| refresh_library_ui(w, &s));
        }
        if let Some(window) = s.window.borrow().as_ref()
            && let Some(player) = s.player.borrow_mut().as_mut()
        {
            player.tick(&window.global::<PlayerState>());
        }
    });
    *shared.timer.borrow_mut() = Some(timer);
}

fn poll_monitors(shared: &SharedRef) {
    let changed = shared
        .engine
        .borrow_mut()
        .as_mut()
        .is_some_and(|e| e.refresh_monitors());
    if changed {
        shared.with_window(|window| {
            let state = window.global::<SettingsState>();
            let monitors = current_monitors(shared);
            let selected = settings::selected_display(&state, &monitors);
            settings::set_monitors(&state, &monitors, &selected);
        });
    }
}

/// Runs an engine action and shows its outcome in the window when there
/// is one; without a window problems only go to the log.
fn run_engine(
    shared: &SharedRef,
    window: Option<&MainWindow>,
    action: impl FnOnce(&mut Engine) -> Result<(), AppError>,
) {
    let mut slot = shared.engine.borrow_mut();
    let Some(engine) = slot.as_mut() else {
        if let Some(window) = window {
            window.set_capture_error(
                crate::i18n::tr("Capture is unavailable on this system.").into(),
            );
        }
        return;
    };
    match action(engine) {
        Ok(()) => {
            if let Some(window) = window {
                window.set_capture_error("".into());
            }
        }
        Err(err) => {
            error!("{err}");
            if let Some(window) = window {
                window.set_capture_error(err.to_string().into());
            }
        }
    }
}

fn toggle_buffer(shared: &SharedRef) {
    let window = shared.window.borrow();
    run_engine(shared, window.as_ref(), |e| e.toggle_buffer());
}

fn toggle_recording(shared: &SharedRef) {
    let window = shared.window.borrow();
    run_engine(shared, window.as_ref(), |e| {
        e.toggle_recording(Box::new(|result| post(UiEvent::RecordingDone(result))))
    });
}

fn save_clip(shared: &SharedRef, hotkey_index: Option<usize>) {
    let length = hotkey_index
        .and_then(|i| shared.config.borrow().hotkeys.bindings.get(i).copied())
        .map(|b| Duration::from_secs(u64::from(b.seconds)));
    let slot = shared.engine.borrow();
    let Some(engine) = slot.as_ref() else {
        shared.with_window(|w| {
            w.set_capture_error(crate::i18n::tr("Capture is unavailable on this system.").into())
        });
        return;
    };
    shared.texts.borrow_mut().save_status = crate::i18n::tr("Saving clip...");
    shared.with_window(|w| apply_texts(w, shared));
    engine.save_clip(length, Box::new(|result| post(UiEvent::ClipSaved(result))));
}

fn refresh_status(shared: &SharedRef) {
    let status = {
        let mut slot = shared.engine.borrow_mut();
        slot.as_mut().map(|engine| engine.status())
    };
    let Some(status) = status else {
        shared.with_window(|window| {
            window.set_buffer_active(false);
            window.set_buffer_status(crate::i18n::tr("Unavailable").into());
        });
        shared.tray.set_buffer_active(false);
        shared.tray.set_recording_active(false);
        shared.discord.update(PresenceState::default());
        return;
    };
    let buffering = status.buffer == BufferState::Running;
    let recording = matches!(
        status.recording,
        RecordingState::Starting | RecordingState::Active { .. }
    );
    shared.tray.set_buffer_active(buffering);
    shared.tray.set_recording_active(recording);
    let game = shared.game.borrow().0.clone();
    shared.discord.update(PresenceState {
        config: shared.config.borrow().discord.clone(),
        game: (game != "No game detected" && !game.is_empty()).then_some(game),
        buffering,
        recording,
    });
    if let RecordingState::Failed(reason) = &status.recording {
        shared.texts.borrow_mut().recording_message =
            crate::i18n::tr("Recording failed: {error}").replace("{error}", reason);
    }

    shared.with_window(|window| {
        window.set_buffer_active(buffering);
        window.set_buffer_label(
            crate::i18n::tr(if buffering { "Buffer on" } else { "Buffer off" }).into(),
        );
        window.set_buffer_status(describe_buffer_state(&status).into());
        window.set_buffer_detail(describe_buffer(&status).into());
        window.set_encoder_name(
            format!("{} ({})", status.encoder.kind.label(), status.backend).into(),
        );
        if let BufferState::Failed(reason) = &status.buffer {
            window.set_capture_error(reason.clone().into());
        }
        let mut notice = status.notice.clone().unwrap_or_default();
        if status.blank {
            if !notice.is_empty() {
                notice.push(' ');
            }
            notice.push_str(&crate::i18n::tr(
                "The capture looks black. If the game runs in exclusive fullscreen, switch it to borderless windowed, or choose Windows Graphics Capture under Settings.",
            ));
        }
        window.set_capture_notice(notice.into());
        window.set_recording_active(recording);
        window.set_recording_status(describe_recording(&status.recording).into());
        if matches!(status.recording, RecordingState::Failed(_)) {
            apply_texts(window, shared);
        }
    });
}

fn describe_buffer_state(status: &EngineStatus) -> String {
    match &status.buffer {
        BufferState::Stopped => crate::i18n::tr("Stopped"),
        BufferState::Running => crate::i18n::tr("Recording into memory"),
        BufferState::Failed(_) => crate::i18n::tr("Failed"),
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
        1 => crate::i18n::tr(", 1 audio track"),
        n => crate::i18n::tr(", {n} audio tracks").replace("{n}", &n.to_string()),
    };
    crate::i18n::tr("{available} of {total} s buffered, {mb} MB in memory{resolution}{audio}")
        .replace("{available}", &format!("{available:.0}"))
        .replace("{total}", &status.replay_length.as_secs().to_string())
        .replace(
            "{mb}",
            &format!("{:.0}", stats.bytes as f64 / (1024.0 * 1024.0)),
        )
        .replace("{resolution}", &resolution)
        .replace("{audio}", &audio)
}

fn describe_recording(state: &RecordingState) -> String {
    match state {
        RecordingState::Idle => crate::i18n::tr("Not recording"),
        RecordingState::Starting => crate::i18n::tr("Starting..."),
        RecordingState::Active { path, duration } => {
            crate::i18n::tr("Recording {name} ({duration})")
                .replace("{name}", &file_name_of(path))
                .replace("{duration}", &format_duration(*duration))
        }
        RecordingState::Finishing => crate::i18n::tr("Finishing file..."),
        RecordingState::Failed(_) => crate::i18n::tr("Failed"),
    }
}

fn init_library(shared: &SharedRef) {
    let engine = shared.engine.borrow();
    let Some(engine) = engine.as_ref() else {
        return;
    };
    let library = LibraryService::new(&shared.paths, &shared.config.borrow(), engine.media_tools());
    *shared.library.borrow_mut() = Some(library);
}

/// The player belongs to the window: it pushes decoded frames into it and
/// dies with it.
fn create_player(window: &MainWindow, shared: &SharedRef) {
    let engine = shared.engine.borrow();
    let Some(engine) = engine.as_ref() else {
        window.global::<LibraryState>().set_message(
            "The clip library needs the media framework, which is unavailable.".into(),
        );
        window
            .global::<PlayerState>()
            .set_message("Playback is unavailable on this system.".into());
        return;
    };
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
    state.on_edit_clip(move |id| {
        if let Some(window) = w.upgrade() {
            open_clip(&window, &s, &id);
            window.global::<PlayerState>().set_editing(true);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    window.on_open_clip(move |id| {
        if let Some(window) = w.upgrade() {
            open_clip(&window, &s, &id);
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
    let mut names: Vec<SharedString> = vec![crate::i18n::tr("All games").into()];
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
    let kind = match state.get_kind_index() {
        1 => Some(ClipKind::Replay),
        2 => Some(ClipKind::Recording),
        3 => Some(ClipKind::Edited),
        _ => None,
    };
    let cards: Vec<ClipCard> = library
        .cards(&CardFilter {
            game: filter.as_deref(),
            kind,
            search: &search,
            sort: CardSort::from_index(state.get_sort_index()),
        })
        .into_iter()
        .map(|c| to_card(shared, c))
        .collect();
    let total = cards.len();
    state.set_clips(ModelRc::new(VecModel::from(cards)));
    state.set_summary(
        match total {
            0 => String::new(),
            1 => crate::i18n::tr("1 clip"),
            n => crate::i18n::tr("{n} clips").replace("{n}", &n.to_string()),
        }
        .into(),
    );

    let recent: Vec<ClipCard> = library
        .cards(&CardFilter::default())
        .into_iter()
        .take(12)
        .map(|c| to_card(shared, c))
        .collect();
    window.set_recent_clips(ModelRc::new(VecModel::from(recent)));
    update_storage(window, library.total_bytes(), &shared.clips_dir());
}

fn to_card(shared: &SharedRef, c: crate::library::CardData) -> ClipCard {
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
        size: c.size.into(),
        kind: c.kind.into(),
        has_thumbnail: thumbnail.is_some(),
        thumbnail: thumbnail.unwrap_or_default(),
        has_icon: icon.is_some(),
        icon: icon.unwrap_or_default(),
    }
}

fn update_storage(window: &MainWindow, used: u64, clips_dir: &Path) {
    let settings = window.global::<SettingsState>();
    match shell::disk_space(clips_dir) {
        Some((free, total)) if total > 0 => {
            window.set_storage_line(
                format!("{} used\n{} free", format_size(used), format_size(free)).into(),
            );
            settings.set_storage_line(
                format!(
                    "{} of clips in this folder. {} free of {} on the drive.",
                    format_size(used),
                    format_size(free),
                    format_size(total)
                )
                .into(),
            );
            settings.set_storage_fraction(((total - free) as f64 / total as f64) as f32);
        }
        _ => {
            window.set_storage_line(
                crate::i18n::tr("{size} used")
                    .replace("{size}", &format_size(used))
                    .into(),
            );
            settings.set_storage_line(
                crate::i18n::tr("{size} of clips in this folder.")
                    .replace("{size}", &format_size(used))
                    .into(),
            );
            settings.set_storage_fraction(0.0);
        }
    }
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
    state.set_save_prompt(false);
    state.set_compress_open(false);
    let tracks: Vec<AudioTrackRow> = record
        .audio_tracks
        .iter()
        .map(|name| AudioTrackRow {
            name: name.clone().into(),
            enabled: true,
        })
        .collect();
    state.set_audio_tracks(ModelRc::new(VecModel::from(tracks)));
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
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_volume_changed(move |percent| {
        if let Some(window) = w.upgrade() {
            window.global::<PlayerState>().set_muted(false);
        }
        if let Some(player) = s.player.borrow_mut().as_mut() {
            player.set_volume(percent);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_toggle_mute(move || {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<PlayerState>();
        let muted = !state.get_muted();
        state.set_muted(muted);
        if let Some(player) = s.player.borrow_mut().as_mut() {
            player.set_volume(if muted { 0.0 } else { state.get_volume() });
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_skip(move |delta| {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<PlayerState>();
        if let Some(player) = s.player.borrow_mut().as_mut() {
            let position = player
                .position()
                .map(|p| p.as_secs_f32())
                .unwrap_or(state.get_position());
            let target = (position + delta).clamp(0.0, state.get_duration().max(0.0));
            player.seek(target);
            state.set_position(target);
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
    state.on_save_trim_as(move |overwrite| {
        if let Some(window) = w.upgrade() {
            window.global::<PlayerState>().set_save_prompt(false);
            save_trim(&window, &s, overwrite);
        }
    });
    let (s, w) = (shared.clone(), window.as_weak());
    state.on_compress(move || {
        if let Some(window) = w.upgrade() {
            window.global::<PlayerState>().set_compress_open(false);
            compress_clip(&window, &s);
        }
    });
}

fn current_record(shared: &SharedRef) -> Option<ClipRecord> {
    let id = current_clip_id(shared)?;
    shared
        .library
        .borrow()
        .as_ref()
        .and_then(|l| l.record(&id).cloned())
}

fn edited_dir(shared: &SharedRef) -> PathBuf {
    shared.config.borrow().edited_dir(&shared.paths)
}

/// Audio tracks kept for a cut, from the editor's toggles.
fn kept_tracks(state: &PlayerState<'_>) -> Vec<bool> {
    state.get_audio_tracks().iter().map(|t| t.enabled).collect()
}

fn compress_clip(window: &MainWindow, shared: &SharedRef) {
    let state = window.global::<PlayerState>();
    let Some(record) = current_record(shared) else {
        state.set_trim_message("Open a clip first.".into());
        return;
    };
    let preset = COMPRESS_PRESETS
        .get(state.get_compress_index().max(0) as usize)
        .copied()
        .unwrap_or(COMPRESS_PRESETS[0]);
    let duration = if record.duration().is_zero() {
        Duration::from_secs_f32(state.get_duration().max(0.0))
    } else {
        record.duration()
    };
    let range = match TrimRange::new(Duration::ZERO, duration, duration) {
        Ok(range) => range,
        Err(err) => {
            state.set_trim_message(err.to_string().into());
            return;
        }
    };
    let output = edited_path(
        &edited_dir(shared),
        &record.path,
        &record.title,
        "compressed",
    );
    let job = TrimJob {
        input: record.path.clone(),
        output,
        range,
        mode: TrimMode::FrameAccurate,
        video_bitrate_kbps: preset.2,
        audio_bitrate_kbps: 128,
        scale_height: (record.height > preset.1).then_some(preset.1),
        keep_audio: kept_tracks(&state),
        audio_labels: record.audio_tracks.clone(),
    };
    run_edit_job(
        window,
        shared,
        job,
        false,
        "Compressing, this takes a moment...",
    );
}

/// Runs a cut or a compression on a worker thread and reports into the
/// editor. With `overwrite` the result replaces the original file.
fn run_edit_job(
    window: &MainWindow,
    shared: &SharedRef,
    job: TrimJob,
    overwrite: bool,
    busy_message: &str,
) {
    let state = window.global::<PlayerState>();
    let tools = shared.engine.borrow().as_ref().map(|e| e.media_tools());
    let Some(tools) = tools else {
        state.set_trim_message("Editing is unavailable on this system.".into());
        return;
    };
    if overwrite && let Some(player) = shared.player.borrow_mut().as_mut() {
        player.stop(&state);
    }
    state.set_trim_busy(true);
    state.set_trim_message(busy_message.into());
    let original = job.input.clone();
    let spawned = std::thread::Builder::new()
        .name("edit".to_owned())
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
            post(UiEvent::EditDone {
                result,
                overwrite,
                original,
            });
        });
    if let Err(err) = spawned {
        state.set_trim_busy(false);
        state.set_trim_message(format!("Could not start the edit: {err}").into());
    }
}

fn save_trim(window: &MainWindow, shared: &SharedRef, overwrite: bool) {
    let state = window.global::<PlayerState>();
    let Some(record) = current_record(shared) else {
        state.set_trim_message("Open a clip first.".into());
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
    let keep_audio = kept_tracks(&state);
    let drops_tracks = keep_audio.iter().any(|k| !k);
    if range.is_whole(duration) && !drops_tracks {
        state.set_trim_message(
            "The selection covers the whole clip and every track is on; nothing to save.".into(),
        );
        return;
    }
    let mode = if state.get_trim_mode_index() == 1 {
        TrimMode::FrameAccurate
    } else {
        TrimMode::StreamCopy
    };
    let output = if overwrite {
        record.path.with_extension("mp4.trimmed")
    } else {
        edited_path(&edited_dir(shared), &record.path, &record.title, "trim")
    };
    let (video_bitrate, audio_bitrate) = {
        let config = shared.config.borrow();
        (config.capture.bitrate_kbps, config.audio.bitrate_kbps)
    };
    let job = TrimJob {
        input: record.path.clone(),
        output,
        range,
        mode,
        video_bitrate_kbps: video_bitrate,
        audio_bitrate_kbps: audio_bitrate,
        scale_height: None,
        keep_audio,
        audio_labels: record.audio_tracks.clone(),
    };
    let message = match mode {
        TrimMode::StreamCopy => "Cutting...",
        TrimMode::FrameAccurate => "Re-encoding the selection, this takes a moment...",
    };
    run_edit_job(window, shared, job, overwrite, message);
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
fn poll_games(shared: &SharedRef) {
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
    let name = active
        .as_ref()
        .map(|g| g.name.clone())
        .unwrap_or_else(|| "No game detected".to_owned());
    if shared.game.borrow().0 != name {
        let icon = shared
            .games
            .borrow()
            .as_ref()
            .and_then(|g| g.icon_for_name(&name, &shared.config.borrow().games));
        *shared.game.borrow_mut() = (name, icon);
        shared.with_window(|w| apply_game(w, shared));
    }
    shared.with_window(|window| {
        let state = window.global::<SettingsState>();
        if state.get_running_games() != running_text {
            state.set_running_games(running_text.clone().into());
            refresh_running_known(window, shared);
        }
    });
    let window = shared.window.borrow();
    run_engine(shared, window.as_ref(), |e| e.set_game_state(active, auto));
    run_engine(shared, window.as_ref(), |e| e.poll_app_audio());
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
        let mut profiles = settings::collect_game_profiles(&state, &monitors, &[]);
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
        state.set_games_message(format!("Added {}.", game.name).into());
        schedule_autosave(&s);
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
        let mut profiles = settings::collect_game_profiles(&state, &monitors, &[]);
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
        state.set_games_message(format!("Added {exe}.").into());
        schedule_autosave(&s);
    });

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_remove_game(move |exe| {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<SettingsState>();
        let monitors = current_monitors(&s);
        let mut profiles = settings::collect_game_profiles(&state, &monitors, &[]);
        profiles.retain(|p| p.exe != exe.as_str());
        set_game_rows(&window, &s, &profiles);
        state.set_games_message("Removed.".into());
        schedule_autosave(&s);
    });

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_suggest_steam_names(move || {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<SettingsState>();
        let monitors = current_monitors(&s);
        let profiles = settings::collect_game_profiles(&state, &monitors, &[]);
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
            post(UiEvent::Steam(result));
        });
    });
}

fn apply_steam_result(window: &MainWindow, result: Result<Vec<(String, String)>, String>) {
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
            state.set_games_message(format!("Filled {} name(s) from Steam.", found.len()).into());
        }
        Err(err) => state.set_games_message(err.into()),
    }
}
