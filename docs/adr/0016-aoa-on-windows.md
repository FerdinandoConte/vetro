# ADR-0016: AOA on Windows needs a one-time driver install; M2 AOA becomes conditional

Status: Proposed · 2026-11-24 (to be accepted or changed by the lead at M0-8). Supersedes ADR-0007 decision 2 in part, on acceptance.

Resolves the open part of [ADR-0007](0007-usb-strategy.md) decision 2 and risk R4 (ARCHITECTURE §17). When the lead accepts this ADR at M0-8, ADR-0007's status gets "Decision 2 amended by ADR-0016"; ADR-0007 is not edited before then.

## Context
AOA (Android Open Accessory) needs two things on the Windows side:
1. **Start:** something on the PC sends the vendor control requests 51 (`GET_PROTOCOL`), 52 (`SEND_STRING`) and 53 (`START`) to the tablet while it is still in its normal USB mode. The tablet then re-enumerates as `18D1:2D00`, or as `18D1:2D01` (accessory + ADB, only when USB debugging is on).
2. **Bind:** after re-enumeration, a driver that user-mode code can open (WinUSB) must be bound to the accessory interface, so the engine can use its two bulk endpoints.

ADR-0007 wanted AOA as the "plug and go" path: no developer options and no toggle. The M0-7 spike checked both steps on the real device: a Galaxy Tab S8 Ultra (SM-X906B, Android 16) on a Windows 11 Home laptop with only VS Build Tools installed. The laptop also has the Samsung USB driver 2.21.4.0, a common install that comes with Samsung's PC tools. The probe is `tools/spikes/aoa-probe.ps1`. It installs nothing and needs no admin rights.

### Evidence from the device
Normal mode with tethering + ADB (`sys.usb.config=rndis,adb`) enumerates as `04E8:6864`:

| Interface | Class | Windows driver | MS OS 1.0 Extended Compat ID |
|---|---|---|---|
| 0, 1 | `E0/01/03` + `0A/00/00` (RNDIS) | `usbrndis6` | none |
| 2 | `FF/42/01` (ADB) | WinUSB (`winusb.sys`), bound by the Samsung ADB INF (catalog `ssudAdb.cat`), also in accessory mode | `WINUSB` |

The composite parent is Samsung's `dg_ssudbus`, not `usbccgp`, because the Samsung driver is installed.

- Through WinUSB on the ADB interface: `51` returned protocol version **2**. Then the six `52` strings and `53` all succeeded (Win32 error 0). The tablet re-enumerated as **`18D1:2D01`** within 8 s. ADB stayed usable.
- In accessory mode the device reports these descriptors:
  - interface 0 `FF/FF/00` (accessory, bulk `0x81`/`0x01`, 512 B);
  - interface 1 `FF/42/01` (ADB);
  - MS OS 1.0 string `MSFT100`, vendor code 1. Its Extended Compat ID lists **only interface 1 → `WINUSB`**;
  - a BOS descriptor with only the USB 2.0 Extension and SuperSpeed capabilities, so **no MS OS 2.0 platform capability**.
- Windows still bound WinUSB to the accessory interface (`SAMSUNG Android Open Accessory(AOA) Device`, class `AndroidAoADeviceClass`). The match came from the AOA INF (catalog `ssudaoa.cat`) in the **Samsung driver package**, which matches `USB\VID_18D1&PID_2D00` and `USB\VID_18D1&PID_2D01&MI_00`. The `MS_COMP_MTP` compatible ID that Windows shows on that interface is synthesized by Samsung's bus driver; the device itself does not report it.
- `adb shell svc usb setFunctions rndis` restored the original state (`04E8:6864`, same drivers).

### Evidence from primary documentation
- Microsoft, [Automatic installation of WinUSB](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/automatic-installation-of-winusb): the inbox `winusb.inf` matches only `USB\MS_COMP_WINUSB`. That ID must come from the device's OS descriptors, and on a composite device it is set per function.
- Microsoft, [WinUSB installation](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/winusb-installation): without those descriptors the vendor ships a custom INF that uses `Include=winusb.inf` / `Needs=WINUSB.NT` and comes with a **signed catalog**.
- Android, [AOA 1.0](https://source.android.com/docs/core/interaction/accessories/aoa) and [AOA 2.0](https://source.android.com/docs/core/interaction/accessories/aoa2):
  - The requests are vendor-type control transfers on endpoint 0. The probe used `bmRequestType` 0xC0 for 51 and 0x40 for 52 and 53.
  - `2D01` exists only with USB debugging on.
  - The accessory (the host here) drives the switch; the docs describe no way for the device to enter accessory mode by itself.
  - AOA and MTP cannot run at the same time.

## Findings (acceptance criteria of #8)
This section answers each acceptance criterion of #8 from the evidence above.

1. **How Windows can send the start requests.** User-mode code can send them only through an interface that is already bound to WinUSB. On a normal Android device the only such interface is ADB. ADB requires developer options, which defeats the point of AOA.
   - The other interfaces a user can select (file transfer, tethering, MIDI, PTP) are owned by WPD/MTP, NDIS or audio drivers. None of them exposes raw control transfers to user mode, and `usbccgp` offers no user-mode IOCTL for them either.
   - The switch cannot be started from the tablet, because AOA is host-initiated.
   - So on a stock PC **the start needs a driver installed once with admin rights**. Candidates:
     - [UsbDk](https://github.com/daynix/UsbDk): Apache-2.0. It detaches a device from its Windows drivers and gives user mode raw access. It is a kernel filter installed by MSI. *Not verified:* its README says only "signed by SHA-256 certificate", without naming the signer. On Windows 10 1607+ with Secure Boot, a new kernel driver loads only with a Microsoft attestation or WHQL signature, and it must be HVCI-compatible when memory integrity is on. Whether the current release meets both is unknown. *Not verified:* the releases page shows v1.00-22 as latest, but its date and year were not confirmed.
     - Our own KMDF filter: needs Microsoft attestation signing through Partner Center, which requires an EV certificate. That is out of reach for a free project.
   - The ADB-interface path was **proven** on the device with the Samsung bus driver (`dg_ssudbus`) as composite parent and WinUSB bound by the Samsung ADB INF (`ssudAdb.cat`). Stock `usbccgp` with the inbox WinUSB binding was not tested. It is fine for development and is how M2 can be developed and tested before the start problem is solved for users.
   - *Not tested on device:* a tablet with developer options off. The claim that no user-mode path exists there comes from the driver model above, not from a run.
2. **Automatic binding after re-enumeration: no** on stock Windows. The tablet reports `WINUSB` only for the ADB interface, and reports no MS OS 2.0 descriptors. The accessory interface of `2D00` and of `2D01&MI_00` therefore matches no inbox INF and stays without a driver. On this laptop it was bound only because the Samsung USB driver was installed. Vetro must not rely on that: it is Samsung-only and not present on every PC. *Not tested:* a clean PC without the Samsung driver. The conclusion follows from the descriptors read above and the Microsoft rule.
3. **Signed-driver path for the bind step.** Ship an **INF-only WinUSB package** matching `USB\VID_18D1&PID_2D00` and `USB\VID_18D1&PID_2D01&MI_00`. These IDs are Google's and the same for every Android OEM, so one INF covers all tablets. It references only the inbox, Microsoft-signed `winusb.sys`, so no new kernel binary is involved. The catalog is signed through SignPath. The engine installs it once with `pnputil /add-driver /install` from the elevated installer. Two points are not settled by the documentation and must be confirmed before M2 starts:
   - (a) that Windows 11 accepts an INF-only package whose catalog has an Authenticode signature that is not from Microsoft and chains to a public CA. Microsoft's pages do not state it. Zadig/libwdi proves nothing here, because it installs its own self-made certificate into Trusted Root and TrustedPublisher. Even when the package is accepted, `pnputil` may still show the "install this device software?" prompt unless the publisher is trusted. That would break M2's "no manual steps beyond UAC" exit criterion;
   - (b) that SignPath Foundation agrees to sign a driver catalog.

## Decision
1. AOA on Windows is **not plug-and-go**. Before first use it needs a one-time, admin-elevated "Vetro USB driver" install with two parts: the WinUSB INF for `18D1:2D00`/`2D01&MI_00` (bind) and a start helper (UsbDk or equivalent).
2. Since the target tablet offers USB tethering (M0-3), **USB tethering stays the default cable path** from M1 on.
3. M2's AOA work items become **conditional on a short M2 pre-spike**. It runs on a clean Windows 11 machine with memory integrity (HVCI) on and without the Samsung driver, and must show three things:
   - (a) UsbDk installs, sends 51/52/53 to a tablet with developer options off, and releases the device;
   - (b) the SignPath-signed INF-only WinUSB package installs and binds;
   - (c) the accessory bulk endpoints carry the QUIC adapter's traffic.
   If (a) or (b) fails, AOA moves to `ROADMAP.md` (after v1.0) and M2 is re-scoped without it.
4. Development builds may use the ADB-interface start path (developer options on), proven with the Samsung bus driver; stock `usbccgp` not tested. It is never required from users ([ADR-0007](0007-usb-strategy.md) alternatives).

## Consequences
- The "plug and go, no developer options" goal of ADR-0007 holds only after one admin install. It cannot hold on a stock PC.
- The installer gains an optional driver component. A kernel filter (UsbDk) widens the attack surface, so the security review must cover it.
- The spike also saw that this cable and port pair enumerates at USB 2.0 High-Speed (bcdUSB 2.10, 512-byte bulk packets), even though the tablet advertises SuperSpeed. That is enough for 50–150 Mbit/s, but M1 should record the cable used for HIL runs.

## Alternatives
- **Rely on the Samsung USB driver:** it binds the accessory interface, but it is Samsung-only and does not solve the start step. Rejected as the main path; it can still help as a detected bonus.
- **Device-level WinUSB INF for each OEM's normal-mode VID/PID:** it replaces MTP/ADB on the user's device and needs one entry per tablet model. Rejected.
- **Own KMDF filter driver:** needs Microsoft attestation signing with an EV certificate. Rejected for cost.
- **libusb-win32 / libusbK filter drivers:** old and not attestation-signed for current Windows. Rejected.
- **Drop AOA now:** possible, but the M2 pre-spike is cheap, so the evidence decides.
