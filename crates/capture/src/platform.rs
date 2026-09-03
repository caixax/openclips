/// Identifies the capture backend compiled into this binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Linux,
    Unsupported,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Platform::Windows
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::Unsupported
        }
    }

    /// Whether a capture backend exists for this platform in the current build.
    pub const fn has_backend(self) -> bool {
        matches!(self, Platform::Windows)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Platform::Windows => "Windows",
            Platform::Linux => "Linux",
            Platform::Unsupported => "unsupported",
        }
    }
}
