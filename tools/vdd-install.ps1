<#
.SYNOPSIS
  Installs (or removes) the pinned, signed Virtual Display Driver release with one 2960x1848 @120 virtual monitor.

.DESCRIPTION
  Used by the M0-5 encode spike (ADR-0003). Must run as administrator; the owner runs it, never an agent.
  - Downloads the pinned release asset; fails closed if the SHA-256 differs (ADR-0003).
  - Refuses the driver unless its catalog (MttVDD.cat) has a valid Authenticode signature.
  - Writes C:\VirtualDisplayDriver\vdd_settings.xml (the driver's default settings path) with one monitor,
    mode 2960x1848 at 60 and 120 Hz.
  - Creates the root device Root\MttVDD and installs the driver on it through SetupAPI
    (what devcon.exe does), so no extra tool is downloaded. Running it twice does not create a second device.
  - -Uninstall removes the device and the driver package; C:\VirtualDisplayDriver is left for the owner to delete.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\vdd-install.ps1
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\vdd-install.ps1 -Uninstall
#>
[CmdletBinding()]
param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'

# Pinned release: VirtualDrivers/Virtual-Display-Driver 25.7.23 (latest stable, 2025-07-23), MIT, SignPath-signed.
# The asset named "x86" is the x64 (amd64) driver build; ARM64 has its own asset. SHA-256 from the GitHub release digest.
$Tag    = '25.7.23'
$Asset  = 'VirtualDisplayDriver-x86.Driver.Only.zip'
$Sha256 = 'e24210692b442b39af763536330ce78b423f19342b7a7792c26de3944e418b3a'
$Url    = "https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/$Tag/$Asset"
$HwId   = 'Root\MttVDD'
$ConfDir = 'C:\VirtualDisplayDriver'

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) { throw 'Run this from an administrator PowerShell.' }

function Get-VddDevice {
    Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.HardwareID -contains $HwId }
}

if ($Uninstall) {
    foreach ($d in Get-VddDevice) { pnputil /remove-device "$($d.InstanceId)" }
    $oem = pnputil /enum-drivers | Out-String
    # Each block of pnputil output names the published oemNN.inf and its original MttVDD.inf.
    foreach ($m in [regex]::Matches($oem, '(oem\d+\.inf)\s*\r?\n[^\r\n]*:\s*mttvdd\.inf', 'IgnoreCase')) {
        pnputil /delete-driver $m.Groups[1].Value /uninstall /force
    }
    Write-Host 'VDD removed.'
    exit 0
}

if (Get-VddDevice) { Write-Host 'VDD already installed; nothing to do.'; exit 0 }

$work = Join-Path $env:TEMP ('vdd-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$ProgressPreference = 'SilentlyContinue'
$zip = Join-Path $work $Asset
Invoke-WebRequest -Uri $Url -OutFile $zip -UseBasicParsing
$actual = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLowerInvariant()
if ($actual -ne $Sha256) { Remove-Item -Recurse -Force $work; throw "SHA-256 mismatch: got $actual, expected $Sha256" }
Expand-Archive -Path $zip -DestinationPath (Join-Path $work 'pkg')

$inf = Get-ChildItem -Path (Join-Path $work 'pkg') -Recurse -Filter 'MttVDD.inf' | Select-Object -First 1
$cat = Get-ChildItem -Path (Join-Path $work 'pkg') -Recurse -Filter 'MttVDD.cat' | Select-Object -First 1
if (-not $inf -or -not $cat) { throw 'MttVDD.inf or MttVDD.cat not found in the archive.' }
$sig = Get-AuthenticodeSignature -FilePath $cat.FullName
if ($sig.Status -ne 'Valid') { throw "Driver catalog signature is $($sig.Status), refusing to install." }
Write-Host "Catalog signed by: $($sig.SignerCertificate.Subject)"

New-Item -ItemType Directory -Path $ConfDir -Force | Out-Null
@'
<?xml version='1.0' encoding='utf-8'?>
<vdd_settings>
    <monitors><count>1</count></monitors>
    <gpu><friendlyname>default</friendlyname></gpu>
    <global>
        <g_refresh_rate>60</g_refresh_rate>
        <g_refresh_rate>120</g_refresh_rate>
    </global>
    <resolutions>
        <resolution><width>2960</width><height>1848</height><refresh_rate>120</refresh_rate></resolution>
    </resolutions>
    <logging><SendLogsThroughPipe>false</SendLogsThroughPipe><logging>false</logging><debuglogging>false</debuglogging></logging>
    <colour><SDR10bit>false</SDR10bit><HDRPlus>false</HDRPlus><ColourFormat>RGB</ColourFormat></colour>
    <cursor><HardwareCursor>true</HardwareCursor><CursorMaxY>128</CursorMaxY><CursorMaxX>128</CursorMaxX><AlphaCursorSupport>true</AlphaCursorSupport><XorCursorSupportLevel>2</XorCursorSupportLevel></cursor>
    <edid><CustomEdid>false</CustomEdid><PreventSpoof>false</PreventSpoof><EdidCeaOverride>false</EdidCeaOverride></edid>
</vdd_settings>
'@ | Set-Content -Path (Join-Path $ConfDir 'vdd_settings.xml') -Encoding UTF8

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class VddSetup {
    [StructLayout(LayoutKind.Sequential)]
    struct SP_DEVINFO_DATA { public uint cbSize; public Guid ClassGuid; public uint DevInst; public IntPtr Reserved; }
    [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool SetupDiGetINFClass(string inf, out Guid g, StringBuilder cls, uint size, IntPtr req);
    [DllImport("setupapi.dll", SetLastError = true)]
    static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid g, IntPtr hwnd);
    [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool SetupDiCreateDeviceInfo(IntPtr set, string name, ref Guid g, string desc, IntPtr hwnd, uint flags, ref SP_DEVINFO_DATA d);
    [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool SetupDiSetDeviceRegistryProperty(IntPtr set, ref SP_DEVINFO_DATA d, uint prop, byte[] buf, uint size);
    [DllImport("setupapi.dll", SetLastError = true)]
    static extern bool SetupDiCallClassInstaller(uint dif, IntPtr set, ref SP_DEVINFO_DATA d);
    [DllImport("setupapi.dll", SetLastError = true)]
    static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);
    [DllImport("newdev.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool UpdateDriverForPlugAndPlayDevices(IntPtr hwnd, string hwid, string inf, uint flags, out bool reboot);

    static void Check(bool ok, string what) {
        if (!ok) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error(), what);
    }

    // Same steps as `devcon install <inf> <hwid>`: create a root-enumerated device, then install the driver on it.
    public static bool Install(string inf, string hwid) {
        Guid g; var cls = new StringBuilder(64);
        Check(SetupDiGetINFClass(inf, out g, cls, 64, IntPtr.Zero), "SetupDiGetINFClass");
        IntPtr set = SetupDiCreateDeviceInfoList(ref g, IntPtr.Zero);
        if (set == new IntPtr(-1)) Check(false, "SetupDiCreateDeviceInfoList");
        try {
            var d = new SP_DEVINFO_DATA(); d.cbSize = (uint)Marshal.SizeOf(typeof(SP_DEVINFO_DATA));
            Check(SetupDiCreateDeviceInfo(set, cls.ToString(), ref g, null, IntPtr.Zero, 1 /*DICD_GENERATE_ID*/, ref d), "SetupDiCreateDeviceInfo");
            byte[] ids = Encoding.Unicode.GetBytes(hwid + "\0\0"); // REG_MULTI_SZ
            Check(SetupDiSetDeviceRegistryProperty(set, ref d, 1 /*SPDRP_HARDWAREID*/, ids, (uint)ids.Length), "SetupDiSetDeviceRegistryProperty");
            Check(SetupDiCallClassInstaller(0x19 /*DIF_REGISTERDEVICE*/, set, ref d), "DIF_REGISTERDEVICE");
        } finally { SetupDiDestroyDeviceInfoList(set); }
        bool reboot;
        Check(UpdateDriverForPlugAndPlayDevices(IntPtr.Zero, hwid, inf, 1 /*INSTALLFLAG_FORCE*/, out reboot), "UpdateDriverForPlugAndPlayDevices");
        return reboot;
    }
}
'@

$reboot = [VddSetup]::Install($inf.FullName, $HwId)
Remove-Item -Recurse -Force $work
Get-VddDevice | Format-Table -AutoSize Status, FriendlyName, InstanceId
if ($reboot) { Write-Host 'Windows asks for a restart to finish.' } else { Write-Host 'VDD installed.' }
