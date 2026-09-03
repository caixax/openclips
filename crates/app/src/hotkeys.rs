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

impl HotkeyAction {
    pub const ALL: [HotkeyAction; 3] = [
        HotkeyAction::SaveReplay,
        HotkeyAction::ToggleReplayBuffer,
        HotkeyAction::ToggleRecording,
    ];

    pub fn from_index(index: i32) -> Option<Self> {
        usize::try_from(index)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
    }

    pub fn binding(self, config: &HotkeyConfig) -> Hotkey {
        match self {
            HotkeyAction::SaveReplay => config.save_replay,
            HotkeyAction::ToggleReplayBuffer => config.toggle_replay_buffer,
            HotkeyAction::ToggleRecording => config.toggle_recording,
        }
    }
}

pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    registered: Vec<HotKey>,
    /// Bindings the OS refused, with a user facing reason.
    pub rejected: Vec<(HotkeyAction, String)>,
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        for hotkey in self.registered.drain(..) {
            let _ = self.manager.unregister(hotkey);
        }
    }
}

type Dispatch = Box<dyn Fn(HotkeyAction)>;

thread_local! {
    static DISPATCH: RefCell<Option<Dispatch>> = const { RefCell::new(None) };
    static BINDINGS: RefCell<HashMap<u32, HotkeyAction>> = RefCell::new(HashMap::new());
}

/// Installs the dispatcher that receives presses on the UI thread. Call once.
pub fn install_dispatch(dispatch: impl Fn(HotkeyAction) + 'static) {
    DISPATCH.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(dispatch));
    });
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state() != HotKeyState::Pressed {
            return;
        }
        let queued = slint::invoke_from_event_loop(move || {
            let action = BINDINGS.with(|b| b.borrow().get(&event.id()).copied());
            let Some(action) = action else {
                return;
            };
            info!("hotkey pressed: {action:?}");
            DISPATCH.with(|slot| {
                if let Some(dispatch) = slot.borrow().as_ref() {
                    dispatch(action);
                }
            });
        });
        if let Err(err) = queued {
            warn!("could not dispatch hotkey: {err}");
        }
    }));
}

/// Registers every binding from the config. Bindings the OS refuses
/// (typically because another application owns them) are reported and
/// skipped rather than failing. Drop the previous `Hotkeys` first when
/// re-registering.
pub fn register(config: &HotkeyConfig) -> Result<Hotkeys, AppError> {
    let manager = GlobalHotKeyManager::new().map_err(|e| AppError::Hotkey(e.to_string()))?;
    let mut hotkeys = Hotkeys {
        manager,
        registered: Vec::new(),
        rejected: Vec::new(),
    };
    let mut ids: HashMap<u32, HotkeyAction> = HashMap::new();
    for action in HotkeyAction::ALL {
        let binding = action.binding(config);
        let hotkey = match to_hotkey(binding) {
            Ok(hotkey) => hotkey,
            Err(err) => {
                hotkeys.rejected.push((action, err.to_string()));
                continue;
            }
        };
        match hotkeys.manager.register(hotkey) {
            Ok(()) => {
                info!("registered {binding} for {action:?}");
                hotkeys.registered.push(hotkey);
                ids.insert(hotkey.id(), action);
            }
            Err(err) => {
                warn!("could not register {binding} for {action:?}: {err}");
                hotkeys.rejected.push((
                    action,
                    format!("{binding} is already in use by another application"),
                ));
            }
        }
    }
    BINDINGS.with(|b| *b.borrow_mut() = ids);
    Ok(hotkeys)
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

/// Modifier state of a key press as reported by the UI toolkit.
#[derive(Debug, Clone, Copy, Default)]
pub struct PressedModifiers {
    pub alt: bool,
    pub control: bool,
    pub shift: bool,
    pub meta: bool,
}

/// Turns a key press captured in the settings UI into a binding. `text` is
/// the Slint key text: a printable character, or one of the private use
/// characters that stand for special keys. Modifier-only presses and keys
/// that cannot be global hotkeys yield `None`.
pub fn hotkey_from_press(text: &str, mods: PressedModifiers) -> Option<Hotkey> {
    use openclips_core::config::Modifiers as M;
    use slint::platform::Key as SK;

    let ch = text.chars().next()?;
    let special: [(SK, Key); 16] = [
        (SK::Return, Key::Enter),
        (SK::Escape, Key::Escape),
        (SK::Tab, Key::Tab),
        (SK::Backspace, Key::Backspace),
        (SK::Delete, Key::Delete),
        (SK::Insert, Key::Insert),
        (SK::Home, Key::Home),
        (SK::End, Key::End),
        (SK::PageUp, Key::PageUp),
        (SK::PageDown, Key::PageDown),
        (SK::UpArrow, Key::Up),
        (SK::DownArrow, Key::Down),
        (SK::LeftArrow, Key::Left),
        (SK::RightArrow, Key::Right),
        (SK::ScrollLock, Key::ScrollLock),
        (SK::Pause, Key::Pause),
    ];
    let key = if ch == ' ' {
        Key::Space
    } else if ch.is_ascii_alphanumeric() {
        Key::Char(ch.to_ascii_uppercase())
    } else if let Some((_, key)) = special.iter().find(|(sk, _)| char::from(*sk) == ch) {
        *key
    } else {
        Key::F(function_key_number(ch)?)
    };
    let modifiers = M {
        ctrl: mods.control,
        alt: mods.alt,
        shift: mods.shift,
        super_key: mods.meta,
    };
    Some(Hotkey::new(modifiers, key))
}

fn function_key_number(c: char) -> Option<u8> {
    use slint::platform::Key as SK;
    const KEYS: [SK; 24] = [
        SK::F1,
        SK::F2,
        SK::F3,
        SK::F4,
        SK::F5,
        SK::F6,
        SK::F7,
        SK::F8,
        SK::F9,
        SK::F10,
        SK::F11,
        SK::F12,
        SK::F13,
        SK::F14,
        SK::F15,
        SK::F16,
        SK::F17,
        SK::F18,
        SK::F19,
        SK::F20,
        SK::F21,
        SK::F22,
        SK::F23,
        SK::F24,
    ];
    KEYS.iter()
        .position(|k| char::from(*k) == c)
        .map(|i| i as u8 + 1)
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

    #[test]
    fn captures_presses_from_the_ui() {
        let alt = PressedModifiers {
            alt: true,
            ..Default::default()
        };
        let text = |h: Option<Hotkey>| h.map(|h| h.to_string());
        assert_eq!(text(hotkey_from_press("8", alt)), Some("Alt+8".to_owned()));
        assert_eq!(
            text(hotkey_from_press("q", PressedModifiers::default())),
            Some("Q".to_owned())
        );
        let f5 = char::from(slint::platform::Key::F5).to_string();
        assert_eq!(
            text(hotkey_from_press(&f5, PressedModifiers::default())),
            Some("F5".to_owned())
        );
        let escape = char::from(slint::platform::Key::Escape).to_string();
        assert_eq!(
            text(hotkey_from_press(&escape, PressedModifiers::default())),
            Some("Escape".to_owned())
        );
        let shift_only = char::from(slint::platform::Key::Shift).to_string();
        assert!(hotkey_from_press(&shift_only, PressedModifiers::default()).is_none());
        assert!(hotkey_from_press("", alt).is_none());
    }
}
