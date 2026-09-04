# Points pkg-config at the MSVC x86_64 GStreamer install so the *-sys crates
# build, wherever GStreamer lives. Dot-source this before running cargo. It
# only sets a variable that is not already set, so an explicit environment
# wins. Mirrors the runtime search in crates/app/src/bootstrap.rs, plus the
# flat per-user layout the Inno installer uses (bin and lib directly under the
# root).

$roots = @()
if ($env:GSTREAMER_1_0_ROOT_MSVC_X86_64) { $roots += $env:GSTREAMER_1_0_ROOT_MSVC_X86_64 }
$roots += "C:\Program Files\gstreamer\1.0\msvc_x86_64"
$roots += "C:\gstreamer\1.0\msvc_x86_64"
$roots += "C:\gstreamer"

$root = $roots | Where-Object {
    (Test-Path (Join-Path $_ "bin\pkg-config.exe")) -and
    (Test-Path (Join-Path $_ "lib\pkgconfig\gstreamer-1.0.pc"))
} | Select-Object -First 1

if (-not $root) {
    Write-Warning "GStreamer MSVC x86_64 development files not found; cargo build may fail. Install from https://gstreamer.freedesktop.org/download/ or set GSTREAMER_1_0_ROOT_MSVC_X86_64."
    return
}

if (-not $env:GSTREAMER_1_0_ROOT_MSVC_X86_64) { $env:GSTREAMER_1_0_ROOT_MSVC_X86_64 = $root }
if (-not $env:PKG_CONFIG) { $env:PKG_CONFIG = Join-Path $root "bin\pkg-config.exe" }
if (-not $env:PKG_CONFIG_PATH) { $env:PKG_CONFIG_PATH = Join-Path $root "lib\pkgconfig" }
Write-Host "Using GStreamer at $root" -ForegroundColor DarkGray
