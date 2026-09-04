//! "Launch on startup" through the per user Run key, plus the values the
//! installer leaves for the app. Nothing is written to the Run key unless
//! the user turns the option on, and turning it off removes the value again.

use openclips_core::config::Language;

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const APP_KEY: &str = r"Software\OpenClips";
const VALUE_NAME: &str = "OpenClips";

/// Makes the registry match the setting. `minimized` decides whether the
/// startup launch opens in the tray. Errors are returned as text for the
/// settings page; they never stop the app.
pub fn apply(enabled: bool, minimized: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let flag = if minimized { " --minimized" } else { "" };
            let command = format!("\"{}\"{flag}", exe.display());
            registry::set_run_value(RUN_KEY, VALUE_NAME, &command)
        } else {
            registry::delete_run_value(RUN_KEY, VALUE_NAME)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (enabled, minimized);
        Err("launch on startup is only implemented on Windows".to_owned())
    }
}

/// The language chosen in the installer, if it left one behind.
pub fn installer_language() -> Option<Language> {
    #[cfg(windows)]
    {
        registry::read_string(APP_KEY, "Language").and_then(|code| Language::from_code(&code))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Whether the Run value currently exists.
pub fn is_enabled() -> bool {
    #[cfg(windows)]
    {
        registry::run_value_exists(RUN_KEY, VALUE_NAME)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
mod registry {
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ, REG_VALUE_TYPE, RegCloseKey,
        RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };
    use windows::core::PCWSTR;

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle was opened by RegOpenKeyExW and is closed once.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn open(
        path: &str,
        access: windows::Win32::System::Registry::REG_SAM_FLAGS,
    ) -> Result<Key, String> {
        let mut key = HKEY::default();
        let path = wide(path);
        // SAFETY: `path` is null terminated and `key` receives the handle.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                None,
                access,
                &mut key,
            )
        };
        status
            .ok()
            .map_err(|e| format!("could not open the Run key: {e}"))?;
        Ok(Key(key))
    }

    pub fn set_run_value(path: &str, name: &str, command: &str) -> Result<(), String> {
        let key = open(path, KEY_SET_VALUE)?;
        let name = wide(name);
        let value = wide(command);
        let bytes: &[u8] = bytemuck_cast(&value);
        // SAFETY: all buffers are valid for the duration of the call.
        let status =
            unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes)) };
        status
            .ok()
            .map_err(|e| format!("could not write the Run value: {e}"))
    }

    pub fn delete_run_value(path: &str, name: &str) -> Result<(), String> {
        let key = open(path, KEY_SET_VALUE)?;
        let name = wide(name);
        // SAFETY: `name` is null terminated.
        let status = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
        if status.is_ok() || status.0 == 2 {
            Ok(())
        } else {
            Err(format!("could not remove the Run value: {}", status.0))
        }
    }

    pub fn read_string(path: &str, name: &str) -> Option<String> {
        let key = open(path, KEY_READ).ok()?;
        let name = wide(name);
        let mut kind = REG_VALUE_TYPE::default();
        let mut buffer = vec![0u16; 256];
        let mut size = (buffer.len() * 2) as u32;
        // SAFETY: `size` holds the byte length of `buffer`, which stays
        // alive for the call; the API writes at most that many bytes.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut kind),
                Some(buffer.as_mut_ptr() as *mut u8),
                Some(&mut size),
            )
        };
        if status.is_err() || kind != REG_SZ {
            return None;
        }
        let len = (size as usize / 2).min(buffer.len());
        let text = String::from_utf16_lossy(&buffer[..len]);
        Some(text.trim_end_matches('\0').to_owned())
    }

    pub fn run_value_exists(path: &str, name: &str) -> bool {
        let Ok(key) = open(path, KEY_READ) else {
            return false;
        };
        let name = wide(name);
        // SAFETY: querying without buffers only reports existence and size.
        let status =
            unsafe { RegQueryValueExW(key.0, PCWSTR(name.as_ptr()), None, None, None, None) };
        status.is_ok()
    }

    /// Views UTF-16 code units as bytes for the registry API.
    fn bytemuck_cast(value: &[u16]) -> &[u8] {
        // SAFETY: u16 has no padding and the slice is valid for reads; the
        // byte view has exactly twice the length.
        unsafe { std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2) }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::registry;

    #[test]
    fn run_value_round_trip() {
        let name = "OpenClipsTestValue";
        registry::set_run_value(super::RUN_KEY, name, "\"x.exe\" --minimized").expect("set");
        assert!(registry::run_value_exists(super::RUN_KEY, name));
        registry::delete_run_value(super::RUN_KEY, name).expect("delete");
        assert!(!registry::run_value_exists(super::RUN_KEY, name));
        registry::delete_run_value(super::RUN_KEY, name).expect("deleting twice is fine");
    }
}
