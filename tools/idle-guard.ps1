<#
.SYNOPSIS
  Prints the machine state and the build/benchmark policy of CLAUDE.md ("Machine etiquette") as one JSON object.

.DESCRIPTION
  Reads: idle time (time since the last keyboard or mouse input), AC power, free physical RAM and CPU load.
  No administrator rights needed. Read-only: changes nothing on the machine.

  Policy (CLAUDE.md):
    owner-active (input in the last 10 min): priority below-normal, at most 8 jobs, pause while free RAM < 3 GB.
    idle (>= 10 min):                        priority normal, all logical CPUs, pause while free RAM < 1 GB.
    bench_ok: idle, on AC power and CPU load < 20 %. Otherwise a benchmark run is marked noisy.

.PARAMETER SelfTest
  Checks the policy logic against the expected table and exits (0 = pass, non-zero = fail). Reads no sensors.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\idle-guard.ps1

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\idle-guard.ps1 -SelfTest
#>
[CmdletBinding()]
param([switch]$SelfTest)

$ErrorActionPreference = 'Stop'

# Thresholds from CLAUDE.md "Machine etiquette".
$IdleThresholdMinutes = 10
$MaxJobsWhenActive    = 8
$RamPauseActiveGb     = 3
$RamPauseIdleGb       = 1
$BenchMaxCpuPct       = 20

if (-not ('VetroNative.Win' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace VetroNative {
    [StructLayout(LayoutKind.Sequential)]
    public struct LASTINPUTINFO { public uint cbSize; public uint dwTime; }

    [StructLayout(LayoutKind.Sequential)]
    public struct SYSTEM_POWER_STATUS {
        public byte ACLineStatus; public byte BatteryFlag; public byte BatteryLifePercent; public byte SystemStatusFlag;
        public uint BatteryLifeTime; public uint BatteryFullLifeTime;
    }

    public static class Win {
        [DllImport("user32.dll")]  public static extern bool GetLastInputInfo(ref LASTINPUTINFO plii);
        [DllImport("kernel32.dll")] public static extern bool GetSystemPowerStatus(out SYSTEM_POWER_STATUS sps);
    }
}
'@
}

function Get-IdlePolicy {
    param(
        [double]$IdleMinutes,
        [bool]$OnAc,
        [double]$FreeRamGb,
        [double]$CpuLoadPct,
        [int]$LogicalCpus
    )
    $active = $IdleMinutes -lt $IdleThresholdMinutes
    if ($active) {
        $mode     = 'owner-active'
        $maxJobs  = [Math]::Min($MaxJobsWhenActive, $LogicalCpus)
        $priority = 'below-normal'
        $pause    = $FreeRamGb -lt $RamPauseActiveGb
    } else {
        $mode     = 'idle'
        $maxJobs  = $LogicalCpus
        $priority = 'normal'
        $pause    = $FreeRamGb -lt $RamPauseIdleGb
    }
    $benchOk = (-not $active) -and $OnAc -and ($CpuLoadPct -lt $BenchMaxCpuPct)
    [pscustomobject]@{
        mode     = $mode
        max_jobs = [int]$maxJobs
        priority = $priority
        pause    = [bool]$pause
        bench_ok = [bool]$benchOk
    }
}

function Assert-Policy {
    param($Name, $Policy, $Mode, $MaxJobs, $Priority, $Pause, $BenchOk)
    $ok = ($Policy.mode -eq $Mode) -and ($Policy.max_jobs -eq $MaxJobs) -and ($Policy.priority -eq $Priority) `
          -and ($Policy.pause -eq $Pause) -and ($Policy.bench_ok -eq $BenchOk)
    if (-not $ok) { throw "SelfTest FAILED: $Name -> $($Policy | ConvertTo-Json -Compress)" }
    Write-Host "ok   $Name"
}

function Invoke-SelfTest {
    # Args: idle min, on AC, free RAM GB, CPU %, logical CPUs
    Assert-Policy 'active 5 min, AC, 10 GB, cpu 50, 16 cpus' `
        (Get-IdlePolicy 5 $true 10 50 16) 'owner-active' 8 'below-normal' $false $false
    Assert-Policy 'active 5 min, AC, 2.5 GB free -> pause' `
        (Get-IdlePolicy 5 $true 2.5 50 16) 'owner-active' 8 'below-normal' $true $false
    Assert-Policy 'active 9.9 min is still owner-active' `
        (Get-IdlePolicy 9.9 $true 10 5 16) 'owner-active' 8 'below-normal' $false $false
    Assert-Policy 'active with 4 cpus -> max_jobs 4' `
        (Get-IdlePolicy 0 $true 10 5 4) 'owner-active' 4 'below-normal' $false $false
    Assert-Policy 'idle exactly 10 min, AC, cpu 5 -> bench ok' `
        (Get-IdlePolicy 10 $true 10 5 16) 'idle' 16 'normal' $false $true
    Assert-Policy 'idle 30 min, 2 GB free -> no pause (idle limit is 1 GB)' `
        (Get-IdlePolicy 30 $true 2 5 16) 'idle' 16 'normal' $false $true
    Assert-Policy 'idle 30 min, 0.5 GB free -> pause' `
        (Get-IdlePolicy 30 $true 0.5 5 16) 'idle' 16 'normal' $true $true
    Assert-Policy 'idle 30 min, on battery -> bench not ok' `
        (Get-IdlePolicy 30 $false 10 5 16) 'idle' 16 'normal' $false $false
    Assert-Policy 'idle 30 min, cpu 25 % -> bench not ok' `
        (Get-IdlePolicy 30 $true 10 25 16) 'idle' 16 'normal' $false $false
    Assert-Policy 'idle 30 min, cpu exactly 20 % -> bench not ok' `
        (Get-IdlePolicy 30 $true 10 20 16) 'idle' 16 'normal' $false $false
    Write-Host 'SelfTest passed (10 cases).'
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

# Idle time: GetLastInputInfo gives dwTime (ms, 32-bit wrap); Environment.TickCount is the same clock (signed).
$lii = New-Object VetroNative.LASTINPUTINFO
$lii.cbSize = [System.Runtime.InteropServices.Marshal]::SizeOf([type][VetroNative.LASTINPUTINFO])
if (-not [VetroNative.Win]::GetLastInputInfo([ref]$lii)) { throw 'GetLastInputInfo failed' }
$nowMs  = [long][Environment]::TickCount
if ($nowMs -lt 0) { $nowMs += 4294967296 }
$idleMs = $nowMs - [long]$lii.dwTime
if ($idleMs -lt 0) { $idleMs += 4294967296 }

# AC power: ACLineStatus 0 = battery, 1 = AC, 255 = unknown (desktops without battery) -> treated as AC.
$sps = New-Object VetroNative.SYSTEM_POWER_STATUS
if (-not [VetroNative.Win]::GetSystemPowerStatus([ref]$sps)) { throw 'GetSystemPowerStatus failed' }
$onAc = ($sps.ACLineStatus -ne 0)

# Free physical RAM (Win32_OperatingSystem reports KB); CPU load: average over all processors (%).
$os      = Get-CimInstance -ClassName Win32_OperatingSystem
$freeGb  = [Math]::Round($os.FreePhysicalMemory / 1MB, 2)
$cpuLoad = [int][Math]::Round((Get-CimInstance -ClassName Win32_Processor | Measure-Object -Property LoadPercentage -Average).Average)

$idleMinutes = [Math]::Round($idleMs / 60000, 1)
$policy = Get-IdlePolicy -IdleMinutes $idleMinutes -OnAc $onAc -FreeRamGb $freeGb -CpuLoadPct $cpuLoad -LogicalCpus ([Environment]::ProcessorCount)

[pscustomobject]@{
    checked_at  = (Get-Date).ToString('o')
    idle_minutes = $idleMinutes
    on_ac       = $onAc
    free_ram_gb = $freeGb
    cpu_load_pct = $cpuLoad
    policy      = $policy
} | ConvertTo-Json -Depth 4
