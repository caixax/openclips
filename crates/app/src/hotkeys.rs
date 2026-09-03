//! Global hotkeys. The manager must be created on the thread that runs the
//! window message loop, which is the Slint UI thread, and it must stay alive
//! for as long as the bindings should work.

use std::cell::RefCell;
use std::collections::HashMap;
use std::str::FromStr;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use openclips_core::config::{Hotkey, HotkeyConfig, Key};
use tracing::{info, warn};

use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    SaveReplay,
    ToggleReplayBuffer,
    ToggleRecording,
}

pub struct Hotkeys {
    _manager: GlobalHotKeyManager,
}

type Dispatch = Box<dyn Fn(HotkeyAction)>;

thread_local! {
    static DISPATCH: RefCell<Option<Dispatch>> = const { RefCell::new(None) };
}

/// Registers every binding from the config and routes presses to `dispatch`
/// on the UI thread. Bindings that the OS refuses (typically because another
/// application owns them) are logged and skipped rather than failing startup.
pub fn install(
    config: &HotkeyConfig,
    dispatch: impl Fn(HotkeyAction) + 'static,
) -> Result<Hotkeys, AppError> {
    let manager = GlobalHotKeyManager::new().map_err(|e| AppError::Hotkey(e.to_string()))?;

    let wanted = [
        (HotkeyAction::SaveReplay, config.save_replay),
        (
            HotkeyAction::ToggleReplayBuffer,
            config.toggle_replay_buffer,
        ),
        (HotkeyAction::ToggleRecording, config.toggle_recording),
    ];
    let mut ids: HashMap<u32, HotkeyAction> = HashMap::new();
    for (action, binding) in wanted {
        let hotkey = match to_hotkey(binding) {
            Ok(hotkey) => hotkey,
            Err(err) => {
                warn!("{action:?}: {err}");
                continue;
            }
        };
        match manager.register(hotkey) {
            Ok(()) => {
                info!("registered {binding} for {action:?}");
                ids.insert(hotkey.id(), action);
            }
            Err(err) => warn!("could not register {binding} for {action:?}: {err}"),
        }
    }

    DISPATCH.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(dispatch));
    });

    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state() != HotKeyState::Pressed {
            return;
        }
        let Some(action) = ids.get(&event.id()).copied() else {
            return;
        };
        info!("hotkey pressed: {action:?}");
        let queued = slint::invoke_from_event_loop(move || {
            DISPATCH.with(|slot| {
                if let Some(dispatch) = slot.borrow().as_ref() {
                    dispatch(action);
                }
            });
        });
        if let Err(err) = queued {
            warn!("could not dispatch hotkey {action:?}: {err}");
        }
    }));

    Ok(Hotkeys { _manager: manager })
}

fn to_hotkey(binding: Hotkey) -> Result<HotKey, AppError> {
    let mut mods = Modifiers::empty();
    if binding.modifiers.ctrl {
        mods |= Modifiers::CONTROL;
    }
    if binding.modifiers.alt {
        mods |= Modifiers::ALT;
    }
    if binding.modifiers.shift {
        mods |= Modifiers::SHIFT;
    }
    if binding.modifiers.super_key {
        mods |= Modifiers::SUPER;
    }
    let code = to_code(binding.key)?;
    Ok(HotKey::new((!mods.is_empty()).then_some(mods), code))
}

fn to_code(key: Key) -> Result<Code, AppError> {
    let name = match key {
        Key::Char(c) if c.is_ascii_digit() => format!("Digit{c}"),
        Key::Char(c) => format!("Key{}", c.to_ascii_uppercase()),
        Key::F(n) => format!("F{n}"),
        Key::Numpad(n) => format!("Numpad{n}"),
        Key::Space => "Space".to_owned(),
        Key::Enter => "Enter".to_owned(),
        Key::Escape => "Escape".to_owned(),
        Key::Tab => "Tab".to_owned(),
        Key::Backspace => "Backspace".to_owned(),
        Key::Delete => "Delete".to_owned(),
        Key::Insert => "Insert".to_owned(),
        Key::Home => "Home".to_owned(),
        Key::End => "End".to_owned(),
        Key::PageUp => "PageUp".to_owned(),
        Key::PageDown => "PageDown".to_owned(),
        Key::Up => "ArrowUp".to_owned(),
        Key::Down => "ArrowDown".to_owned(),
        Key::Left => "ArrowLeft".to_owned(),
        Key::Right => "ArrowRight".to_owned(),
        Key::PrintScreen => "PrintScreen".to_owned(),
        Key::ScrollLock => "ScrollLock".to_owned(),
        Key::Pause => "Pause".to_owned(),
    };
    Code::from_str(&name).map_err(|_| AppError::Hotkey(format!("unsupported key {key}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_key_kind() {
        for key in [
            Key::Char('8'),
            Key::Char('a'),
            Key::F(12),
            Key::Numpad(5),
            Key::Space,
            Key::PrintScreen,
            Key::Up,
        ] {
            assert!(to_code(key).is_ok(), "{key} should map to a key code");
        }
    }

    #[test]
    fn maps_modifiers() {
        let binding: Hotkey = "Ctrl+Shift+F9".parse().expect("valid");
        let hotkey = to_hotkey(binding).expect("mapped");
        assert_eq!(hotkey.mods, Modifiers::CONTROL | Modifiers::SHIFT);
        assert_eq!(hotkey.key, Code::F9);
    }
}
