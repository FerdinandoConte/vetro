//! The D3D11 device the encoder runs on, and a GPU test pattern (Windows).
//!
//! The encoder must run on the adapter that renders the virtual monitor (ARCHITECTURE section
//! 5.1), so the device is opened by adapter LUID; capture (M1-9) uses the same device, which is
//! what makes the hand-over zero-copy.

use crate::Error;
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_IMMUTABLE, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIAdapter1, IDXGIFactory1,
};
use windows::core::Interface;

/// A D3D11 device on one adapter.
#[derive(Clone)]
pub struct Device {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    /// PCI vendor id of the adapter (selects the encoder backend).
    pub vendor_id: u32,
    pub luid: LUID,
    pub adapter: String,
}

// SAFETY: ID3D11Device is free-threaded. The immediate context is not, but `open` turns on its
// multithread protection, so calls from different threads are serialised by D3D11 itself.
unsafe impl Send for Device {}
// SAFETY: as for Send.
unsafe impl Sync for Device {}

fn win(what: &'static str) -> impl FnOnce(windows::core::Error) -> Error {
    move |e| Error(format!("{what}: {e}"))
}

impl Device {
    /// Opens a device on the adapter with `luid`, or on the first hardware adapter when `None`.
    pub fn open(luid: Option<LUID>) -> Result<Self, Error> {
        // SAFETY: plain factory creation.
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(win("DXGI"))?;
        for i in 0.. {
            // SAFETY: enumeration ends with DXGI_ERROR_NOT_FOUND, which ends the loop.
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else {
                break;
            };
            // SAFETY: a live adapter.
            let desc = unsafe { adapter.GetDesc1() }.map_err(win("adapter description"))?;
            let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
            let wanted = match luid {
                Some(l) => {
                    l.LowPart == desc.AdapterLuid.LowPart && l.HighPart == desc.AdapterLuid.HighPart
                }
                None => !software,
            };
            if wanted {
                let name = String::from_utf16_lossy(&desc.Description);
                let name = name.trim_end_matches('\0').to_owned();
                return Self::on(&adapter, desc.VendorId, desc.AdapterLuid, name);
            }
        }
        Err(Error("no matching hardware adapter".into()))
    }

    fn on(
        adapter: &IDXGIAdapter1,
        vendor_id: u32,
        luid: LUID,
        adapter_name: String,
    ) -> Result<Self, Error> {
        let (mut device, mut context) = (None, None);
        // SAFETY: out-pointers to locals; an explicit adapter needs DRIVER_TYPE_UNKNOWN.
        unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(win("D3D11CreateDevice"))?;
        let device: ID3D11Device = device.ok_or(Error("no device".into()))?;
        let context: ID3D11DeviceContext = context.ok_or(Error("no context".into()))?;
        // FFmpeg's QSV/MF paths use the immediate context from their own threads; FFmpeg's own
        // D3D11 devices turn this on for the same reason.
        let mt: ID3D11Multithread = context.cast().map_err(win("ID3D11Multithread"))?;
        // SAFETY: a live interface; the return value is the previous state.
        let _ = unsafe { mt.SetMultithreadProtected(true) };
        Ok(Self {
            device,
            context,
            vendor_id,
            luid,
            adapter: adapter_name,
        })
    }
}

/// Moving vertical bars, entirely on the GPU: eight NV12 textures (one per phase) uploaded once,
/// then each frame is one GPU copy into the encoder's surface. Nothing is read back.
pub struct Bars {
    phases: Vec<ID3D11Texture2D>,
}

/// Bar width in pixels; the pattern shifts by one bar per frame.
pub const BAR: u32 = 32;

/// Luma of the bar covering column `x` in frame `seq` (also what a decoder must see).
pub fn bar_luma(seq: u32, x: u32) -> u8 {
    let bar = (x / BAR + seq % 8) % 8;
    // Truncation intended: 8 levels from 16 to 226.
    (16 + bar * 30) as u8
}

impl Bars {
    pub fn new(dev: &Device, width: u32, height: u32) -> Result<Self, Error> {
        let (w, h) = (width as usize, height as usize);
        let mut phases = Vec::with_capacity(8);
        let mut data = vec![0u8; w * h * 3 / 2];
        for phase in 0..8 {
            let (luma, chroma) = data.split_at_mut(w * h);
            for row in luma.chunks_exact_mut(w) {
                for (x, px) in row.iter_mut().enumerate() {
                    *px = bar_luma(phase, x as u32);
                }
            }
            chroma.fill(128);
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_IMMUTABLE,
                // NV12 needs a bind flag on most drivers.
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            // NV12 initial data: the chroma rows follow the luma rows at the same pitch.
            let init = D3D11_SUBRESOURCE_DATA {
                pSysMem: data.as_ptr().cast(),
                SysMemPitch: width,
                SysMemSlicePitch: 0,
            };
            let mut tex = None;
            // SAFETY: `init` points at w×h×1.5 bytes that outlive the call.
            unsafe {
                dev.device
                    .CreateTexture2D(&desc, Some(&init), Some(&mut tex))
            }
            .map_err(win("pattern texture"))?;
            phases.push(tex.ok_or(Error("no texture".into()))?);
        }
        Ok(Self { phases })
    }

    /// Copies frame `seq` into slice `index` of `dst` (same size, NV12), on the GPU.
    pub fn draw(&self, dev: &Device, dst: &ID3D11Texture2D, index: u32, seq: u32) {
        let src = &self.phases[(seq % 8) as usize];
        // SAFETY: both textures live on `dev`, are NV12 and the same size; `index` is a valid
        // array slice of `dst` (the encoder's pool) with one mip level.
        unsafe {
            dev.context
                .CopySubresourceRegion(dst, index, 0, 0, 0, src, 0, None)
        };
    }
}
