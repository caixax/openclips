# Records the primary display (play a game meanwhile) with each pipeline
# variant back to back and reports repeated frames and GPU load. Compare
# with a clip saved by the app over the same seconds.
#
#   scripts\diagnostics\game-ab.ps1 -Seconds 15
param([int]$Seconds = 15)
$env:Path = "C:\Program Files\gstreamer\1.0\msvc_x86_64\bin;" + $env:Path
$scratch = Join-Path $env:TEMP "openclips-diag"
New-Item -ItemType Directory -Force $scratch | Out-Null
Set-Location (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))
Remove-Item Env:OPENCLIPS_MONITOR -ErrorAction SilentlyContinue
$variants = @(
    @{ name = "base-wgc";  api = "wgc";  src = 60;  queue = 0; mmcss = 0 },
    @{ name = "src239";    api = "wgc";  src = 239; queue = 0; mmcss = 0 },
    @{ name = "queue";     api = "wgc";  src = 60;  queue = 1; mmcss = 0 },
    @{ name = "mmcss";     api = "wgc";  src = 60;  queue = 0; mmcss = 1 },
    @{ name = "all";       api = "wgc";  src = 239; queue = 1; mmcss = 1 },
    @{ name = "base-dxgi"; api = "dxgi"; src = 60;  queue = 0; mmcss = 0 }
)
foreach ($v in $variants) {
    $env:OPENCLIPS_API = $v.api
    $env:OPENCLIPS_SOURCE_FPS = $v.src
    $env:OPENCLIPS_QUEUE = $v.queue
    $env:OPENCLIPS_MMCSS = $v.mmcss
    $out = "$scratch\ab-$($v.name).mp4"
    $job = Start-Job -ScriptBlock { Start-Sleep 6; (nvidia-smi --query-gpu=utilization.gpu --format=csv,noheader) }
    cargo run -q -p openclips-capture --example record_check -- $Seconds $out finish 2>&1 | Select-String -Pattern "start capture|panick" | ForEach-Object { $_.Line }
    $gpu = (Receive-Job -Job $job -Wait) -join " "
    $result = python "$PSScriptRoot\dups.py" $out 2>&1 | Select-Object -Last 1
    "$($v.name): gpu=$gpu | $result"
}
