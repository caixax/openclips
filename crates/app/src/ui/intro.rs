//! The first start walkthrough: key, capture scope, clip length and folder.
//! Shown over the window until it is finished or skipped, then never again.

use std::path::{Path, PathBuf};

use openclips_core::config::{CaptureScope, Hotkey, HotkeyActionKind, HotkeyBinding};
use slint::ComponentHandle;
use tracing::{info, warn};

use super::{IntroState, MainWindow, SharedRef, apply_settings};
use crate::settings;

/// The `HotkeyCapture` action id the walkthrough uses; settings rows start
/// at `settings::SAVE_ACTION_BASE` and never reach it.
pub const HOTKEY_ACTION: i32 = 200;

pub fn wire(window: &MainWindow, shared: &SharedRef) {
    let state = window.global::<IntroState>();
    {
        let config = shared.config.borrow();
        let save = config
            .hotkeys
            .bindings
            .iter()
            .find(|b| b.action == HotkeyActionKind::SaveReplay)
            .copied()
            .unwrap_or_default();
        *shared.intro_hotkey.borrow_mut() = Some(save.binding);
        state.set_hotkey_keys(settings::key_parts(save.binding));
        let seconds = if save.seconds > 0 {
            save.seconds
        } else {
            config.replay.length_seconds
        };
        state.set_minutes((seconds / 60) as i32);
        state.set_seconds((seconds % 60) as i32);
        state.set_scope_index(match config.games.scope {
            CaptureScope::PerGame => 0,
            CaptureScope::Global => 1,
        });
        state.set_clips_dir(shared.clips_dir().display().to_string().into());
        state.set_step(0);
        state.set_visible(!config.general.intro_done);
    }

    let w = window.as_weak();
    state.on_browse(move || {
        let Some(window) = w.upgrade() else {
            return;
        };
        let state = window.global::<IntroState>();
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
    state.on_finish(move || {
        if let Some(window) = w.upgrade() {
            finish(&window, &s);
        }
    });

    let (s, w) = (shared.clone(), window.as_weak());
    state.on_skip(move || {
        if let Some(window) = w.upgrade() {
            info!("first start walkthrough skipped");
            let mut next = s.config.borrow().clone();
            next.general.intro_done = true;
            apply_settings(&s, &window, next);
            window.global::<IntroState>().set_visible(false);
        }
    });
}

/// Called from the settings key capture when the walkthrough's pill is
/// listening.
pub fn set_hotkey(window: &MainWindow, shared: &SharedRef, hotkey: Hotkey) {
    *shared.intro_hotkey.borrow_mut() = Some(hotkey);
    window
        .global::<IntroState>()
        .set_hotkey_keys(settings::key_parts(hotkey));
}

fn finish(window: &MainWindow, shared: &SharedRef) {
    let state = window.global::<IntroState>();
    let mut next = shared.config.borrow().clone();

    let seconds =
        state.get_minutes().clamp(0, 60) as u32 * 60 + state.get_seconds().clamp(0, 59) as u32;
    let hotkey = shared
        .intro_hotkey
        .borrow()
        .unwrap_or(HotkeyBinding::default().binding);
    match next
        .hotkeys
        .bindings
        .iter_mut()
        .find(|b| b.action == HotkeyActionKind::SaveReplay)
    {
        Some(binding) => {
            binding.binding = hotkey;
            binding.seconds = seconds;
        }
        None => next.hotkeys.bindings.insert(
            0,
            HotkeyBinding {
                binding: hotkey,
                action: HotkeyActionKind::SaveReplay,
                seconds,
            },
        ),
    }
    if seconds > 0 {
        next.replay.length_seconds = seconds;
    }
    next.games.scope = if state.get_scope_index() == 1 {
        CaptureScope::Global
    } else {
        CaptureScope::PerGame
    };
    let dir = state.get_clips_dir();
    let dir = dir.trim();
    next.output.clips_dir = if dir.is_empty() || Path::new(dir) == shared.default_clips_dir() {
        None
    } else {
        Some(PathBuf::from(dir))
    };
    next.general.intro_done = true;

    if let Err(err) = next.validate() {
        warn!("walkthrough answers rejected: {err}");
        return;
    }
    info!("first start walkthrough finished");
    apply_settings(shared, window, next);
    state.set_visible(false);
}
