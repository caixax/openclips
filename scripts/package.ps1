<#
.SYNOPSIS
Builds a release binary and stages a distributable folder and zip.

.DESCRIPTION
Produces dist\OpenClips-<version>-win64\ with the executable, README and
LICENSE, and zips it. With -BundleRuntime the GStreamer runtime found on
this machine is copied next to the executable so the zip runs on a PC
without GStreamer installed (the app looks for gstreamer\bin beside the
executable first).

.PARAMETER Installer
Also compiles packaging\openclips.nsi with makensis (NSIS 3 must be installed).

.PARAMETER BundleRuntime
Copy the GStreamer runtime (bin and plugins) into the staged folder.

.PARAMETER GStreamerRoot
Location of the MSVC x86_64 GStreamer install. Defaults to the value of
GSTREAMER_1_0_ROOT_MSVC_X86_64 or C:\Program Files\gstreamer\1.0\msvc_x86_64.
#>
param(
    [switch]$BundleRuntime,
    [switch]$Installer,
    [string]$GStreamerRoot = ""
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

# Make cargo find GStreamer wherever it is installed on this machine (also
# sets GSTREAMER_1_0_ROOT_MSVC_X86_64, used below to bundle the runtime).
. (Join-Path $PSScriptRoot "gstreamer-env.ps1")

$version = (Select-String -Path "Cargo.toml" -Pattern '^version = "([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$name = "OpenClips-$version-win64"
$stage = Join-Path $repo "dist\$name"

if ($GStreamerRoot -eq "") {
    $GStreamerRoot = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64
}
if ($GStreamerRoot -eq "" -or $null -eq $GStreamerRoot) {
    $GStreamerRoot = "C:\Program Files\gstreamer\1.0\msvc_x86_64"
}

Write-Host "Building release binary..."
cargo build --release --workspace
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "target\release\openclips.exe" $stage
Copy-Item "README.md", "LICENSE" $stage

# The signed OBS capture hooks for game capture. Kept in a folder the app
# looks for beside the executable.
$hooks = Join-Path $repo "third_party\obs-capture"
if (Test-Path (Join-Path $hooks "graphics-hook64.dll")) {
    $hooksTarget = Join-Path $stage "obs-capture"
    New-Item -ItemType Directory -Force $hooksTarget | Out-Null
    Copy-Item (Join-Path $hooks "*.dll") $hooksTarget
    Copy-Item (Join-Path $hooks "*.exe") $hooksTarget
    Copy-Item (Join-Path $hooks "COPYING"), (Join-Path $hooks "README.md") $hooksTarget
} else {
    Write-Warning "OBS capture hooks not found at $hooks; game capture will be unavailable in this build"
}

if ($BundleRuntime) {
    $bin = Join-Path $GStreamerRoot "bin"
    $plugins = Join-Path $GStreamerRoot "lib\gstreamer-1.0"
    if (-not (Test-Path (Join-Path $bin "gstreamer-1.0-0.dll"))) {
        throw "GStreamer runtime not found at $GStreamerRoot"
    }
    Write-Host "Bundling GStreamer runtime from $GStreamerRoot..."
    $target = Join-Path $stage "gstreamer"
    New-Item -ItemType Directory -Force (Join-Path $target "bin") | Out-Null
    New-Item -ItemType Directory -Force (Join-Path $target "lib\gstreamer-1.0") | Out-Null
    Copy-Item (Join-Path $bin "*.dll") (Join-Path $target "bin")
    Copy-Item (Join-Path $plugins "*.dll") (Join-Path $target "lib\gstreamer-1.0")
    # The plugin scanner helper keeps a broken plugin from taking the app down.
    $scanner = Join-Path $GStreamerRoot "libexec\gstreamer-1.0\gst-plugin-scanner.exe"
    if (Test-Path $scanner) {
        New-Item -ItemType Directory -Force (Join-Path $target "libexec\gstreamer-1.0") | Out-Null
        Copy-Item $scanner (Join-Path $target "libexec\gstreamer-1.0")
    }
}

$zip = Join-Path $repo "dist\$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path "$stage\*" -DestinationPath $zip
Write-Host "Staged $stage"
Write-Host "Wrote $zip"

if ($Installer) {
    $makensis = Get-Command makensis -ErrorAction SilentlyContinue
    if (-not $makensis) {
        $candidate = Join-Path ${env:ProgramFiles(x86)} "NSIS\makensis.exe"
        if (Test-Path $candidate) { $makensis = $candidate } else { throw "makensis was not found; install NSIS 3" }
    } else {
        $makensis = $makensis.Source
    }
    & $makensis "/DVERSION=$version" (Join-Path $repo "packaging\openclips.nsi")
    if ($LASTEXITCODE -ne 0) { throw "makensis failed" }
    Write-Host "Wrote $(Join-Path $repo "dist\OpenClips-$version-setup.exe")"
}
