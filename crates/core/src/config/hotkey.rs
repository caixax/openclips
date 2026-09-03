use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// Modifier keys of a hotkey binding. Order and duplicates in the textual
/// form are irrelevant; the parsed value is canonical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
        super_key: false,
    };
    pub const ALT: Self = Self {
        alt: true,
        ..Self::NONE
    };
    pub const CTRL: Self = Self {
        ctrl: true,
        ..Self::NONE
    };
    pub const SHIFT: Self = Self {
        shift: true,
        ..Self::NONE
    };

    pub fn is_empty(self) -> bool {
        self == Self::NONE
    }
}

/// A platform neutral key identifier. Backends translate these into OS
/// specific virtual key codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    F(u8),
    Numpad(u8),
    Space,
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    PrintScreen,
    ScrollLock,
    Pause,
}

impl Key {
    fn parse(token: &str) -> Option<Self> {
        let upper = token.to_ascii_uppercase();
        let key = match upper.as_str() {
            "SPACE" => Key::Space,
            "ENTER" | "RETURN" => Key::Enter,
            "ESC" | "ESCAPE" => Key::Escape,
            "TAB" => Key::Tab,
            "BACKSPACE" => Key::Backspace,
            "DELETE" | "DEL" => Key::Delete,
            "INSERT" | "INS" => Key::Insert,
            "HOME" => Key::Home,
            "END" => Key::End,
            "PAGEUP" | "PGUP" => Key::PageUp,
            "PAGEDOWN" | "PGDN" => Key::PageDown,
            "UP" => Key::Up,
            "DOWN" => Key::Down,
            "LEFT" => Key::Left,
            "RIGHT" => Key::Right,
            "PRINTSCREEN" | "PRTSC" => Key::PrintScreen,
            "SCROLLLOCK" => Key::ScrollLock,
            "PAUSE" => Key::Pause,
            _ => {
                if let Some(n) = upper.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
                    return (1..=24).contains(&n).then_some(Key::F(n));
                }
                if let Some(n) = upper
                    .strip_prefix("NUMPAD")
                    .and_then(|n| n.parse::<u8>().ok())
                {
                    return (n <= 9).then_some(Key::Numpad(n));
                }
                let mut chars = upper.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_alphanumeric() => Key::Char(c),
                    _ => return None,
                }
            }
        };
        Some(key)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Char(c) => write!(f, "{c}"),
            Key::F(n) => write!(f, "F{n}"),
            Key::Numpad(n) => write!(f, "Numpad{n}"),
            Key::Space => f.write_str("Space"),
            Key::Enter => f.write_str("Enter"),
            Key::Escape => f.write_str("Escape"),
            Key::Tab => f.write_str("Tab"),
            Key::Backspace => f.write_str("Backspace"),
            Key::Delete => f.write_str("Delete"),
            Key::Insert => f.write_str("Insert"),
            Key::Home => f.write_str("Home"),
            Key::End => f.write_str("End"),
            Key::PageUp => f.write_str("PageUp"),
            Key::PageDown => f.write_str("PageDown"),
            Key::Up => f.write_str("Up"),
            Key::Down => f.write_str("Down"),
            Key::Left => f.write_str("Left"),
            Key::Right => f.write_str("Right"),
            Key::PrintScreen => f.write_str("PrintScreen"),
            Key::ScrollLock => f.write_str("ScrollLock"),
            Key::Pause => f.write_str("Pause"),
        }
    }
}

/// A global hotkey such as `Alt+8`. Serialized as its textual form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub modifiers: Modifiers,
    pub key: Key,
}

impl Hotkey {
    pub const fn new(modifiers: Modifiers, key: Key) -> Self {
        Self { modifiers, key }
    }
}

impl FromStr for Hotkey {
    type Err = CoreError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let invalid = |reason: &str| CoreError::InvalidHotkey {
            input: input.to_owned(),
            reason: reason.to_owned(),
        };

        let mut modifiers = Modifiers::NONE;
        let mut key = None;
        for token in input.split('+').map(str::trim) {
            if token.is_empty() {
                return Err(invalid("empty token"));
            }
            match token.to_ascii_uppercase().as_str() {
                "CTRL" | "CONTROL" => modifiers.ctrl = true,
                "ALT" => modifiers.alt = true,
                "SHIFT" => modifiers.shift = true,
                "SUPER" | "WIN" | "META" | "CMD" => modifiers.super_key = true,
                _ => {
                    if key.is_some() {
                        return Err(invalid("more than one non modifier key"));
                    }
                    key = Some(Key::parse(token).ok_or_else(|| invalid("unknown key"))?);
                }
            }
        }
        let key = key.ok_or_else(|| invalid("missing key"))?;
        Ok(Self { modifiers, key })
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.modifiers.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.modifiers.alt {
            f.write_str("Alt+")?;
        }
        if self.modifiers.shift {
            f.write_str("Shift+")?;
        }
        if self.modifiers.super_key {
            f.write_str("Super+")?;
        }
        write!(f, "{}", self.key)
    }
}

impl Serialize for Hotkey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Hotkey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_binding() {
        let hk: Hotkey = "Alt+8".parse().expect("valid");
        assert_eq!(hk, Hotkey::new(Modifiers::ALT, Key::Char('8')));
    }

    #[test]
    fn parsing_is_case_and_order_insensitive() {
        let a: Hotkey = "shift+CTRL+f9".parse().expect("valid");
        let b: Hotkey = "Ctrl+Shift+F9".parse().expect("valid");
        assert_eq!(a, b);
        assert_eq!(b.to_string(), "Ctrl+Shift+F9");
    }

    #[test]
    fn display_round_trips() {
        for text in [
            "Alt+8",
            "Ctrl+Alt+Shift+Super+Numpad5",
            "F12",
            "Ctrl+Space",
            "Alt+PrintScreen",
        ] {
            let hk: Hotkey = text.parse().expect("valid");
            assert_eq!(hk.to_string(), text);
        }
    }

    #[test]
    fn rejects_malformed_input() {
        for text in [
            "",
            "Alt+",
            "Alt",
            "Ctrl+A+B",
            "Alt+F25",
            "Numpad12",
            "Alt+++8",
            "Alt+Bogus",
        ] {
            assert!(
                text.parse::<Hotkey>().is_err(),
                "{text:?} should be rejected"
            );
        }
    }

    #[test]
    fn serde_uses_textual_form() {
        #[derive(Serialize, Deserialize)]
        struct Wrapper {
            hk: Hotkey,
        }
        let wrapper = Wrapper {
            hk: "Ctrl+Alt+R".parse().expect("valid"),
        };
        let text = toml::to_string(&wrapper).expect("serialize");
        assert_eq!(text.trim(), "hk = \"Ctrl+Alt+R\"");
        let back: Wrapper = toml::from_str(&text).expect("deserialize");
        assert_eq!(back.hk, wrapper.hk);
    }
}
