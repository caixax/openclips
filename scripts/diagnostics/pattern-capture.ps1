# Captures a moving test pattern on the second display with the capture
# example and scores repeated frames. Variants: -Api dxgi|wgc, -SrcFps N
# (rate asked from the source), -Queue 1, -Mmcss 1, -Snow (present bound
# GPU pattern instead of the CPU ball), -Fullscreen (exclusive fullscreen
# presenter), -NoLoad (skip the GPU hog), -Seconds N. Assumes the second
# display sits at -1920,0 and is \\.\DISPLAY2; adjust below otherwise.
#
#   scripts\diagnostics\pattern-capture.ps1 -Api wgc -Snow -Fullscreen -Seconds 60
param(
    [string]$Api = "wgc",
    [int]$SrcFps = 60,
    [string]$Out = "pattern.mp4",
    [int]$Seconds = 12,
    [switch]$NoLoad,
    [switch]$Fullscreen,
    [switch]$Snow,
    [int]$Queue = 0,
    [int]$Mmcss = 0
)
$env:Path = "C:\Program Files\gstreamer\1.0\msvc_x86_64\bin;" + $env:Path
Add-Type -Namespace Win32 -Name Diag -MemberDefinition @'
public delegate bool EnumProc(IntPtr h, IntPtr l);
[DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, System.Text.StringBuilder s, int n);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
[DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);
public static IntPtr FindForPid(uint want) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => {
        uint pid; GetWindowThreadProcessId(h, out pid);
        if (pid != want || !IsWindowVisible(h)) return true;
        var sb = new System.Text.StringBuilder(256); GetClassNameW(h, sb, 256);
        if (!sb.ToString().StartsWith("Console")) { found = h; return false; }
        return true;
    }, IntPtr.Zero);
    return found;
}
'@
$scratch = Join-Path $env:TEMP "openclips-diag"
New-Item -ItemType Directory -Force $scratch | Out-Null
$pattern = "videotestsrc pattern=ball is-live=true ! video/x-raw,framerate=240/1,width=640,height=360 ! d3d11videosink fullscreen-toggle-mode=alt-enter"
if ($Snow) { $pattern = "d3d11testsrc pattern=snow ! video/x-raw(memory:D3D11Memory),width=1920,height=1080,framerate=1000/1 ! d3d11videosink sync=false fullscreen-toggle-mode=alt-enter" }
$presenter = Start-Process -FilePath "gst-launch-1.0.exe" -ArgumentList $pattern -PassThru
Start-Sleep -Seconds 5
$h = [Win32.Diag]::FindForPid([uint32]$presenter.Id)
[Win32.Diag]::SetWindowPos($h, [IntPtr]0, -1920, 0, 1920, 1080, 0x0040) | Out-Null
[Win32.Diag]::PostMessageW($h, 0x112, [IntPtr]0xF030, [IntPtr]0) | Out-Null
if ($Fullscreen) {
    Start-Sleep -Seconds 1
    [Win32.Diag]::PostMessageW($h, 0x104, [IntPtr]0x0D, [IntPtr]0x20000001) | Out-Null
    Start-Sleep -Milliseconds 100
    [Win32.Diag]::PostMessageW($h, 0x105, [IntPtr]0x0D, [IntPtr]0xE0000001) | Out-Null
    Start-Sleep -Seconds 2
}
$hog = $null
if (-not $NoLoad) {
    $hog = Start-Process -FilePath "gst-launch-1.0.exe" -ArgumentList "-q d3d11testsrc pattern=snow ! video/x-raw(memory:D3D11Memory),width=3840,height=2160,framerate=2000/1 ! d3d11convert ! video/x-raw(memory:D3D11Memory),width=7680,height=4320 ! d3d11convert ! video/x-raw(memory:D3D11Memory),width=1920,height=1080 ! fakesink sync=false" -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 4
}
$env:OPENCLIPS_MONITOR = [string][char]92 + [char]92 + '.' + [char]92 + 'DISPLAY2'
$env:OPENCLIPS_API = $Api
$env:OPENCLIPS_SOURCE_FPS = $SrcFps
$env:OPENCLIPS_QUEUE = $Queue
$env:OPENCLIPS_MMCSS = $Mmcss
Set-Location (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))
$job = Start-Job -ScriptBlock { Start-Sleep 5; nvidia-smi --query-gpu=utilization.gpu --format=csv }
cargo run -q -p openclips-capture --example record_check -- $Seconds "$scratch\$Out" finish 2>&1 | Select-String -Pattern "finished|error|-> video" | ForEach-Object { $_.Line }
"gpu during capture: " + ((Receive-Job -Job $job -Wait) -join " ")
if ($hog) { Stop-Process -Id $hog.Id -Force }
Stop-Process -Id $presenter.Id -Force
python "$PSScriptRoot\dups.py" "$scratch\$Out"
