<#
.SYNOPSIS
  M0-5 encode capacity spike: FFmpeg ddagrab on the VDD output -> hevc_qsv low-latency, with per-frame encode
  times, CPU/GPU load and clock/thermal samples. Also measures how often the virtual output really updates.

.DESCRIPTION
  -Mode encode : ddagrab (framerate -Fps, duplicates kept so the encoder sees a constant -Fps) -> hwmap to QSV
                 (zero-copy) -> hevc_qsv with async_depth=1, low_power=1, bf=0, low_delay_brc=1, VBV of one frame,
                 no periodic IDR. Per-frame encode time comes from -benchmark_all: with async_depth=1 each
                 avcodec_send_frame waits for that frame's encode, so send + receive is the frame's encode time.
  -Mode render : ddagrab with dup_frames=0 at 240 fps; only frames DWM really composed on the output are counted,
                 so frames/second is the output's real update rate (risk R10).
  -Page        : opens tools/latency-timer.html full screen on the virtual monitor (fresh Edge profile) for the run.

  Runs tools/idle-guard.ps1 before the run and every 30 s during it. The run is valid only if bench_ok was true
  at the start and the machine stayed idle and on AC throughout; otherwise the summary says "noisy": true.
  CPU load is not re-checked during the run because the run itself loads the CPU.

  Writes <LogDir>\<Label>.ffmpeg.log (raw, large) and <OutDir>\<Label>.json (summary, small).

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\encode-bench.ps1 -Label enc-bgra-60s -Seconds 60 -Page
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\encode-bench.ps1 -SelfTest
#>
[CmdletBinding()]
param(
    [ValidateSet('encode', 'render')][string]$Mode = 'encode',
    [string]$Label = "run-$(Get-Date -Format yyyyMMdd-HHmmss)",
    [int]$Seconds = 60,
    [int]$Fps = 120,
    [int]$OutputIdx = 1,
    # bgra: the encoder converts BGRA->NV12 itself; nv12: vpp_qsv converts first.
    [ValidateSet('bgra', 'nv12')][string]$PixFmt = 'bgra',
    # Encoded size, e.g. 1480x924: vpp_qsv scales the captured frame on the GPU. Empty = captured size.
    [ValidatePattern('^(\d{3,5}x\d{3,5})?$')][string]$Scale = '',
    # hevc_qsv preset (veryfast = TargetUsage 7). Empty = encoder default (TargetUsage 4).
    [ValidateSet('', 'veryfast')][string]$Preset = '',
    [int]$BitrateMbps = 80,
    [switch]$Page,
    [string]$OutDir = '',  # default bench\raw\m0-encode, set below: $PSScriptRoot is empty in param defaults on PS 5.1
    [string]$LogDir = (Join-Path $env:TEMP 'vetro-bench'),
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
$IdleGuard = Join-Path $PSScriptRoot 'idle-guard.ps1'
if (-not $OutDir) { $OutDir = Join-Path (Split-Path -Parent $PSScriptRoot) 'bench\raw\m0-encode' }
$WarmupSeconds = 5

function Get-Percentile([double[]]$Sorted, [double]$P) {
    # nearest-rank on an ascending array
    if ($Sorted.Count -eq 0) { return $null }
    $rank = [Math]::Ceiling($P / 100 * $Sorted.Count)
    return $Sorted[[Math]::Max(0, $rank - 1)]
}

function Get-EncodeTimesMs([string[]]$Lines) {
    # -benchmark_all prints one "encode_video" line for send_frame and one for receive_packet per frame.
    $real = foreach ($l in $Lines) {
        if ($l -match '^bench:\s+\d+ user\s+\d+ sys\s+(\d+) real encode_video') { [double]$Matches[1] }
    }
    $real = @($real)
    $send = @(for ($i = 0; $i + 1 -lt $real.Count; $i += 2) { $real[$i] / 1000 })
    $recv = @(for ($i = 1; $i -lt $real.Count; $i += 2) { $real[$i] / 1000 })
    $times = @(for ($i = 0; $i -lt $recv.Count; $i++) { $send[$i] + $recv[$i] })
    [pscustomobject]@{ times = $times; send = $send; recv = $recv; lines = $real.Count }
}

function Get-Histogram([double[]]$Values) {
    # Share of frames per 2 ms bin ("16" = 16-18 ms); the last bin "40" holds everything >= 40 ms.
    $h = [ordered]@{}
    if ($Values.Count -eq 0) { return $h }
    $Values | Group-Object { [Math]::Min(40, 2 * [Math]::Floor($_ / 2)) } | Sort-Object { [double]$_.Name } |
        ForEach-Object { $h[$_.Name] = [Math]::Round(100 * $_.Count / $Values.Count, 1) }
    $h
}

function Get-Stats([double[]]$Values) {
    $s = @($Values | Sort-Object)
    if ($s.Count -eq 0) { return $null }
    [ordered]@{
        n   = $s.Count
        p50 = [Math]::Round((Get-Percentile $s 50), 2)
        p95 = [Math]::Round((Get-Percentile $s 95), 2)
        p99 = [Math]::Round((Get-Percentile $s 99), 2)
        max = [Math]::Round($s[-1], 2)
        mean = [Math]::Round(($s | Measure-Object -Average).Average, 2)
    }
}

if ($SelfTest) {
    $p = Get-Percentile @(1, 2, 3, 4, 5, 6, 7, 8, 9, 10) 50
    if ($p -ne 5) { throw "p50 expected 5, got $p" }
    $p = Get-Percentile @(1, 2, 3, 4, 5, 6, 7, 8, 9, 10) 99
    if ($p -ne 10) { throw "p99 expected 10, got $p" }
    $sample = @(
        'bench:        0 user        0 sys     4000 real encode_video 0.0',
        'bench:        0 user        0 sys       60 real encode_video 0.0',
        'frame=    1 fps=0.0 q=0.0 size=0KiB',
        'bench:    15625 user    15625 sys     5000 real encode_video 0.0',
        'bench:        0 user        0 sys       40 real encode_video 0.0',
        'bench:        0 user        0 sys      999 real decode_video 0.0'
    )
    $r = Get-EncodeTimesMs $sample
    if ($r.lines -ne 4 -or $r.times.Count -ne 2 -or $r.times[0] -ne 4.06 -or $r.times[1] -ne 5.04 -or
        $r.send[1] -ne 5 -or $r.recv[0] -ne 0.06) {
        throw "pairing failed: $($r | ConvertTo-Json -Compress)"
    }
    $h = Get-Histogram @(1, 3, 3.5, 17, 55)
    if ($h['0'] -ne 20 -or $h['2'] -ne 40 -or $h['16'] -ne 20 -or $h['40'] -ne 20) { throw "histogram failed: $($h | ConvertTo-Json -Compress)" }    Write-Host 'SelfTest passed.'
    exit 0
}

$ffmpeg = Join-Path $env:FFMPEG_DIR 'bin\ffmpeg.exe'
if (-not (Test-Path $ffmpeg)) { throw "ffmpeg.exe not found under FFMPEG_DIR ($env:FFMPEG_DIR)" }
New-Item -ItemType Directory -Force -Path $OutDir, $LogDir | Out-Null

$guardStart = & powershell -NoProfile -ExecutionPolicy Bypass -File $IdleGuard | Out-String | ConvertFrom-Json

# Sampler: CPU, clocks, thermal, GPU engines (summed over processes, busiest engine per type), idle state.
$sampler = Start-Job -ArgumentList $IdleGuard -ScriptBlock {
    param($IdleGuard)
    $i = 0
    while ($true) {
        $cpu = Get-CimInstance Win32_PerfFormattedData_Counters_ProcessorInformation -Filter "Name='_Total'"
        $tz = @(Get-CimInstance Win32_PerfFormattedData_Counters_ThermalZoneInformation)
        $eng = Get-CimInstance Win32_PerfFormattedData_GPUPerformanceCounters_GPUEngine |
            ForEach-Object { if ($_.Name -match '(luid_\w+?_phys_\d+_eng_\d+)_engtype_(\w*)$') {
                [pscustomobject]@{ eng = $Matches[1]; type = $Matches[2]; u = [double]$_.UtilizationPercentage } } }
        $gpu = @{}
        foreach ($g in ($eng | Group-Object eng)) {
            $t = $g.Group[0].type; if (-not $t) { $t = 'other' }
            $sum = ($g.Group | Measure-Object u -Sum).Sum
            if (-not $gpu.ContainsKey($t) -or $sum -gt $gpu[$t]) { $gpu[$t] = $sum }
        }
        $s = [ordered]@{
            t          = (Get-Date).ToString('o')
            cpu_pct    = [int]$cpu.PercentProcessorTime
            # ProcessorFrequency is the static base clock; PercentProcessorPerformance is the real clock as % of base.
            cpu_perf_pct  = [int]$cpu.PercentProcessorPerformance
            cpu_limit_pct = [int]$cpu.PercentPerformanceLimit
            temp_c     = if ($tz.Count) { [Math]::Round(($tz | Measure-Object HighPrecisionTemperature -Maximum).Maximum / 10 - 273.15, 1) } else { $null }
            throttle   = [int](($tz | Measure-Object ThrottleReasons -Maximum).Maximum)
            passive_limit_pct = [int](($tz | Measure-Object PercentPassiveLimit -Minimum).Minimum)
            gpu        = $gpu
        }
        if ($i % 15 -eq 0) {
            $g = & powershell -NoProfile -ExecutionPolicy Bypass -File $IdleGuard | Out-String | ConvertFrom-Json
            $s.idle_min = $g.idle_minutes; $s.on_ac = $g.on_ac
        }
        $s | ConvertTo-Json -Compress -Depth 3
        $i++
        Start-Sleep -Seconds 2
    }
}

$edge = $null
if ($Page) {
    Add-Type -AssemblyName System.Windows.Forms
    # The virtual monitor is the screen closest to the tablet's 2960:1848 aspect; aspect survives DPI virtualisation, area does not.
    # shortcut: assumes the laptop panel's aspect differs (16:9 on the reference laptop); match by device name when that stops holding.
    $screen = [System.Windows.Forms.Screen]::AllScreens | Sort-Object { [Math]::Abs($_.Bounds.Width / $_.Bounds.Height - 2960 / 1848) } | Select-Object -First 1
    $msedge = Join-Path ${env:ProgramFiles(x86)} 'Microsoft\Edge\Application\msedge.exe'
    $profileDir = Join-Path $LogDir 'edge-profile'
    $pageUrl = ([Uri](Join-Path $PSScriptRoot 'latency-timer.html')).AbsoluteUri
    $edge = Start-Process -FilePath $msedge -PassThru -ArgumentList @(
        "--user-data-dir=`"$profileDir`"", '--no-first-run', '--new-window', '--start-fullscreen',
        "--window-position=$($screen.Bounds.X + 50),$($screen.Bounds.Y + 50)", $pageUrl)
    Start-Sleep -Seconds 5
}

$log = Join-Path $LogDir "$Label.ffmpeg.log"
$progress = Join-Path $LogDir "$Label.progress.txt"
if ($Mode -eq 'encode') {
    $chain = "ddagrab=output_idx=${OutputIdx}:framerate=${Fps}:draw_mouse=0,hwmap=derive_device=qsv,format=qsv"
    $vpp = @()
    if ($PixFmt -eq 'nv12') { $vpp += 'format=nv12' }
    if ($Scale) { $sw, $sh = $Scale -split 'x'; $vpp += "w=${sw}:h=${sh}" }
    if ($vpp) { $chain += ',vpp_qsv=' + ($vpp -join ':') }
    $vbvBits = [int]($BitrateMbps * 1e6 / $Fps)
    $presetArgs = if ($Preset) { @('-preset', $Preset) } else { @() }
    # -v verbose: hevc_qsv logs its effective parameters once (VDENC, TargetUsage); no per-frame lines.
    $ffArgs = @('-hide_banner', '-nostdin', '-v', 'verbose', '-benchmark_all', '-stats_period', '1', '-progress', $progress,
        '-filter_complex', $chain, '-c:v', 'hevc_qsv', '-profile:v', 'main') + $presetArgs + @('-async_depth', '1', '-low_power', '1',
        '-bf', '0', '-low_delay_brc', '1', '-b:v', "${BitrateMbps}M", '-maxrate', "${BitrateMbps}M",
        '-bufsize', "$vbvBits", '-g', '1000000', '-t', "$Seconds", '-f', 'null', '-')
} else {
    $ffArgs = @('-hide_banner', '-nostdin', '-stats_period', '1', '-progress', $progress,
        '-filter_complex', "ddagrab=output_idx=${OutputIdx}:framerate=240:dup_frames=0:draw_mouse=0",
        '-fps_mode', 'passthrough', '-t', "$Seconds", '-f', 'null', '-')
}
try {
    # ffmpeg writes everything to stderr; Start-Process keeps it out of PowerShell's error stream.
    $ff = Start-Process -FilePath $ffmpeg -ArgumentList $ffArgs -RedirectStandardError $log -NoNewWindow -PassThru -Wait
    $exit = $ff.ExitCode
} finally {
    # Also on Ctrl+C: close the Edge window this run opened and stop the sampler.
    if ($edge) { Stop-Process -Id $edge.Id -ErrorAction SilentlyContinue }
    Stop-Job $sampler
}
$samples = @(Receive-Job $sampler | ForEach-Object { $_ | ConvertFrom-Json })
Remove-Job $sampler -Force

# Progress blocks come once per second of wall time: frame count per block gives fps over time.
$frames = @(Select-String -Path $progress -Pattern '^frame=(\d+)' | ForEach-Object { [int]$_.Matches[0].Groups[1].Value })
$perSec = @(for ($i = 1; $i -lt $frames.Count; $i++) { $frames[$i] - $frames[$i - 1] })
$steady = @($perSec | Select-Object -Skip $WarmupSeconds)
$fpsStats = Get-Stats $steady

$enc = $null; $perMinute = @()
if ($Mode -eq 'encode') {
    $r = Get-EncodeTimesMs (Get-Content -Path $log)
    $skip = if ($frames.Count -gt $WarmupSeconds) { $frames[$WarmupSeconds - 1] } else { 0 }
    $times = @($r.times | Select-Object -Skip $skip)
    $enc = Get-Stats $times
    if ($enc) {  # null when ffmpeg failed before encoding anything
        $enc.frames_logged = $r.times.Count
        $enc.frames_reported = if ($frames.Count) { $frames[-1] } else { 0 }
        $enc.over_budget_5ms_pct = [Math]::Round(100 * @($times | Where-Object { $_ -gt 5 }).Count / $times.Count, 2)
        $enc.over_frame_period_pct = [Math]::Round(100 * @($times | Where-Object { $_ -gt 1000 / $Fps }).Count / $times.Count, 2)
        # With async_depth=1 almost all of the time is in send_frame; the split shows it.
        $enc.send_ms = Get-Stats @($r.send | Select-Object -Skip $skip)
        $enc.receive_ms = Get-Stats @($r.recv | Select-Object -Skip $skip)
        $enc.histogram_2ms_pct = Get-Histogram $times
    }
    # Drift over a soak: encode p50/p99 and fps per minute.
    for ($m = 0; ($m + 1) * 60 -le $frames.Count; $m++) {
        $a = if ($m -eq 0) { 0 } else { $frames[$m * 60 - 1] }
        $b = $frames[($m + 1) * 60 - 1]
        if ($b -le $a) { continue }  # no frame in that minute: $a..($b - 1) would count downwards
        $w = @($r.times[$a..($b - 1)])
        $st = Get-Stats $w
        $perMinute += [ordered]@{ minute = $m + 1; fps = [Math]::Round(($b - $a) / 60, 1); p50 = $st.p50; p99 = $st.p99 }
    }
}

function Get-SampleStats($Name) { Get-Stats @($samples | ForEach-Object { $_.$Name } | Where-Object { $null -ne $_ }) }
$gpuTypes = @($samples | ForEach-Object { $_.gpu.PSObject.Properties.Name } | Sort-Object -Unique)
$gpuStats = [ordered]@{}
foreach ($t in $gpuTypes) { $gpuStats[$t] = Get-Stats @($samples | ForEach-Object { $_.gpu.$t } | Where-Object { $null -ne $_ }) }

$idleSamples = @($samples | Where-Object { $null -ne $_.idle_min })
# An idle check comes about every 40 s; a run without enough of them proves nothing about idleness.
$idleChecksNeeded = [Math]::Max(1, [Math]::Floor($Seconds / 45))
$stayedIdle = $idleSamples.Count -ge $idleChecksNeeded -and -not ($idleSamples | Where-Object { $_.idle_min -lt 10 -or -not $_.on_ac })
# The timer page is the only big 3D user besides vpp_qsv; a near-idle 3D engine means the page did not render
# (with nv12 input vpp_qsv keeps 3D busy too, so there it catches only a page that is also missing other load).
$page3d = $gpuStats['3D']
$pageSuspect = [bool]$Page -and (-not $page3d -or $page3d.p50 -lt 5)
$noisy = -not ($guardStart.policy.bench_ok -and $stayedIdle)

$summary = [ordered]@{
    label = $Label; mode = $Mode; started = $guardStart.checked_at; seconds = $Seconds
    settings = [ordered]@{ fps = $Fps; output_idx = $OutputIdx; input = $PixFmt; scale = $Scale; preset = $Preset; bitrate_mbps = $BitrateMbps; page = [bool]$Page }
    ffmpeg_exit = $exit
    # What hevc_qsv really configured (verbose log); null in render mode (indexing a missing match threw there).
    encoder = if ($Mode -eq 'encode') { [ordered]@{
        vdenc        = @(Select-String -Path $log -Pattern '\] VDENC: (\w+)' | ForEach-Object { $_.Matches[0].Groups[1].Value })[0]
        target_usage = @(Select-String -Path $log -Pattern '\] TargetUsage: (\d+)' | ForEach-Object { $_.Matches[0].Groups[1].Value })[0]
        low_delay_brc = @(Select-String -Path $log -Pattern '\] LowDelayBRC: (\w+)' | ForEach-Object { $_.Matches[0].Groups[1].Value })[0]
        rate_control = @(Select-String -Path $log -Pattern 'RateControlMethod: (\w+)' | ForEach-Object { $_.Matches[0].Groups[1].Value })[0]
    } } else { $null }
    # Size of ffmpeg's output stream: the encoded size in encode mode (after -Scale), the captured size in render mode.
    output_size = @(Select-String -Path $log -Pattern 'Stream #0:0: Video: .*?(\d{3,5}x\d{3,5})' | ForEach-Object { $_.Matches[0].Groups[1].Value })[0]
    page_render_suspect = $pageSuspect
    noisy = $noisy; guard_start = $guardStart; idle_checks = $idleSamples.Count; idle_checks_needed = $idleChecksNeeded; stayed_idle_on_ac = [bool]$stayedIdle
    idle_check_failures = @($idleSamples | Where-Object { $_.idle_min -lt 10 -or -not $_.on_ac } | ForEach-Object { "$($_.t) idle_min=$($_.idle_min) on_ac=$($_.on_ac)" })
    fps_per_second_after_warmup = $fpsStats
    encode_ms = $enc
    per_minute = $perMinute
    cpu_pct = Get-SampleStats 'cpu_pct'
    cpu_perf_pct = Get-SampleStats 'cpu_perf_pct'; cpu_limit_pct = Get-SampleStats 'cpu_limit_pct'
    temp_c = Get-SampleStats 'temp_c'; passive_limit_pct = Get-SampleStats 'passive_limit_pct'
    throttle_reasons_seen = @($samples | ForEach-Object { $_.throttle } | Sort-Object -Unique)
    gpu_engine_util_pct = $gpuStats
}
$json = Join-Path $OutDir "$Label.json"
$summary | ConvertTo-Json -Depth 6 | Set-Content -Path $json -Encoding UTF8
Write-Host "Summary: $json (noisy=$noisy, page_render_suspect=$pageSuspect, ffmpeg exit $exit)"
