<#
.SYNOPSIS
Cuts a release from this machine: the same steps as the Release workflow,
without waiting for GitHub runners.

.DESCRIPTION
Picks the next version (or the one given), writes it into Cargo.toml, runs
the tests, builds the portable zip and the NSIS installer with the GStreamer
runtime bundled, writes SHA256SUMS.txt, commits and tags vX.Y.Z, pushes, and
publishes the GitHub release with gh. Installed copies of the app pick the
release up on their next start.

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
    [switch]$NoPublish
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

Step "Writing checksums"
$lines = Get-ChildItem dist -File | Where-Object { $_.Extension -in ".exe", ".zip" } | ForEach-Object {
    "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)"
}
Set-Content dist/SHA256SUMS.txt ($lines -join "`n") -Encoding ascii
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
Run "gh release create v$next --title `"OpenClips v$next`" --generate-notes $($assets -join ' ')"
Write-Host "`nReleased v$next" -ForegroundColor Green
