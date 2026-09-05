//! Last resort crash reporting. An unhandled exception on any thread (the
//! GStreamer capture threads included) writes a minidump and a short text
//! note under the data directory before the process dies, so a crash in a
//! native plugin can be traced instead of guessed.

use std::path::{Path, PathBuf};

/// Installs the handler. Safe to call once, early in `main`.
pub fn install(data_dir: &Path) {
    #[cfg(windows)]
    imp::install(data_dir.join("crashes"));
    #[cfg(not(windows))]
    {
        let _ = data_dir;
    }
}

/// Where dumps go; shown in the log at startup so the user can find them.
pub fn dump_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("crashes")
}

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::io::Write;
    use std::os::windows::io::AsRawHandle;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use windows::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, HANDLE, HMODULE};
    use windows::Win32::System::Diagnostics::Debug::{
        EXCEPTION_EXECUTE_HANDLER, EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION,
        MINIDUMP_TYPE, MiniDumpNormal, MiniDumpWithIndirectlyReferencedMemory,
        MiniDumpWithThreadInfo, MiniDumpWriteDump, SetUnhandledExceptionFilter,
    };
    use windows::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
    };
    use windows::core::PCWSTR;

    static DIR: OnceLock<PathBuf> = OnceLock::new();

    pub fn install(dir: PathBuf) {
        let _ = DIR.set(dir);
        // SAFETY: the filter is a plain function that stays valid for the
        // life of the process.
        unsafe {
            SetUnhandledExceptionFilter(Some(filter));
        }
    }

    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        let Some(dir) = DIR.get() else {
            return EXCEPTION_EXECUTE_HANDLER;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return EXCEPTION_EXECUTE_HANDLER;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let base = dir.join(format!("openclips-{stamp}"));

        // SAFETY: `info` is the pointer Windows hands to the filter.
        let record = unsafe { info.as_ref() }.and_then(|p| unsafe { p.ExceptionRecord.as_ref() });
        if let Ok(mut note) = File::create(base.with_extension("txt")) {
            let _ = writeln!(note, "OpenClips {}", openclips_core::APP_VERSION);
            if let Some(record) = record {
                let address = record.ExceptionAddress as usize;
                let kind = if record.ExceptionCode == EXCEPTION_ACCESS_VIOLATION {
                    "access violation"
                } else {
                    "exception"
                };
                let _ = writeln!(
                    note,
                    "{kind} code 0x{:08x} at 0x{address:x}",
                    record.ExceptionCode.0 as u32
                );
                if let Some((module, offset)) = module_of(address) {
                    let _ = writeln!(note, "in {module} + 0x{offset:x}");
                }
            }
        }

        if let Ok(file) = File::create(base.with_extension("dmp")) {
            let exception = MINIDUMP_EXCEPTION_INFORMATION {
                // SAFETY: plain queries about the current thread.
                ThreadId: unsafe { GetCurrentThreadId() },
                ExceptionPointers: info as *mut EXCEPTION_POINTERS,
                ClientPointers: false.into(),
            };
            let kind = MINIDUMP_TYPE(
                MiniDumpNormal.0
                    | MiniDumpWithThreadInfo.0
                    | MiniDumpWithIndirectlyReferencedMemory.0,
            );
            // SAFETY: the file handle stays open until the call returns and
            // the exception structure points at data owned by the caller.
            unsafe {
                let _ = MiniDumpWriteDump(
                    GetCurrentProcess(),
                    GetCurrentProcessId(),
                    HANDLE(file.as_raw_handle()),
                    kind,
                    Some(&raw const exception),
                    None,
                    None,
                );
            }
        }
        EXCEPTION_EXECUTE_HANDLER
    }

    /// The module that contains `address` and the offset inside it, the
    /// same pair Windows Error Reporting shows.
    fn module_of(address: usize) -> Option<(String, usize)> {
        let mut module = HMODULE::default();
        // SAFETY: the address is only used as a lookup key; the refcount
        // flag keeps the module from being pinned.
        let ok = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                PCWSTR(address as *const u16),
                &mut module,
            )
        };
        if ok.is_err() {
            return None;
        }
        let mut name = [0u16; 512];
        // SAFETY: `name` is a valid buffer of the length passed.
        let len = unsafe { GetModuleFileNameW(Some(module), &mut name) } as usize;
        let path = String::from_utf16_lossy(&name[..len.min(name.len())]);
        let file = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned();
        Some((file, address - module.0 as usize))
    }
}
