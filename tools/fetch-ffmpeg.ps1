<#
.SYNOPSIS
  Downloads the pinned BtbN/FFmpeg-Builds LGPL shared win64 FFmpeg, checks its SHA-256 and unpacks it to third_party/ffmpeg/.

.DESCRIPTION
  - The release is pinned (dated autobuild tag and exact file name); "latest" is never used.
  - The archive must match the hardcoded SHA-256, otherwise the script deletes it and fails (fail closed).
  - Only LGPL builds are used (ARCHITECTURE section 14). GPL-only encoders are never linked.
  - Nothing from the archive is executed except `ffmpeg.exe -hide_banner -encoders` to check for hevc_qsv.
  - No administrator rights needed. It does not change environment variables: it prints the command to run.
  - If third_party/ffmpeg/bin/ffmpeg.exe already exists, nothing is downloaded; only the encoder check runs.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\fetch-ffmpeg.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

# Pinned build: BtbN autobuild of 2026-11-19 (FFmpeg 8.1 line), LGPL shared, win64.
# To bump: pick a new dated release, copy its asset name and SHA-256 (from the release page or
# `gh api repos/FFmpeg-Builds/FFmpeg-Builds/releases/tags/<tag>`), and update this block in the same PR.
$Tag      = 'autobuild-2026-11-19-12-39'
$Asset    = 'ffmpeg-n8.1.3-36-ge22a4510c9-win64-lgpl-shared-8.1.zip'
$Sha256   = 'ae947c6f7f87d19cf3de80e448409419a9e81a18987d55ead133c55c2db29e9c'
$Url      = "https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/download/$Tag/$Asset"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$Dest     = Join-Path $RepoRoot 'third_party\ffmpeg'
$Ffmpeg   = Join-Path $Dest 'bin\ffmpeg.exe'

# GitHub requires TLS 1.2; Windows PowerShell 5.1 may default to older protocols.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$ProgressPreference = 'SilentlyContinue'

if (-not (Test-Path $Ffmpeg)) {
    # Fresh, empty working directory for the download and the unpacked archive.
    $Work = Join-Path ([IO.Path]::GetTempPath()) ('vetro-ffmpeg-' + [guid]::NewGuid().ToString('N'))
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
        if (Test-Path $Dest) { throw "$Dest exists without bin\ffmpeg.exe. Delete that folder and run again." }
        New-Item -ItemType Directory -Path (Split-Path -Parent $Dest) -Force | Out-Null
        Move-Item -Path $Inner[0].FullName -Destination $Dest
    }
    finally {
        Remove-Item -Recurse -Force -Path $Work -ErrorAction SilentlyContinue
    }
} else {
    Write-Host "Already installed: $Ffmpeg (not downloaded again)."
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
