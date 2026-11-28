//! The FFmpeg hardware encoder on a D3D11 device (ARCHITECTURE section 5.3), zero-copy.
//!
//! Input surfaces come from an FFmpeg D3D11 frame pool (NV12 texture array, decoder-bound)
//! created on the caller's `ID3D11Device`; the caller fills one on the GPU (capture's
//! colour conversion, or [`crate::d3d11::Bars`]) and the encoder reads it in place. QSV reads
//! QSV surfaces, so its pool is derived from the D3D11 one: the same textures, mapped, never
//! copied. No staging texture and no CPU readback exist anywhere in this path.

use crate::backend::Backend;
use crate::d3d11::Device;
use crate::ffi::{self, AVBufferRef, AVCodecContext, AVFrame, AVHWFramesContext, AVPacket, Lib};
use crate::{Encoder, Error, Settings};
use std::ffi::{CString, c_void};
use std::ptr::{self, null_mut};
use std::thread::JoinHandle;
use vetro_protocol::messages::Profile;
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_DECODER, ID3D11Texture2D};
use windows::core::Interface;

/// Input surfaces in the pool: one being filled, one the encoder still references, spares.
const POOL: i32 = 4;

/// One encoder input: slice `index` of the pool's texture array (NV12, the stream's size).
pub struct Surface {
    pub texture: ID3D11Texture2D,
    pub index: u32,
}

/// One opened FFmpeg encoder with its surface pool.
struct Session {
    lib: &'static Lib,
    backend: Backend,
    settings: Settings,
    device: Device,
    ctx: *mut AVCodecContext,
    d3d_device: *mut AVBufferRef,
    d3d_frames: *mut AVBufferRef,
    qsv_device: *mut AVBufferRef,
    qsv_frames: *mut AVBufferRef,
    input: *mut AVFrame,
    mapped: *mut AVFrame,
    packet: *mut AVPacket,
    pts: i64,
}

// SAFETY: every FFmpeg object and COM pointer here is owned by this value and used only through
// `&mut self`, so never from two threads at once. FFmpeg contexts are not bound to the thread
// that made them, and the D3D11 device is free-threaded with multithread protection on
// (`Device::open`).
unsafe impl Send for Session {}

fn check(lib: &Lib, what: &str, r: i32) -> Result<(), Error> {
    if r < 0 {
        Err(Error(lib.err(what, r)))
    } else {
        Ok(())
    }
}

impl Session {
    fn open_backend(device: &Device, backend: Backend, settings: &Settings) -> Result<Self, Error> {
        let lib = ffi::lib().map_err(Error)?;
        let mut e = Self {
            lib,
            backend,
            settings: *settings,
            device: device.clone(),
            ctx: null_mut(),
            d3d_device: null_mut(),
            d3d_frames: null_mut(),
            qsv_device: null_mut(),
            qsv_frames: null_mut(),
            input: null_mut(),
            mapped: null_mut(),
            packet: null_mut(),
            pts: 0,
        };
        // On error `e` is dropped, which frees whatever was made so far.
        e.init()?;
        Ok(e)
    }

    fn init(&mut self) -> Result<(), Error> {
        let lib = self.lib;
        let s = self.settings;
        let (w, h) = (to_int(s.width)?, to_int(s.height)?);
        // SAFETY: (whole block) each FFmpeg object is checked for null right after it is made
        // and owned by `self` (freed in Drop); pointer fields are written only on objects
        // FFmpeg just allocated and has not initialised yet, as its API requires, through the
        // layouts declared in `ffi` (checked by major version at load).
        unsafe {
            // D3D11 device context around the caller's device.
            self.d3d_device = (lib.av_hwdevice_ctx_alloc)(ffi::AV_HWDEVICE_TYPE_D3D11VA);
            if self.d3d_device.is_null() {
                return Err(Error("av_hwdevice_ctx_alloc failed".into()));
            }
            let hw = (*self.d3d_device).data.cast::<ffi::AVHWDeviceContext>();
            let d3d = (*hw).hwctx.cast::<ffi::AVD3D11VADeviceContext>();
            // FFmpeg releases this reference when the device context is freed.
            (*d3d).device = self.device.device.clone().into_raw();
            check(
                lib,
                "D3D11 device context",
                (lib.av_hwdevice_ctx_init)(self.d3d_device),
            )?;

            // The input pool.
            self.d3d_frames = (lib.av_hwframe_ctx_alloc)(self.d3d_device);
            if self.d3d_frames.is_null() {
                return Err(Error("av_hwframe_ctx_alloc failed".into()));
            }
            let fc = (*self.d3d_frames).data.cast::<AVHWFramesContext>();
            (*fc).format = lib.pix_d3d11;
            (*fc).sw_format = lib.pix_nv12;
            (*fc).width = w;
            (*fc).height = h;
            (*fc).initial_pool_size = POOL;
            let dfc = (*fc).hwctx.cast::<ffi::AVD3D11VAFramesContext>();
            // Decoder binding: the Intel driver refuses NV12 texture arrays with any other
            // binding (render target, shader resource, none), and QSV needs an array. So
            // capture's colour conversion (M1-9) renders into its own NV12 target and copies
            // it into the slot on the GPU.
            (*dfc).bind_flags = D3D11_BIND_DECODER.0 as u32;
            check(
                lib,
                "D3D11 frame pool",
                (lib.av_hwframe_ctx_init)(self.d3d_frames),
            )?;

            let (pix_fmt, frames) = if self.backend.takes_d3d11_frames() {
                (lib.pix_d3d11, self.d3d_frames)
            } else {
                check(
                    lib,
                    "QSV device from D3D11",
                    (lib.av_hwdevice_ctx_create_derived)(
                        &mut self.qsv_device,
                        ffi::AV_HWDEVICE_TYPE_QSV,
                        self.d3d_device,
                        0,
                    ),
                )?;
                check(
                    lib,
                    "QSV frames from the D3D11 pool",
                    (lib.av_hwframe_ctx_create_derived)(
                        &mut self.qsv_frames,
                        lib.pix_qsv,
                        self.qsv_device,
                        self.d3d_frames,
                        ffi::AV_HWFRAME_MAP_READ_WRITE,
                    ),
                )?;
                (lib.pix_qsv, self.qsv_frames)
            };

            let name = CString::new(self.backend.encoder_name(s.profile))
                .map_err(|_| Error("name".into()))?;
            let codec = lib.encoder(&name).map_err(Error)?;
            self.ctx = (lib.avcodec_alloc_context3)(codec);
            if self.ctx.is_null() {
                return Err(Error("avcodec_alloc_context3 failed".into()));
            }
            let obj = self.ctx.cast::<c_void>();
            lib.set(obj, "video_size", &format!("{w}x{h}"))
                .map_err(Error)?;
            lib.set_int(obj, "pixel_format", i64::from(pix_fmt))
                .map_err(Error)?;
            // One tick per frame at the client's refresh (mHz / 1000 = frames per second); the
            // BRC sizes frames from `framerate`.
            let fps = ffi::AVRational {
                num: to_int(s.refresh_mhz)?,
                den: 1000,
            };
            lib.write_field(self.ctx, lib.offsets.framerate, fps);
            lib.write_field(
                self.ctx,
                lib.offsets.time_base,
                ffi::AVRational {
                    num: fps.den,
                    den: fps.num,
                },
            );
            for (k, v) in self.backend.options(s.profile) {
                lib.set(obj, k, &v).map_err(Error)?;
            }
            self.apply_rate(s.bitrate_kbps)?;
            let r = (lib.av_buffer_ref)(frames);
            if r.is_null() {
                return Err(Error("av_buffer_ref failed".into()));
            }
            // Owned by the codec context from here on (avcodec_free_context unrefs it).
            lib.write_field(self.ctx, lib.offsets.hw_frames_ctx, r);
            check(
                lib,
                self.backend.encoder_name(s.profile),
                (lib.avcodec_open2)(self.ctx, codec, ptr::null_mut()),
            )?;

            self.input = (lib.av_frame_alloc)();
            self.mapped = (lib.av_frame_alloc)();
            self.packet = (lib.av_packet_alloc)();
            if self.input.is_null() || self.mapped.is_null() || self.packet.is_null() {
                return Err(Error("frame/packet allocation failed".into()));
            }
        }
        Ok(())
    }

    fn apply_rate(&mut self, kbps: u32) -> Result<(), Error> {
        for (k, v) in self.backend.rate_options(kbps, self.settings.refresh_mhz) {
            // SAFETY: `ctx` is a live codec context.
            unsafe { self.lib.set_int(self.ctx.cast(), k, v) }.map_err(Error)?;
        }
        self.settings.bitrate_kbps = kbps;
        Ok(())
    }

    fn encode(
        &mut self,
        fill: &mut dyn FnMut(&Surface),
        force_keyframe: bool,
        out: &mut Vec<u8>,
    ) -> Result<bool, Error> {
        let lib = self.lib;
        out.clear();
        // SAFETY: `input`, `mapped` and `packet` are live and owned by `self`; each is unref'd
        // before reuse. After `av_hwframe_get_buffer`, a D3D11 frame's data[0] is its
        // `ID3D11Texture2D*` and data[1] its array slice (libavutil/hwcontext_d3d11va.h); the
        // texture is borrowed only for this call, while the frame holds a reference.
        unsafe {
            check(
                lib,
                "pool surface",
                (lib.av_hwframe_get_buffer)(self.d3d_frames, self.input, 0),
            )?;
            let raw = (*self.input).data[0].cast::<c_void>();
            let Some(texture) = ID3D11Texture2D::from_raw_borrowed(&raw).cloned() else {
                (lib.av_frame_unref)(self.input);
                return Err(Error("pool frame without a texture".into()));
            };
            // Truncation impossible: the slice index is below POOL.
            let index = (*self.input).data[1] as usize as u32;
            fill(&Surface { texture, index });

            let send = if self.backend.takes_d3d11_frames() {
                self.input
            } else {
                (*self.mapped).format = lib.pix_qsv;
                (*self.mapped).hw_frames_ctx = (lib.av_buffer_ref)(self.qsv_frames);
                let r =
                    (lib.av_hwframe_map)(self.mapped, self.input, ffi::AV_HWFRAME_MAP_READ_WRITE);
                if r < 0 {
                    (lib.av_frame_unref)(self.mapped);
                    (lib.av_frame_unref)(self.input);
                    return Err(Error(lib.err("map D3D11 → QSV", r)));
                }
                self.mapped
            };
            (*send).pts = self.pts;
            (*send).pict_type = if force_keyframe {
                ffi::AV_PICTURE_TYPE_I
            } else {
                ffi::AV_PICTURE_TYPE_NONE
            };
            self.pts += 1;
            let r = (lib.avcodec_send_frame)(self.ctx, send);
            // The encoder holds its own references to what it still needs.
            (lib.av_frame_unref)(self.mapped);
            (lib.av_frame_unref)(self.input);
            check(lib, "avcodec_send_frame", r)?;

            let mut keyframe = false;
            loop {
                let r = (lib.avcodec_receive_packet)(self.ctx, self.packet);
                if r == ffi::AVERROR_EAGAIN {
                    break;
                }
                check(lib, "avcodec_receive_packet", r)?;
                let p = &*self.packet;
                let len = usize::try_from(p.size).unwrap_or(0);
                if !p.data.is_null() && len > 0 {
                    out.extend_from_slice(std::slice::from_raw_parts(p.data, len));
                }
                keyframe |= p.flags & ffi::AV_PKT_FLAG_KEY != 0;
                (lib.av_packet_unref)(self.packet);
            }
            Ok(keyframe)
        }
    }
}

/// The FFmpeg encoder of the render adapter's vendor, with zero-copy D3D11 input.
///
/// A bitrate change goes to the running encoder where the backend allows it
/// ([`Backend::retargets_live`]). Otherwise a replacement encoder at the new bitrate is opened
/// on a helper thread (about 200 ms for QSV on Iris Xe) while frames keep flowing at the old
/// rate, and is swapped in at the next frame once ready; its first frame is an IDR. Only one
/// replacement is in flight: if the target moves again meanwhile, the next one starts after
/// the swap with the latest target. The old encoder is closed on the helper thread too.
pub struct FfmpegEncoder {
    live: Session,
    target_kbps: u32,
    replacement: Option<JoinHandle<Result<Session, Error>>>,
}

impl FfmpegEncoder {
    /// Opens the backend chosen by the device's vendor, for `settings`.
    pub fn open(device: &Device, settings: &Settings) -> Result<Self, Error> {
        Self::open_backend(device, Backend::for_vendor(device.vendor_id), settings)
    }

    pub fn open_backend(
        device: &Device,
        backend: Backend,
        settings: &Settings,
    ) -> Result<Self, Error> {
        Ok(Self {
            live: Session::open_backend(device, backend, settings)?,
            target_kbps: settings.bitrate_kbps,
            replacement: None,
        })
    }

    pub fn backend(&self) -> Backend {
        self.live.backend
    }

    pub fn device(&self) -> &Device {
        &self.live.device
    }

    /// Where the FFmpeg DLLs were loaded from.
    pub fn ffmpeg_dir() -> Result<std::path::PathBuf, Error> {
        ffi::lib().map(|l| l.dir.clone()).map_err(Error)
    }

    fn start_replacement(&mut self) -> Result<(), Error> {
        let (device, backend) = (self.live.device.clone(), self.live.backend);
        let settings = Settings {
            bitrate_kbps: self.target_kbps,
            ..self.live.settings
        };
        let t = std::thread::Builder::new()
            .name("vetro-encoder-open".into())
            .spawn(move || Session::open_backend(&device, backend, &settings))
            .map_err(|e| Error(format!("encoder helper thread: {e}")))?;
        self.replacement = Some(t);
        Ok(())
    }

    /// Swaps in a finished replacement; never waits for one still opening.
    fn swap_if_ready(&mut self) -> Result<(), Error> {
        if !self
            .replacement
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            return Ok(());
        }
        let Some(t) = self.replacement.take() else {
            return Ok(());
        };
        let new = t
            .join()
            .map_err(|_| Error("encoder helper thread panicked".into()))??;
        let old = std::mem::replace(&mut self.live, new);
        // Closing a QSV session takes milliseconds: not on the frame thread.
        let _ = std::thread::Builder::new()
            .name("vetro-encoder-close".into())
            .spawn(move || drop(old));
        if self.live.settings.bitrate_kbps != self.target_kbps {
            self.start_replacement()?;
        }
        Ok(())
    }
}

impl Encoder for FfmpegEncoder {
    type Surface = Surface;

    fn encode(
        &mut self,
        fill: &mut dyn FnMut(&Surface),
        force_keyframe: bool,
        out: &mut Vec<u8>,
    ) -> Result<bool, Error> {
        self.swap_if_ready()?;
        self.live.encode(fill, force_keyframe, out)
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), Error> {
        self.target_kbps = kbps;
        if self.live.backend.retargets_live() {
            self.live.apply_rate(kbps)
        } else if self.replacement.is_none() && self.live.settings.bitrate_kbps != kbps {
            self.start_replacement()
        } else {
            Ok(())
        }
    }

    fn profile(&self) -> Profile {
        self.live.settings.profile
    }

    /// The bitrate of the encoder producing frames now (a replacement may be on its way).
    fn bitrate_kbps(&self) -> u32 {
        self.live.settings.bitrate_kbps
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let lib = self.lib;
        // SAFETY: each pointer is null or owned by `self`; the FFmpeg free functions accept a
        // pointer to null and null it out. The codec context goes first (it references the
        // frames), then frames before the devices they were made on.
        unsafe {
            (lib.av_packet_free)(&mut self.packet);
            (lib.av_frame_free)(&mut self.mapped);
            (lib.av_frame_free)(&mut self.input);
            (lib.avcodec_free_context)(&mut self.ctx);
            (lib.av_buffer_unref)(&mut self.qsv_frames);
            (lib.av_buffer_unref)(&mut self.qsv_device);
            (lib.av_buffer_unref)(&mut self.d3d_frames);
            (lib.av_buffer_unref)(&mut self.d3d_device);
        }
    }
}

fn to_int(v: u32) -> Result<i32, Error> {
    i32::try_from(v).map_err(|_| Error(format!("{v} out of range")))
}
