<#
.SYNOPSIS
  Downloads the pinned BtbN/FFmpeg-Builds LGPL shared win64 FFmpeg, checks its SHA-256 and unpacks it to third_party/ffmpeg/.

.DESCRIPTION
  - The release is pinned (dated autobuild tag and exact file name); "latest" is never used.
  - The archive must match the hardcoded SHA-256, otherwise the script deletes it and fails (fail closed).
  - Only LGPL builds are used (ARCHITECTURE section 14). GPL-only encoders are never linked.
  - Nothing from the archive is executed except `ffmpeg.exe -hide_banner -encoders` to check for hevc_qsv.
  - No administrator rights needed. It does not change environment variables: it prints the command to run.
  - third_party/ffmpeg/.pin holds the installed asset name. If it matches the pin, nothing is downloaded.
    If it is missing or differs, the build is downloaded again and replaces the old folder.
    A folder without bin\ffmpeg.exe is never touched: the script stops and asks you to delete it.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\fetch-ffmpeg.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

# Pinned build: BtbN autobuild of 2026-10-31 (FFmpeg 8.1 line), LGPL shared, win64.
# pin month-end builds only (kept 2 years; dailies 14)
# To bump: pick a new month-end release, copy its asset name and SHA-256 (from `gh api repos/FFmpeg-Builds/FFmpeg-Builds/releases/tags/<tag>`),
# confirm the SHA-256 after downloading, and update this block in the same PR.
$Tag      = 'autobuild-2026-10-31-12-59'
$Asset    = 'ffmpeg-n8.1.3-27-g8dca1586f4-win64-lgpl-shared-8.1.zip'
$Sha256   = '5cef7bc48e513f3cee7c881e8b2999d835541edb1846c3e89443a67f8f8556ab'
$Url      = "https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/download/$Tag/$Asset"

$RepoRoot   = Split-Path -Parent $PSScriptRoot
$ThirdParty = Join-Path $RepoRoot 'third_party'
$Dest       = Join-Path $ThirdParty 'ffmpeg'
$Ffmpeg     = Join-Path $Dest 'bin\ffmpeg.exe'
$Pin        = Join-Path $Dest '.pin'

# GitHub requires TLS 1.2; Windows PowerShell 5.1 may default to older protocols.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$ProgressPreference = 'SilentlyContinue'

$installedAsset = if (Test-Path $Pin) { (Get-Content -Path $Pin -Raw).Trim() } else { '' }

if ((Test-Path $Ffmpeg) -and ($installedAsset -eq $Asset)) {
    Write-Host "Already installed: $Ffmpeg (pin matches $Asset; not downloaded again)."
} else {
    # Checked before the download: an unknown folder is never deleted.
    if ((Test-Path $Dest) -and -not (Test-Path $Ffmpeg)) {
        throw "$Dest exists without bin\ffmpeg.exe. Delete that folder and run again."
    }
    New-Item -ItemType Directory -Path $ThirdParty -Force | Out-Null
    # Empty working folder inside third_party (same drive as the destination, so Move-Item works).
    $Work = Join-Path $ThirdParty ('download-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $Work | Out-Null
    try {
        $Zip = Join-Path $Work $Asset
        Write-Host "Downloading $Asset (about 80 MB) ..."
        Invoke-WebRequest -Uri $Url -OutFile $Zip -UseBasicParsing

        $Actual = (Get-FileHash -Algorithm SHA256 -Path $Zip).Hash.ToLowerInvariant()
        if ($Actual -ne $Sha256) {
            Remove-Item -Force $Zip
            throw "SHA-256 mismatch for $Asset. Expected $Sha256, got $Actual. Archive deleted, nothing installed."
        }
        Write-Host 'SHA-256 OK.'

        $Unpacked = Join-Path $Work 'unpacked'
        Expand-Archive -Path $Zip -DestinationPath $Unpacked

        # The archive has one top-level folder; its contents (bin, include, lib, ...) become third_party/ffmpeg.
        $Inner = @(Get-ChildItem -Path $Unpacked -Directory)
        if ($Inner.Count -ne 1) { throw "Unexpected archive layout: $($Inner.Count) top-level folders." }

        # Replace our earlier install of another build (it has bin\ffmpeg.exe, checked above).
        if (Test-Path $Dest) {
            Write-Host "Replacing $Dest (installed: '$installedAsset', pinned: '$Asset')."
            Remove-Item -Recurse -Force -Path $Dest
        }
        Move-Item -Path $Inner[0].FullName -Destination $Dest
        Set-Content -Path $Pin -Value $Asset -Encoding ascii
    }
    finally {
        Remove-Item -Recurse -Force -Path $Work -ErrorAction SilentlyContinue
    }
}

if (-not (Test-Path $Ffmpeg)) { throw "ffmpeg.exe not found at $Ffmpeg" }

# The only thing executed from the archive: list the encoders and look for HEVC Quick Sync.
$Encoders = & $Ffmpeg -hide_banner -encoders | Out-String
if ($LASTEXITCODE -ne 0) { throw "ffmpeg -encoders failed with exit code $LASTEXITCODE" }
if ($Encoders -notmatch '\bhevc_qsv\b') { throw 'hevc_qsv is missing from ffmpeg -encoders.' }
Write-Host 'hevc_qsv: present.'

Write-Host ''
Write-Host "FFmpeg installed in: $Dest"
Write-Host 'To let the build find it, run once in PowerShell (no admin needed):'
Write-Host "  [Environment]::SetEnvironmentVariable('FFMPEG_DIR', '$Dest', 'User')"
Write-Host 'Open a new terminal afterwards so the variable is visible.'
