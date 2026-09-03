//! Running process enumeration through the tool help snapshot, plus the
//! foreground window's process so the active game can be told apart.

use std::mem::size_of;
use std::path::PathBuf;

use openclips_core::games::RunningProcess;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::PWSTR;

use crate::backend::ProcessWatcher;
use crate::error::CaptureError;

pub struct ToolHelpWatcher;

fn foreground_pid() -> u32 {
    // SAFETY: plain Win32 calls with valid arguments.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return 0;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid
    }
}

fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

impl ProcessWatcher for ToolHelpWatcher {
    fn running(&self) -> Result<Vec<RunningProcess>, CaptureError> {
        let foreground = foreground_pid();
        // SAFETY: the snapshot handle is closed before returning and the
        // entry struct is sized correctly for the API.
        unsafe {
            let snapshot: HANDLE =
                CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(|e| {
                    CaptureError::Pipeline {
                        message: format!("process snapshot failed: {e}"),
                        element: String::new(),
                    }
                })?;
            let mut entry = PROCESSENTRY32W {
                dwSize: size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut found = Vec::new();
            if Process32FirstW(snapshot, &mut entry).is_ok() {
                loop {
                    let exe = wide_to_string(&entry.szExeFile).to_lowercase();
                    if !exe.is_empty() {
                        found.push(RunningProcess {
                            pid: entry.th32ProcessID,
                            exe,
                            path: None,
                            foreground: entry.th32ProcessID == foreground,
                        });
                    }
                    if Process32NextW(snapshot, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snapshot);
            Ok(found)
        }
    }

    fn process_path(&self, pid: u32) -> Option<PathBuf> {
        // SAFETY: the process handle is closed before returning and the
        // buffer length is passed alongside the buffer.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buffer = vec![0u16; 1024];
            let mut len = buffer.len() as u32;
            let result = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut len,
            );
            let _ = CloseHandle(handle);
            result.ok()?;
            let path = String::from_utf16_lossy(&buffer[..len as usize]);
            (!path.is_empty()).then(|| PathBuf::from(path))
        }
    }
}
