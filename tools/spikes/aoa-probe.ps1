# M0-7 spike probe (see docs/adr/0016-aoa-on-windows.md). Throwaway quality; installs nothing, needs no admin.
# Talks to a USB interface that is already bound to WinUSB, found by its device interface GUID.
#   -Mode Protocol     AOA request 51 only (read-only).
#   -Mode Start        AOA 51 + 52 (strings) + 53: the device re-enumerates as 18D1:2D00/2D01.
#   -Mode Descriptors  device/config/BOS descriptors and MS OS 1.0 Extended Compat ID.
# Default GUID is the ADB interface class used by Google/Samsung ADB drivers; run `adb kill-server` first
# because adb holds that WinUSB handle exclusively. Undo Start with `adb shell svc usb setFunctions <original>`.
param(
  [ValidateSet('Protocol', 'Start', 'Descriptors')] [string]$Mode = 'Protocol',
  [Guid]$InterfaceGuid = 'F72FE0D4-CBCB-407d-8814-9ED673D0DD6B'
)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices; using Microsoft.Win32.SafeHandles;
public static class AoaProbe {
  [StructLayout(LayoutKind.Sequential)] public struct DID { public int cb; public Guid g; public int f; public IntPtr r; }
  [StructLayout(LayoutKind.Sequential, Pack=1)] public struct SP { public byte T; public byte R; public ushort V; public ushort I; public ushort L; }
  [DllImport("setupapi.dll", SetLastError=true)] static extern IntPtr SetupDiGetClassDevs(ref Guid g, IntPtr e, IntPtr h, int f);
  [DllImport("setupapi.dll", SetLastError=true)] static extern bool SetupDiEnumDeviceInterfaces(IntPtr s, IntPtr d, ref Guid g, int i, ref DID did);
  [DllImport("setupapi.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool SetupDiGetDeviceInterfaceDetail(IntPtr s, ref DID did, IntPtr buf, int sz, out int req, IntPtr di);
  [DllImport("setupapi.dll")] static extern bool SetupDiDestroyDeviceInfoList(IntPtr s);
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] public static extern SafeFileHandle CreateFile(string n, uint a, uint s, IntPtr sa, uint c, uint f, IntPtr t);
  [DllImport("winusb.dll", SetLastError=true)] public static extern bool WinUsb_Initialize(SafeFileHandle h, out IntPtr w);
  [DllImport("winusb.dll", SetLastError=true)] public static extern bool WinUsb_ControlTransfer(IntPtr w, SP s, byte[] b, int l, out int n, IntPtr o);
  [DllImport("winusb.dll")] public static extern bool WinUsb_Free(IntPtr w);
  public static string[] Paths(Guid g) {
    var list = new System.Collections.Generic.List<string>();
    IntPtr s = SetupDiGetClassDevs(ref g, IntPtr.Zero, IntPtr.Zero, 0x12); // DIGCF_PRESENT | DIGCF_DEVICEINTERFACE
    for (int i = 0; ; i++) {
      var did = new DID(); did.cb = Marshal.SizeOf(did);
      if (!SetupDiEnumDeviceInterfaces(s, IntPtr.Zero, ref g, i, ref did)) break;
      int req; SetupDiGetDeviceInterfaceDetail(s, ref did, IntPtr.Zero, 0, out req, IntPtr.Zero);
      IntPtr buf = Marshal.AllocHGlobal(req); Marshal.WriteInt32(buf, IntPtr.Size == 8 ? 8 : 6);
      if (SetupDiGetDeviceInterfaceDetail(s, ref did, buf, req, out req, IntPtr.Zero)) list.Add(Marshal.PtrToStringUni(buf + 4));
      Marshal.FreeHGlobal(buf);
    }
    SetupDiDestroyDeviceInfoList(s);
    return list.ToArray();
  }
}
'@
$paths = [AoaProbe]::Paths($InterfaceGuid)
"Interfaces for {$InterfaceGuid}: $($paths.Count)"
if ($paths.Count -eq 0) { return }
# GENERIC_READ|GENERIC_WRITE, share read/write, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL|FILE_FLAG_OVERLAPPED (WinUSB requires it)
$h = [AoaProbe]::CreateFile($paths[0], [uint32]3221225472, 3, [IntPtr]::Zero, 3, 0x40000080, [IntPtr]::Zero)
if ($h.IsInvalid) { throw "CreateFile failed: Win32 error $([Runtime.InteropServices.Marshal]::GetLastWin32Error()) (adb still running?)" }
$w = [IntPtr]::Zero
if (-not [AoaProbe]::WinUsb_Initialize($h, [ref]$w)) { throw "WinUsb_Initialize failed: Win32 error $([Runtime.InteropServices.Marshal]::GetLastWin32Error())" }

function Invoke-Ctl([byte]$Type, [byte]$Req, [uint16]$Value, [uint16]$Index, [byte[]]$Buf) {
  $sp = New-Object AoaProbe+SP; $sp.T = $Type; $sp.R = $Req; $sp.V = $Value; $sp.I = $Index; $sp.L = $Buf.Length
  $n = 0; $ok = [AoaProbe]::WinUsb_ControlTransfer($w, $sp, $Buf, $Buf.Length, [ref]$n, [IntPtr]::Zero)
  [pscustomobject]@{ ok = $ok; err = [Runtime.InteropServices.Marshal]::GetLastWin32Error(); n = $n; b = $Buf }
}
function Get-Desc([uint16]$Value, [int]$Len) { Invoke-Ctl 0x80 6 $Value 0 ([byte[]]::new($Len)) }  # standard GET_DESCRIPTOR

try {
  if ($Mode -ne 'Descriptors') {
    $p = Invoke-Ctl 0xC0 51 0 0 ([byte[]]::new(2))  # ACCESSORY_GET_PROTOCOL: IN | vendor | device
    "51 GET_PROTOCOL: ok=$($p.ok) err=$($p.err) version=$($p.b[0] + 256 * $p.b[1])"
  }
  if ($Mode -eq 'Start') {
    $strings = 'Vetro contributors', 'Vetro', 'Vetro M0-7 AOA probe', '0.0.1', 'https://example.invalid/vetro', '0'
    for ($i = 0; $i -lt $strings.Count; $i++) {
      $r = Invoke-Ctl 0x40 52 0 $i ([Text.Encoding]::UTF8.GetBytes($strings[$i] + "`0"))  # SEND_STRING: OUT | vendor | device
      "52 SEND_STRING[$i]: ok=$($r.ok) err=$($r.err) bytes=$($r.n)"
    }
    $r = Invoke-Ctl 0x40 53 0 0 ([byte[]]::new(0))
    "53 START: ok=$($r.ok) err=$($r.err) (device now re-enumerates)"
  }
  if ($Mode -eq 'Descriptors') {
    $d = Get-Desc 0x0100 18
    "Device: bcdUSB=$('{0:X4}' -f ($d.b[2] + 256 * $d.b[3])) VID:PID=$('{0:X4}:{1:X4}' -f ($d.b[8] + 256 * $d.b[9]), ($d.b[10] + 256 * $d.b[11]))"
    $c = Get-Desc 0x0200 255
    for ($o = 0; $o -lt $c.n -and $c.b[$o] -gt 0; $o += $c.b[$o]) {
      if ($c.b[$o + 1] -eq 4) { "  iface $($c.b[$o + 2]) class=$('{0:X2}/{1:X2}/{2:X2}' -f $c.b[$o + 5], $c.b[$o + 6], $c.b[$o + 7])" }
      elseif ($c.b[$o + 1] -eq 5) { "    ep 0x$('{0:X2}' -f $c.b[$o + 2]) maxPacket=$($c.b[$o + 4] + 256 * $c.b[$o + 5])" }
    }
    $os = Get-Desc 0x03EE 18  # MS OS 1.0 string descriptor
    if ($os.ok -and $os.n -ge 18) {
      "MS OS 1.0: '$([Text.Encoding]::Unicode.GetString($os.b, 2, 14))' vendorCode=$($os.b[16])"
      $cid = Invoke-Ctl 0xC0 $os.b[16] 0 4 ([byte[]]::new(256))  # Extended Compat ID feature descriptor
      for ($o = 16; $o + 24 -le $cid.n; $o += 24) { "  Extended Compat ID: iface $($cid.b[$o]) -> '$([Text.Encoding]::ASCII.GetString($cid.b, $o + 2, 8).Trim([char]0))'" }
    } else { "MS OS 1.0: none (ok=$($os.ok) err=$($os.err))" }
    $bh = Get-Desc 0x0F00 5
    if ($bh.ok) {
      $bos = Get-Desc 0x0F00 ($bh.b[2] + 256 * $bh.b[3])
      for ($o = 5; $o -lt $bos.n -and $bos.b[$o] -gt 0; $o += $bos.b[$o]) {
        $cap = "  BOS capability type=$($bos.b[$o + 2])"
        if ($bos.b[$o + 2] -eq 5) { $cap += " platform uuid=$([Guid]::new([byte[]]$bos.b[($o + 4)..($o + 19)]))" }  # MS OS 2.0 is uuid d8dd60df-...
        $cap
      }
    } else { "BOS: none" }
  }
} finally { [AoaProbe]::WinUsb_Free($w) | Out-Null; $h.Dispose() }
