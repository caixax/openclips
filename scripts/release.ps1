<#
.SYNOPSIS
Cuts a release from this machine: the same steps as the Release workflow,
without waiting for GitHub runners.

.DESCRIPTION
Picks the next version (or the one given), writes it into Cargo.toml, runs
the tests, builds the portable zip and the NSIS installer with the GStreamer
runtime bundled, builds the Linux packages inside the WSL distros of this
machine, writes SHA256SUMS.txt, commits and tags vX.Y.Z, pushes, and
publishes the GitHub release with gh. Installed copies of the app pick the
release up on their next start.

The Linux packages come from scripts/linux/build.sh, run in one distro per
family at the same time: a .deb (and the generic tarball, because its glibc
is the oldest) on the first of -LinuxDistros, then an .rpm and a pacman
package. Prepare each distro once with scripts/linux/setup-wsl.sh. A failed
Linux build stops the release before anything is tagged.

.PARAMETER Patch
Bump the patch number (default when nothing else is given).

.PARAMETER Minor
Bump the minor number and reset patch.

.PARAMETER Major
Bump the major number and reset minor and patch.

.PARAMETER Version
Exact version to release, for example 0.3.0. Alias: -V.

.PARAMETER SkipTests
Do not run cargo test first.

.PARAMETER NoPublish
Build, commit and tag, but do not push or create the GitHub release.

.PARAMETER SkipLinux
Release Windows only.

.PARAMETER LinuxDistros
WSL distro names to build in. The first one also builds the tarball.

.EXAMPLE
scripts\release.ps1 -Patch
scripts\release.ps1 -V 0.3.0
#>
[CmdletBinding()]
param(
    [switch]$Patch,
    [switch]$Minor,
    [switch]$Major,
    [Alias("V")][string]$Version,
    [switch]$SkipTests,
    [switch]$NoPublish,
    [switch]$SkipLinux,
    [string[]]$LinuxDistros = @("Ubuntu-22.04", "FedoraLinux-43", "archlinux")
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

# Make cargo find GStreamer wherever it is installed on this machine.
. (Join-Path $PSScriptRoot "gstreamer-env.ps1")

function Step($text) { Write-Host "`n== $text" -ForegroundColor Cyan }
function Run($command) {
    Write-Host "> $command" -ForegroundColor DarkGray
    Invoke-Expression $command
    if ($LASTEXITCODE -ne 0) { throw "command failed: $command" }
}

Step "Checking the working tree"
if (git status --porcelain) { throw "the working tree has uncommitted changes; commit or stash them first" }
$branch = (git rev-parse --abbrev-ref HEAD).Trim()
if ($branch -ne "main") { throw "releases are cut from main (current branch: $branch)" }

Step "Picking the version"
$toml = Get-Content Cargo.toml -Raw
$current = [regex]::Match($toml, '(?m)^version = "([^"]+)"').Groups[1].Value
if ($Version) {
    if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "version must look like 1.2.3" }
    $next = $Version
} else {
    $parts = $current.Split('.') | ForEach-Object { [int]$_ }
    if ($Major) { $parts[0]++; $parts[1] = 0; $parts[2] = 0 }
    elseif ($Minor) { $parts[1]++; $parts[2] = 0 }
    else { $parts[2]++ }
    $next = "$($parts[0]).$($parts[1]).$($parts[2])"
}
if (git tag --list "v$next") { throw "tag v$next already exists" }
Write-Host "Releasing $current -> $next"
$toml = [regex]::Replace($toml, '(?m)^version = "[^"]+"', "version = `"$next`"", 1)
Set-Content Cargo.toml $toml -NoNewline -Encoding utf8
Run "cargo update --workspace"

if (-not $SkipTests) {
    Step "Running the tests"
    Run "cargo test --workspace"
}

Step "Building the portable zip and the installer"
if (Test-Path dist) { Remove-Item -Recurse -Force dist }
Run "powershell -ExecutionPolicy Bypass -File scripts\package.ps1 -BundleRuntime -Installer"

if (-not $SkipLinux) {
    Step "Building the Linux packages in WSL ($($LinuxDistros -join ', '))"
    # The checkout as WSL sees it: I:\Projects\openclips -> /mnt/i/Projects/openclips.
    $drive = $repo.Substring(0, 1).ToLower()
    $script = "/mnt/$drive" + ($repo.Substring(2) -replace '\\', '/') + "/scripts/linux/build.sh"
    New-Item -ItemType Directory -Force dist | Out-Null
    $jobs = @()
    for ($i = 0; $i -lt $LinuxDistros.Count; $i++) {
        $distro = $LinuxDistros[$i]
        $flag = if ($i -eq 0) { "--tarball" } else { "" }
        Write-Host "> wsl -d $distro -- bash $script $flag" -ForegroundColor DarkGray
        $jobs += Start-Job -Name $distro -ArgumentList $distro, $script, $flag -ScriptBlock {
            param($distro, $script, $flag)
            $output = if ($flag) { wsl.exe -d $distro -- bash $script $flag 2>&1 } else { wsl.exe -d $distro -- bash $script 2>&1 }
            [pscustomobject]@{ Code = $LASTEXITCODE; Tail = ($output | Select-Object -Last 25) -join "`n" }
        }
    }
    $failed = @()
    foreach ($job in $jobs) {
        $result = Receive-Job -Job $job -Wait -AutoRemoveJob
        if ($result.Code -ne 0) {
            $failed += $job.Name
            Write-Host "`n--- $($job.Name) failed:`n$($result.Tail)" -ForegroundColor Red
        } else {
            Write-Host "$($job.Name): ok" -ForegroundColor Green
        }
    }
    if ($failed.Count -gt 0) {
        # Nothing is committed yet; put the version back so a retry starts clean.
        git checkout -- Cargo.toml Cargo.lock
        throw "the Linux build failed in: $($failed -join ', ')"
    }
}

Step "Writing checksums"
$lines = Get-ChildItem dist -File | Where-Object { $_.Name -match '\.(exe|zip|deb|rpm|zst|gz)$' } | ForEach-Object {
    "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)"
}
# LF only, also at the end: a CR glued to the last file name breaks every
# Unix tool that reads this file (install.sh, sha256sum -c).
[IO.File]::WriteAllText((Join-Path $repo "dist\SHA256SUMS.txt"), (($lines -join "`n") + "`n"), [Text.Encoding]::ASCII)
Get-Content dist/SHA256SUMS.txt

Step "Committing and tagging v$next"
Run "git add Cargo.toml Cargo.lock"
Run "git commit -q -m `"chore: release v$next`""
Run "git tag v$next"

if ($NoPublish) {
    Write-Host "`nDone. Nothing was pushed (-NoPublish)." -ForegroundColor Yellow
    exit 0
}

Step "Pushing"
Run "git push origin main"
Run "git push origin v$next"

Step "Publishing the GitHub release"
$assets = @("dist/OpenClips-$next-setup.exe", "dist/OpenClips-$next-win64.zip", "dist/SHA256SUMS.txt")
if (-not $SkipLinux) {
    $assets += Get-ChildItem dist -File | Where-Object { $_.Name -match '\.(deb|rpm|zst|gz)$' } | ForEach-Object { "dist/$($_.Name)" }
}
Run "gh release create v$next --title `"OpenClips v$next`" --generate-notes $($assets -join ' ')"
Write-Host "`nReleased v$next" -ForegroundColor Green
