<#
.SYNOPSIS
  M0-5 unattended benchmark sequence (~67 min, -Only for a subset): short encode runs, R10 render-rate runs,
  the 30-min soak and 3-min fallback-mode runs (GPU-scaled 1480x924 and 1920x1200 @120, native @60, preset veryfast).

.DESCRIPTION
  Refuses to start unless tools/idle-guard.ps1 says bench_ok (owner idle >= 10 min, AC power, CPU < 20 %), and
  re-checks before every step. Each step is one tools/encode-bench.ps1 run; results land in
  bench\raw\m0-encode\<Prefix>-<step>.json plus <Prefix>-sequence.json.

  Display layout (no administrator rights; the APIs behind Win+P and Settings > Display): saves the active layout
  (QueryDisplayConfig), extends the desktop onto the VDD if it is off, sets it to 2960x1848 @120, makes it primary
  for the render-vdd-primary step only (SetDisplayConfig source positions: the source at (0,0) is primary), and puts
  the saved layout back at the end, also on failure or Ctrl+C. A step that fails is recorded and the next one runs.
  Precondition besides bench_ok: no screen saver or display sleep (a screen saver stops Desktop Duplication).
  Must run in the owner's interactive session: a sandboxed shell gets access denied and the script refuses.

  Exit codes: 0 every run valid; 1 finished but a run was noisy, failed or captured the wrong size; 2 refused to
  start (not bench_ok, VDD or its 120 Hz mode missing, no display access); 3 stopped early because the machine
  stopped being idle/on AC; 4 unexpected error.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\m0-encode-sequence.ps1 -DryRun
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\m0-encode-sequence.ps1
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\m0-encode-sequence.ps1 -Prefix m0-20261126-0102 -Only render-vdd-primary,soak-nv12-30min
#>
[CmdletBinding()]
param(
    [switch]$DryRun,
    [string]$Prefix = "m0-$(Get-Date -Format yyyyMMdd-HHmm)",
    # Run only these steps (comma-separated names). With an existing -Prefix, results of other steps are kept.
    [string[]]$Only = @()
)

$ErrorActionPreference = 'Stop'
$Bench = Join-Path $PSScriptRoot 'encode-bench.ps1'
$IdleGuard = Join-Path $PSScriptRoot 'idle-guard.ps1'
$OutDir = Join-Path (Split-Path -Parent $PSScriptRoot) 'bench\raw\m0-encode'
$W = 2960; $H = 1848; $Hz = 120

# size = expected encoded (encode mode) or captured (render mode) size in the run's summary.
$Steps = @(
    @{ name = 'enc-bgra-120';         vddPrimary = $false; cool = 30;  seconds = 300;  args = @('-Mode', 'encode', '-PixFmt', 'bgra', '-Page') }
    @{ name = 'enc-nv12-120';         vddPrimary = $false; cool = 30;  seconds = 300;  args = @('-Mode', 'encode', '-PixFmt', 'nv12', '-Page') }
    @{ name = 'render-panel-primary'; vddPrimary = $false; cool = 30;  seconds = 120;  args = @('-Mode', 'render', '-Page') }
    @{ name = 'render-vdd-primary';   vddPrimary = $true;  cool = 30;  seconds = 120;  args = @('-Mode', 'render', '-Page') }
    # nv12 is the faster input path (enc-nv12-120 vs enc-bgra-120), so it is the one the soak must hold.
    @{ name = 'soak-nv12-30min';      vddPrimary = $false; cool = 120; seconds = 1800; args = @('-Mode', 'encode', '-PixFmt', 'nv12', '-Page') }
    # Fallback candidates for M0-8, all from the native 2960x1848 @120 capture (no new VDD mode).
    @{ name = 'fb-nv12-1480x924-120';  vddPrimary = $false; cool = 120; seconds = 180; size = '1480x924';  args = @('-Mode', 'encode', '-PixFmt', 'nv12', '-Scale', '1480x924', '-Page') }
    @{ name = 'fb-nv12-1920x1200-120'; vddPrimary = $false; cool = 30;  seconds = 180; size = '1920x1200'; args = @('-Mode', 'encode', '-PixFmt', 'nv12', '-Scale', '1920x1200', '-Page') }
    @{ name = 'fb-nv12-2960x1848-60';  vddPrimary = $false; cool = 30;  seconds = 180; args = @('-Mode', 'encode', '-PixFmt', 'nv12', '-Fps', '60', '-Page') }
    @{ name = 'fb-nv12-120-veryfast';  vddPrimary = $false; cool = 30;  seconds = 180; args = @('-Mode', 'encode', '-PixFmt', 'nv12', '-Preset', 'veryfast', '-Page') }
)
if ($Only) {
    $Only = @($Only -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ })  # -File passes "a,b" as one string
    $unknown = @($Only | Where-Object { $_ -notin $Steps.name })
    if ($unknown) { Write-Host "Unknown step(s): $($unknown -join ', '). Steps: $($Steps.name -join ', ')"; exit 2 }
    $Steps = @($Steps | Where-Object { $_.name -in $Only })
}

if (-not ('VetroDisplay' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class VetroDisplay {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct DISPLAY_DEVICE { public int cb; [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string DeviceName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceString; public int StateFlags;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceID;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceKey; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct DEVMODE { [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmDeviceName;
        public short dmSpecVersion, dmDriverVersion, dmSize, dmDriverExtra; public int dmFields;
        public int dmPositionX, dmPositionY, dmDisplayOrientation, dmDisplayFixedOutput;
        public short dmColor, dmDuplex, dmYResolution, dmTTOption, dmCollate;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmFormName; public short dmLogPixels;
        public int dmBitsPerPel, dmPelsWidth, dmPelsHeight, dmDisplayFlags, dmDisplayFrequency;
        public int dmICMMethod, dmICMIntent, dmMediaType, dmDitherType, dmReserved1, dmReserved2, dmPanningWidth, dmPanningHeight; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool EnumDisplayDevicesW(string dev, uint i, ref DISPLAY_DEVICE dd, uint flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool EnumDisplaySettingsW(string dev, int mode, ref DEVMODE dm);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int ChangeDisplaySettingsExW(string dev, ref DEVMODE dm, IntPtr hwnd, uint flags, IntPtr p);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int ChangeDisplaySettingsExW(string dev, IntPtr dm, IntPtr hwnd, uint flags, IntPtr p);

    public class Info { public string Name, Desc; public bool Attached, Primary; public int X, Y, W, H, Hz; public string[] Modes; }

    static DEVMODE NewMode() { var m = new DEVMODE(); m.dmSize = (short)Marshal.SizeOf(typeof(DEVMODE)); return m; }

    public static List<Info> Snapshot() {
        var r = new List<Info>();
        for (uint i = 0; ; i++) {
            var dd = new DISPLAY_DEVICE(); dd.cb = Marshal.SizeOf(typeof(DISPLAY_DEVICE));
            if (!EnumDisplayDevicesW(null, i, ref dd, 0)) break;
            var info = new Info { Name = dd.DeviceName, Desc = dd.DeviceString, Attached = (dd.StateFlags & 1) != 0, Primary = (dd.StateFlags & 4) != 0 };
            var cur = NewMode();
            if (info.Attached && EnumDisplaySettingsW(dd.DeviceName, -1, ref cur)) {
                info.X = cur.dmPositionX; info.Y = cur.dmPositionY; info.W = cur.dmPelsWidth; info.H = cur.dmPelsHeight; info.Hz = cur.dmDisplayFrequency;
            }
            var modes = new SortedSet<string>();
            for (int m = 0; m < 10000; m++) {
                var x = NewMode(); if (!EnumDisplaySettingsW(dd.DeviceName, m, ref x)) break;
                modes.Add(x.dmPelsWidth + "x" + x.dmPelsHeight + "@" + x.dmDisplayFrequency);
            }
            info.Modes = new string[modes.Count]; modes.CopyTo(info.Modes);
            r.Add(info);
        }
        return r;
    }

    // Queued until Commit(): mode and position, plus primary flag.
    public static int Set(string dev, int w, int h, int hz, int x, int y, bool primary) {
        var dm = NewMode();
        dm.dmPelsWidth = w; dm.dmPelsHeight = h; dm.dmDisplayFrequency = hz; dm.dmPositionX = x; dm.dmPositionY = y;
        dm.dmFields = 0x20 | 0x80000 | 0x100000 | 0x400000;  // POSITION | PELSWIDTH | PELSHEIGHT | DISPLAYFREQUENCY
        uint f = 0x1u | 0x10000000u | (primary ? 0x10u : 0u); // CDS_UPDATEREGISTRY | CDS_NORESET | CDS_SET_PRIMARY
        return ChangeDisplaySettingsExW(dev, ref dm, IntPtr.Zero, f, IntPtr.Zero);
    }
    public static int Commit() { return ChangeDisplaySettingsExW(null, IntPtr.Zero, IntPtr.Zero, 0, IntPtr.Zero); }

    // CCD: the active layout as opaque path/mode arrays (DISPLAYCONFIG_PATH_INFO 72 B, DISPLAYCONFIG_MODE_INFO 64 B).
    [DllImport("user32.dll")] static extern int GetDisplayConfigBufferSizes(uint flags, out uint np, out uint nm);
    [DllImport("user32.dll")] static extern int QueryDisplayConfig(uint flags, ref uint np, byte[] p, ref uint nm, byte[] m, IntPtr topo);
    [DllImport("user32.dll")] static extern int SetDisplayConfig(uint np, byte[] p, uint nm, byte[] m, uint flags);
    public class Layout { public uint Np, Nm; public byte[] P, M; }
    public static Layout Save() {
        uint np, nm; int e = GetDisplayConfigBufferSizes(2, out np, out nm); // QDC_ONLY_ACTIVE_PATHS
        if (e != 0) throw new Exception("GetDisplayConfigBufferSizes " + e);
        var l = new Layout { P = new byte[np * 72], M = new byte[nm * 64] };
        e = QueryDisplayConfig(2, ref np, l.P, ref nm, l.M, IntPtr.Zero);
        if (e != 0) throw new Exception("QueryDisplayConfig " + e);
        l.Np = np; l.Nm = nm; return l;
    }
    // validate == true checks only (SDC_VALIDATE); otherwise SDC_APPLY | SAVE_TO_DATABASE | ALLOW_CHANGES.
    public static int Restore(Layout l, bool validate) {
        return SetDisplayConfig(l.Np, l.P, l.Nm, l.M, 0x20u | (validate ? 0x40u : 0x80u | 0x200u | 0x400u)); // USE_SUPPLIED_DISPLAY_CONFIG
    }
    public static int Extend(bool validate) { return SetDisplayConfig(0, null, 0, null, 0x4u | (validate ? 0x40u : 0x80u)); } // TOPOLOGY_EXTEND

    // Places the two desktop sources side by side; the primary is the source at (0,0) (CCD has no primary flag).
    // The VDD source is the one of size vw x vh. Source mode: infoType@0 (1 = source), width@16, height@20, x@28, y@32.
    // shortcut: identifies the VDD by size, so it needs a panel of another size (true on the reference laptop).
    public static int Arrange(int vw, int vh, bool vddPrimary) {
        var l = Save();
        int v = -1, p = -1;
        for (int o = 0; o < l.Nm * 64; o += 64) {
            if (BitConverter.ToUInt32(l.M, o) != 1) continue;
            bool isVdd = BitConverter.ToInt32(l.M, o + 16) == vw && BitConverter.ToInt32(l.M, o + 20) == vh;
            if (isVdd) { if (v >= 0) throw new Exception("two sources of the VDD size"); v = o; }
            else { if (p >= 0) throw new Exception("more than one non-VDD source"); p = o; }
        }
        if (v < 0 || p < 0) throw new Exception("VDD or panel source not found");
        int pw = BitConverter.ToInt32(l.M, p + 16);
        Put(l.M, v + 28, vddPrimary ? 0 : pw); Put(l.M, v + 32, 0);
        Put(l.M, p + 28, vddPrimary ? -pw : 0); Put(l.M, p + 32, 0);
        return Restore(l, false);
    }
    static void Put(byte[] b, int o, int x) { BitConverter.GetBytes(x).CopyTo(b, o); }
}
'@
}
[void][VetroDisplay]::SetProcessDPIAware()  # physical pixels for positions

function Get-Guard { & powershell -NoProfile -ExecutionPolicy Bypass -File $IdleGuard | Out-String | ConvertFrom-Json }

function Get-Vdd($Snap) { @($Snap | Where-Object { $_.Desc -eq 'Virtual Display Driver' })[0] }

function Set-Layout([bool]$VddPrimary) {
    # Extend (Win+P) if the VDD is off the desktop, then panel and VDD side by side; the primary sits at (0,0).
    if (-not (Get-Vdd ([VetroDisplay]::Snapshot())).Attached) {
        $e = [VetroDisplay]::Extend($false); if ($e -ne 0) { throw "extend failed (Win32 error $e)" }
        Start-Sleep -Seconds 3
    }
    $vdd = Get-Vdd ([VetroDisplay]::Snapshot())
    if (-not $vdd.Attached) { throw 'VDD is not on the desktop after extend' }
    if ($vdd.W -ne $W -or $vdd.H -ne $H -or $vdd.Hz -ne $Hz) {
        # Mode only, in place. Primary goes through CCD below: CDS_SET_PRIMARY on the VDD returned DISP_CHANGE_FAILED (run m0-20261126-0102).
        $codes = @([VetroDisplay]::Set($vdd.Name, $W, $H, $Hz, $vdd.X, $vdd.Y, $false), [VetroDisplay]::Commit())
        if ($codes | Where-Object { $_ -ne 0 }) { throw "VDD mode change failed (DISP_CHANGE codes $($codes -join ','))" }
        Start-Sleep -Seconds 3
    }
    $e = [VetroDisplay]::Arrange($W, $H, $VddPrimary)
    if ($e -ne 0) { throw "SetDisplayConfig arrange failed (vddPrimary=$VddPrimary, Win32 error $e)" }
    Start-Sleep -Seconds 5  # let DWM and DXGI settle before ddagrab enumerates outputs
    $v = Get-Vdd ([VetroDisplay]::Snapshot())
    if ($v.W -ne $W -or $v.H -ne $H -or $v.Hz -ne $Hz -or $v.Primary -ne $VddPrimary) {
        throw "VDD is $($v.W)x$($v.H)@$($v.Hz) primary=$($v.Primary), wanted ${W}x${H}@${Hz} primary=$VddPrimary"
    }
}

function Find-VddOutput {
    # ddagrab numbers outputs across adapters, and the order changes with the primary: probe for the VDD's size.
    $ffmpeg = Join-Path $env:FFMPEG_DIR 'bin\ffmpeg.exe'
    $log = Join-Path $env:TEMP 'vetro-dda-probe.log'
    for ($i = 0; $i -lt 4; $i++) {
        Start-Process -FilePath $ffmpeg -NoNewWindow -Wait -RedirectStandardError $log -ArgumentList @(
            '-hide_banner', '-nostdin', '-filter_complex', "ddagrab=output_idx=${i}:framerate=10", '-frames:v', '1', '-f', 'null', '-')
        if (Select-String -Path $log -Pattern "Video: .*\b${W}x${H}\b" -Quiet) { return $i }
    }
    throw "no ddagrab output is ${W}x${H}"
}

function Wait-BenchOk {
    # CPU can spike briefly after a run; idle or AC lost means the owner is back, so stop.
    for ($try = 0; $try -lt 4; $try++) {
        $g = Get-Guard
        if ($g.policy.bench_ok) { return $true }
        if ($g.policy.mode -ne 'idle' -or -not $g.on_ac) { Write-Host "Not idle/on AC: $($g | ConvertTo-Json -Compress -Depth 3)"; return $false }
        Start-Sleep -Seconds 30
    }
    Write-Host "CPU stayed busy: $($g | ConvertTo-Json -Compress -Depth 3)"
    return $false
}

# ---- plan ----
$total = ($Steps | ForEach-Object { $_.seconds + $_.cool + 20 } | Measure-Object -Sum).Sum + 120  # +20 s per run for page start and parsing, +2 min soak parse
Write-Host "Plan ($Prefix), about $([Math]::Round($total / 60)) min:"
foreach ($s in $Steps) {
    Write-Host ("  {0,-22} {1,5} s  primary={2,-5} cooldown {3,3} s  {4}" -f $s.name, $s.seconds, $(if ($s.vddPrimary) { 'vdd' } else { 'panel' }), $s.cool, ($s.args -join ' '))
}

$vdd = Get-Vdd ([VetroDisplay]::Snapshot())
$saved = [VetroDisplay]::Save()
# Display changes need the owner's interactive session; a sandboxed agent shell gets ERROR_ACCESS_DENIED (5) here.
$access = [VetroDisplay]::Restore($saved, $true)
$extendOk = [VetroDisplay]::Extend($true)
Write-Host "Display access: validate current layout -> $access, validate extend -> $extendOk (0 = OK, 5 = access denied)"
$guard = Get-Guard
Write-Host "idle-guard: mode=$($guard.policy.mode) on_ac=$($guard.on_ac) cpu=$($guard.cpu_load_pct)% bench_ok=$($guard.policy.bench_ok)"
if (-not $vdd) { Write-Host 'Virtual Display Driver not found.'; exit 2 }
Write-Host "VDD: $($vdd.Name) attached=$($vdd.Attached) primary=$($vdd.Primary) current=$($vdd.W)x$($vdd.H)@$($vdd.Hz) offers ${W}x${H}@${Hz}=$($vdd.Modes -contains "${W}x${H}@${Hz}")"
if ($vdd.Modes -notcontains "${W}x${H}@${Hz}") { Write-Host "VDD does not offer ${W}x${H}@${Hz}."; exit 2 }

if ($DryRun) { Write-Host 'Dry run: nothing changed.'; exit 0 }
if ($access -ne 0 -or $extendOk -ne 0) { Write-Host 'Refusing to start: no display-configuration access from this shell; launch it from the owner''s own terminal.'; exit 2 }
if (-not $guard.policy.bench_ok) { Write-Host 'Refusing to start: idle-guard bench_ok is false (needs owner idle >= 10 min, AC power, CPU < 20 %).'; exit 2 }

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$seqJson = Join-Path $OutDir "$Prefix-sequence.json"
# Keep earlier results of this prefix for the steps not rerun now.
# The parentheses matter: PS 5.1 ConvertFrom-Json emits a JSON array as one object, which Where-Object would keep whole.
$results = @(if (Test-Path $seqJson) { (Get-Content $seqJson -Raw | ConvertFrom-Json) | Where-Object { $_.step -notin $Steps.name } })
$exitCode = 0
try {
    $layout = $null
    foreach ($s in $Steps) {
        Start-Sleep -Seconds $s.cool
        if (-not (Wait-BenchOk)) { $exitCode = 3; break }
        $label = "$Prefix-$($s.name)"
        $json = Join-Path $OutDir "$label.json"
        Remove-Item $json -ErrorAction SilentlyContinue  # a failed run must not pick up an older summary
        $err = $null
        try {
            # A failing step (layout or bench) is recorded and the sequence goes on with the next one.
            if ($layout -ne $s.vddPrimary) { $layout = $null; Set-Layout $s.vddPrimary; $layout = $s.vddPrimary; $idx = Find-VddOutput }
            Write-Host "$(Get-Date -Format HH:mm:ss) $label on output $idx"
            & powershell -NoProfile -ExecutionPolicy Bypass -File $Bench -Label $label -Seconds $s.seconds -OutputIdx $idx @($s.args)
            if ($LASTEXITCODE -ne 0) { $err = "encode-bench exit $LASTEXITCODE" }
        } catch { $err = "$_"; Write-Host "Step $($s.name) failed: $err" }
        $r = if (Test-Path $json) { Get-Content $json -Raw | ConvertFrom-Json } else { $null }
        $size = if ($s.size) { $s.size } else { "${W}x${H}" }
        $ok = -not $err -and $r -and $r.ffmpeg_exit -eq 0 -and -not $r.noisy -and $r.captured_size -eq $size
        if (-not $ok) { $exitCode = 1 }
        $results += [pscustomobject][ordered]@{ step = $s.name; label = $label; valid = [bool]$ok; error = $err; noisy = $r.noisy; ffmpeg_exit = $r.ffmpeg_exit
            captured_size = $r.captured_size; fps_p50 = $r.fps_per_second_after_warmup.p50; encode_p50_ms = $r.encode_ms.p50; encode_p99_ms = $r.encode_ms.p99 }
    }
} catch {
    Write-Host "Error: $_"
    $exitCode = 4
} finally {
    Write-Host "Display layout restored (SetDisplayConfig $([VetroDisplay]::Restore($saved, $false)), 0 = OK)."
    ConvertTo-Json -InputObject @($results) -Depth 3 | Set-Content -Path $seqJson -Encoding UTF8
    $results | Format-Table -AutoSize | Out-String | Write-Host
    Write-Host "Sequence exit code $exitCode"
}
exit $exitCode
