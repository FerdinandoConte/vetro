//! The slice of FFmpeg's C API the encoder uses, loaded at run time from the LGPL shared build
//! (ADR-0005, ADR-0014): no import library, no static link, no bindings crate.
//!
//! Struct layouts are those of the pinned build's headers (`tools/fetch-ffmpeg.ps1`): libavutil
//! 60, libavcodec 62. They are stable within a major version (FFmpeg only appends fields), so
//! [`Lib::load`] refuses any other major. Only *prefixes* are declared, up to the last field we
//! touch; FFmpeg allocates every struct, we never create or move one. `AVCodecContext` is
//! opaque: its fields are set through AVOptions, and the three without one (`time_base`,
//! `framerate`, `hw_frames_ctx`) are found from the offsets of neighbouring AVOptions
//! ([`Lib::find_offsets`]).

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW,
};
use windows::core::{HSTRING, PCSTR};

pub const AVUTIL_MAJOR: u32 = 60;
pub const AVCODEC_MAJOR: u32 = 62;
const AVUTIL_DLL: &str = "avutil-60.dll";
const AVCODEC_DLL: &str = "avcodec-62.dll";

pub const AV_OPT_SEARCH_CHILDREN: c_int = 1;
pub const AV_HWDEVICE_TYPE_QSV: c_int = 5;
pub const AV_HWDEVICE_TYPE_D3D11VA: c_int = 7;
pub const AV_HWFRAME_MAP_READ_WRITE: c_int = 3;
pub const AV_PICTURE_TYPE_NONE: c_int = 0;
pub const AV_PICTURE_TYPE_I: c_int = 1;
pub const AV_PKT_FLAG_KEY: c_int = 1;
/// `AVERROR(EAGAIN)`; FFmpeg's Windows builds use EAGAIN = 11.
pub const AVERROR_EAGAIN: c_int = -11;

#[repr(C)]
pub struct AVBufferRef {
    pub buffer: *mut c_void,
    pub data: *mut u8,
    pub size: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AVRational {
    pub num: c_int,
    pub den: c_int,
}

/// Prefix of `AVFrame` (libavutil/frame.h), through `hw_frames_ctx`.
#[repr(C)]
pub struct AVFrame {
    pub data: [*mut u8; 8],
    pub linesize: [c_int; 8],
    pub extended_data: *mut *mut u8,
    pub width: c_int,
    pub height: c_int,
    pub nb_samples: c_int,
    pub format: c_int,
    pub pict_type: c_int,
    pub sample_aspect_ratio: AVRational,
    pub pts: i64,
    pub pkt_dts: i64,
    pub time_base: AVRational,
    pub quality: c_int,
    pub opaque: *mut c_void,
    pub repeat_pict: c_int,
    pub sample_rate: c_int,
    pub buf: [*mut AVBufferRef; 8],
    pub extended_buf: *mut *mut AVBufferRef,
    pub nb_extended_buf: c_int,
    pub side_data: *mut *mut c_void,
    pub nb_side_data: c_int,
    pub flags: c_int,
    pub color_range: c_int,
    pub color_primaries: c_int,
    pub color_trc: c_int,
    pub colorspace: c_int,
    pub chroma_location: c_int,
    pub best_effort_timestamp: i64,
    pub metadata: *mut c_void,
    pub decode_error_flags: c_int,
    pub hw_frames_ctx: *mut AVBufferRef,
}

/// Prefix of `AVPacket` (libavcodec/packet.h), through `flags`.
#[repr(C)]
pub struct AVPacket {
    pub buf: *mut AVBufferRef,
    pub pts: i64,
    pub dts: i64,
    pub data: *mut u8,
    pub size: c_int,
    pub stream_index: c_int,
    pub flags: c_int,
}

/// `AVHWDeviceContext` (libavutil/hwcontext.h), whole.
#[repr(C)]
pub struct AVHWDeviceContext {
    pub av_class: *const c_void,
    pub kind: c_int,
    pub hwctx: *mut c_void,
    pub free: *mut c_void,
    pub user_opaque: *mut c_void,
}

/// `AVHWFramesContext` (libavutil/hwcontext.h), whole.
#[repr(C)]
pub struct AVHWFramesContext {
    pub av_class: *const c_void,
    pub device_ref: *mut AVBufferRef,
    pub device_ctx: *mut AVHWDeviceContext,
    pub hwctx: *mut c_void,
    pub free: *mut c_void,
    pub user_opaque: *mut c_void,
    pub pool: *mut c_void,
    pub initial_pool_size: c_int,
    pub format: c_int,
    pub sw_format: c_int,
    pub width: c_int,
    pub height: c_int,
}

/// `AVD3D11VADeviceContext` (libavutil/hwcontext_d3d11va.h), whole.
#[repr(C)]
pub struct AVD3D11VADeviceContext {
    pub device: *mut c_void,
    pub device_context: *mut c_void,
    pub video_device: *mut c_void,
    pub video_context: *mut c_void,
    pub lock: *mut c_void,
    pub unlock: *mut c_void,
    pub lock_ctx: *mut c_void,
    pub bind_flags: u32,
    pub misc_flags: u32,
}

/// `AVD3D11VAFramesContext` (libavutil/hwcontext_d3d11va.h), whole.
#[repr(C)]
pub struct AVD3D11VAFramesContext {
    pub texture: *mut c_void,
    pub bind_flags: u32,
    pub misc_flags: u32,
    pub texture_infos: *mut c_void,
}

/// Prefix of `AVOption` (libavutil/opt.h), through `offset`.
#[repr(C)]
pub struct AVOption {
    pub name: *const c_char,
    pub help: *const c_char,
    pub offset: c_int,
}

/// Opaque.
pub enum AVCodec {}
pub enum AVCodecContext {}
pub enum AVDictionary {}

type Ctx = *mut AVCodecContext;

/// Byte offsets of `AVCodecContext` fields that have no AVOption ([`Lib::find_offsets`]).
#[derive(Debug, Clone, Copy)]
pub struct Offsets {
    pub time_base: usize,
    pub framerate: usize,
    pub hw_frames_ctx: usize,
}

/// FFmpeg's exported functions, resolved once per process. The DLLs stay loaded for the life of
/// the process.
pub struct Lib {
    pub av_strerror: unsafe extern "C" fn(c_int, *mut c_char, usize) -> c_int,
    pub av_log_set_level: unsafe extern "C" fn(c_int),
    pub av_get_pix_fmt: unsafe extern "C" fn(*const c_char) -> c_int,
    pub av_buffer_ref: unsafe extern "C" fn(*const AVBufferRef) -> *mut AVBufferRef,
    pub av_buffer_unref: unsafe extern "C" fn(*mut *mut AVBufferRef),
    pub av_frame_alloc: unsafe extern "C" fn() -> *mut AVFrame,
    pub av_frame_free: unsafe extern "C" fn(*mut *mut AVFrame),
    pub av_frame_unref: unsafe extern "C" fn(*mut AVFrame),
    pub av_hwdevice_ctx_alloc: unsafe extern "C" fn(c_int) -> *mut AVBufferRef,
    pub av_hwdevice_ctx_init: unsafe extern "C" fn(*mut AVBufferRef) -> c_int,
    pub av_hwdevice_ctx_create_derived:
        unsafe extern "C" fn(*mut *mut AVBufferRef, c_int, *mut AVBufferRef, c_int) -> c_int,
    pub av_hwframe_ctx_alloc: unsafe extern "C" fn(*mut AVBufferRef) -> *mut AVBufferRef,
    pub av_hwframe_ctx_init: unsafe extern "C" fn(*mut AVBufferRef) -> c_int,
    pub av_hwframe_ctx_create_derived: unsafe extern "C" fn(
        *mut *mut AVBufferRef,
        c_int,
        *mut AVBufferRef,
        *mut AVBufferRef,
        c_int,
    ) -> c_int,
    pub av_hwframe_get_buffer: unsafe extern "C" fn(*mut AVBufferRef, *mut AVFrame, c_int) -> c_int,
    pub av_hwframe_map: unsafe extern "C" fn(*mut AVFrame, *const AVFrame, c_int) -> c_int,
    pub av_opt_set: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, c_int) -> c_int,
    pub av_opt_set_int: unsafe extern "C" fn(*mut c_void, *const c_char, i64, c_int) -> c_int,
    pub av_opt_find: unsafe extern "C" fn(
        *mut c_void,
        *const c_char,
        *const c_char,
        c_int,
        c_int,
    ) -> *const AVOption,
    pub avcodec_find_encoder_by_name: unsafe extern "C" fn(*const c_char) -> *const AVCodec,
    #[cfg(test)] // the decoder checks the encoder's output in tests
    pub avcodec_find_decoder_by_name: unsafe extern "C" fn(*const c_char) -> *const AVCodec,
    pub avcodec_alloc_context3: unsafe extern "C" fn(*const AVCodec) -> Ctx,
    pub avcodec_free_context: unsafe extern "C" fn(*mut Ctx),
    pub avcodec_open2: unsafe extern "C" fn(Ctx, *const AVCodec, *mut *mut AVDictionary) -> c_int,
    pub avcodec_send_frame: unsafe extern "C" fn(Ctx, *const AVFrame) -> c_int,
    pub avcodec_receive_packet: unsafe extern "C" fn(Ctx, *mut AVPacket) -> c_int,
    #[cfg(test)] // the decoder checks the encoder's output in tests
    pub avcodec_send_packet: unsafe extern "C" fn(Ctx, *const AVPacket) -> c_int,
    #[cfg(test)] // the decoder checks the encoder's output in tests
    pub avcodec_receive_frame: unsafe extern "C" fn(Ctx, *mut AVFrame) -> c_int,
    pub av_packet_alloc: unsafe extern "C" fn() -> *mut AVPacket,
    pub av_packet_free: unsafe extern "C" fn(*mut *mut AVPacket),
    pub av_packet_unref: unsafe extern "C" fn(*mut AVPacket),
    /// Pixel format numbers, looked up by name rather than counted from the header's enum.
    pub pix_nv12: c_int,
    pub pix_d3d11: c_int,
    pub pix_qsv: c_int,
    pub offsets: Offsets,
    pub dir: PathBuf,
}

/// Loads FFmpeg once per process; later calls return the same result.
pub fn lib() -> Result<&'static Lib, String> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(Lib::load).as_ref().map_err(Clone::clone)
}

/// Where the FFmpeg DLLs are looked for, in order: next to the running executable (the
/// installed layout), then `%FFMPEG_DIR%\bin` (the development layout of
/// `tools/fetch-ffmpeg.ps1`). Never the current directory or `PATH`.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut buf = [0u16; 1024];
    // SAFETY: a null module handle means the executable; the buffer is valid for its length.
    let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    if n > 0 && n < buf.len()
        && let Some(dir) = Path::new(&String::from_utf16_lossy(&buf[..n])).parent()
    {
        dirs.push(dir.to_owned());
    }
    if let Some(d) = std::env::var_os("FFMPEG_DIR") {
        dirs.push(PathBuf::from(d).join("bin"));
    }
    dirs
}

fn load_dll(path: &Path) -> Result<HMODULE, String> {
    // SAFETY: `path` is absolute (from `search_dirs`), so only that file is loaded; the flags
    // make its own dependencies (avutil next to avcodec) resolve from its directory and the
    // system directories, never the current directory. FFmpeg's DllMain has no preconditions.
    unsafe {
        LoadLibraryExW(
            &HSTRING::from(path.as_os_str()),
            None,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    }
    .map_err(|e| format!("{}: {e}", path.display()))
}

macro_rules! sym {
    ($module:expr, $name:ident) => {{
        let name = concat!(stringify!($name), "\0");
        // SAFETY: the module stays loaded for the process lifetime and `name` is a
        // NUL-terminated symbol name.
        let p = unsafe { GetProcAddress($module, PCSTR(name.as_ptr())) }
            .ok_or_else(|| format!("FFmpeg export {} missing", stringify!($name)))?;
        // SAFETY: the exported function has the C signature declared in `Lib` (from the
        // pinned headers; the major version is checked right after loading), and a function
        // pointer transmutes to another function pointer of the same size.
        unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, _>(p) }
    }};
}

impl Lib {
    fn load() -> Result<Self, String> {
        let dir = search_dirs()
            .into_iter()
            .find(|d| d.join(AVCODEC_DLL).is_file())
            .ok_or_else(|| {
                format!(
                    "{AVCODEC_DLL} not found next to the executable or in %FFMPEG_DIR%\\bin \
                     (run tools/fetch-ffmpeg.ps1)"
                )
            })?;
        let util = load_dll(&dir.join(AVUTIL_DLL))?;
        let codec = load_dll(&dir.join(AVCODEC_DLL))?;
        let avutil_version: unsafe extern "C" fn() -> u32 = sym!(util, avutil_version);
        let avcodec_version: unsafe extern "C" fn() -> u32 = sym!(codec, avcodec_version);
        // SAFETY: both take no arguments and only return a constant.
        let (u, c) = unsafe { (avutil_version() >> 16, avcodec_version() >> 16) };
        if (u, c) != (AVUTIL_MAJOR, AVCODEC_MAJOR) {
            return Err(format!(
                "FFmpeg in {} is libavutil {u} / libavcodec {c}; Vetro is built for \
                 {AVUTIL_MAJOR} / {AVCODEC_MAJOR} (tools/fetch-ffmpeg.ps1)",
                dir.display()
            ));
        }
        let mut lib = Self {
            av_strerror: sym!(util, av_strerror),
            av_log_set_level: sym!(util, av_log_set_level),
            av_get_pix_fmt: sym!(util, av_get_pix_fmt),
            av_buffer_ref: sym!(util, av_buffer_ref),
            av_buffer_unref: sym!(util, av_buffer_unref),
            av_frame_alloc: sym!(util, av_frame_alloc),
            av_frame_free: sym!(util, av_frame_free),
            av_frame_unref: sym!(util, av_frame_unref),
            av_hwdevice_ctx_alloc: sym!(util, av_hwdevice_ctx_alloc),
            av_hwdevice_ctx_init: sym!(util, av_hwdevice_ctx_init),
            av_hwdevice_ctx_create_derived: sym!(util, av_hwdevice_ctx_create_derived),
            av_hwframe_ctx_alloc: sym!(util, av_hwframe_ctx_alloc),
            av_hwframe_ctx_init: sym!(util, av_hwframe_ctx_init),
            av_hwframe_ctx_create_derived: sym!(util, av_hwframe_ctx_create_derived),
            av_hwframe_get_buffer: sym!(util, av_hwframe_get_buffer),
            av_hwframe_map: sym!(util, av_hwframe_map),
            av_opt_set: sym!(util, av_opt_set),
            av_opt_set_int: sym!(util, av_opt_set_int),
            av_opt_find: sym!(util, av_opt_find),
            avcodec_find_encoder_by_name: sym!(codec, avcodec_find_encoder_by_name),
            #[cfg(test)]
            avcodec_find_decoder_by_name: sym!(codec, avcodec_find_decoder_by_name),
            avcodec_alloc_context3: sym!(codec, avcodec_alloc_context3),
            avcodec_free_context: sym!(codec, avcodec_free_context),
            avcodec_open2: sym!(codec, avcodec_open2),
            avcodec_send_frame: sym!(codec, avcodec_send_frame),
            avcodec_receive_packet: sym!(codec, avcodec_receive_packet),
            #[cfg(test)]
            avcodec_send_packet: sym!(codec, avcodec_send_packet),
            #[cfg(test)]
            avcodec_receive_frame: sym!(codec, avcodec_receive_frame),
            av_packet_alloc: sym!(codec, av_packet_alloc),
            av_packet_free: sym!(codec, av_packet_free),
            av_packet_unref: sym!(codec, av_packet_unref),
            pix_nv12: -1,
            pix_d3d11: -1,
            pix_qsv: -1,
            offsets: Offsets {
                time_base: 0,
                framerate: 0,
                hw_frames_ctx: 0,
            },
            dir,
        };
        let get_pix_fmt = lib.av_get_pix_fmt;
        let pix = |name: &CStr| {
            // SAFETY: a NUL-terminated name; returns -1 when unknown.
            let f = unsafe { get_pix_fmt(name.as_ptr()) };
            if f < 0 {
                Err(format!("FFmpeg lacks pixel format {name:?}"))
            } else {
                Ok(f)
            }
        };
        lib.pix_nv12 = pix(c"nv12")?;
        lib.pix_d3d11 = pix(c"d3d11")?;
        lib.pix_qsv = pix(c"qsv")?;
        lib.offsets = lib.find_offsets()?;
        // SAFETY: plain setter. Errors only (AV_LOG_ERROR = 16): FFmpeg logs from inside
        // encode calls, and section 10 allows no logging on the hot path beyond failures.
        unsafe { (lib.av_log_set_level)(16) };
        Ok(lib)
    }

    /// Offsets of the three `AVCodecContext` fields that have no AVOption, derived from fields
    /// that do (libavcodec/avcodec.h, major 62), so no offset is counted by hand:
    /// - `bit_rate` (option `b`, an i64) is followed by `flags`, `flags2`, `extradata`,
    ///   `extradata_size`, then `time_base`, `pkt_timebase`, `framerate` (AVRationals);
    /// - `hw_frames_ctx`, `hw_device_ctx` (pointers) come right before `hwaccel_flags`, which
    ///   is followed by `extra_hw_frames`.
    ///
    /// The neighbours that are options (`flags`, `flags2`, `extra_hw_frames`) must sit where
    /// the header says, or loading fails.
    fn find_offsets(&self) -> Result<Offsets, String> {
        // SAFETY: a null codec gives a generic context; it is freed below.
        let mut ctx = unsafe { (self.avcodec_alloc_context3)(std::ptr::null()) };
        if ctx.is_null() {
            return Err("avcodec_alloc_context3 failed".into());
        }
        let off = |name: &CStr| {
            // SAFETY: `ctx` is a live AVCodecContext (an AVClass object); search flags 0 look
            // at its own options only.
            let o = unsafe {
                (self.av_opt_find)(ctx.cast(), name.as_ptr(), std::ptr::null(), 0, 0)
            };
            // SAFETY: a non-null result points at a static AVOption.
            (!o.is_null()).then(|| usize::try_from(unsafe { (*o).offset }).ok()).flatten()
        };
        let found = [c"b", c"flags", c"flags2", c"hwaccel_flags", c"extra_hw_frames"].map(off);
        let offsets = match found {
            [Some(b), Some(f), Some(f2), Some(hw), Some(extra)]
                if f == b + 8 && f2 == b + 12 && extra == hw + 4 && hw >= 16 && hw % 8 == 0 =>
            {
                // AVRational is two ints (4-byte aligned): no padding after extradata_size.
                let o = Offsets {
                    time_base: b + 28,
                    framerate: b + 44,
                    hw_frames_ctx: hw - 16,
                };
                // SAFETY: the offset lies inside the context, before an option's field, and is
                // pointer-aligned (checked above); a fresh context has no frames context.
                let empty = unsafe {
                    ctx.cast::<u8>()
                        .add(o.hw_frames_ctx)
                        .cast::<*mut AVBufferRef>()
                        .read()
                }
                .is_null();
                empty.then_some(o)
            }
            _ => None,
        };
        // SAFETY: frees the context allocated above.
        unsafe { (self.avcodec_free_context)(&mut ctx) };
        offsets.ok_or_else(|| {
            format!("AVCodecContext layout is not libavcodec {AVCODEC_MAJOR}'s ({found:?})")
        })
    }

    /// Writes an `AVCodecContext` field found by [`Lib::find_offsets`].
    ///
    /// # Safety
    /// `ctx` must be a live codec context not yet opened, and `T` the field's type.
    pub unsafe fn write_field<T>(&self, ctx: *mut AVCodecContext, offset: usize, value: T) {
        // SAFETY: the caller's contract; offsets from `find_offsets` are aligned for the field.
        unsafe { ctx.cast::<u8>().add(offset).cast::<T>().write(value) }
    }

    pub fn encoder(&self, name: &CStr) -> Result<*const AVCodec, String> {
        // SAFETY: NUL-terminated name; returns null when absent.
        let c = unsafe { (self.avcodec_find_encoder_by_name)(name.as_ptr()) };
        if c.is_null() {
            Err(format!("this FFmpeg build has no encoder {name:?}"))
        } else {
            Ok(c)
        }
    }

    /// A readable message for an FFmpeg error code.
    pub fn err(&self, what: &str, code: c_int) -> String {
        let mut buf = [0 as c_char; 128];
        // SAFETY: the buffer is valid for its length; av_strerror always NUL-terminates.
        unsafe { (self.av_strerror)(code, buf.as_mut_ptr(), buf.len()) };
        // SAFETY: NUL-terminated by av_strerror.
        let msg = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
        format!("{what}: {msg} ({code})")
    }

    /// Sets an option on `obj` or its private options. A name FFmpeg does not know is an error.
    ///
    /// # Safety
    /// `obj` must be a live AVClass object (here an `AVCodecContext`).
    pub unsafe fn set(&self, obj: *mut c_void, name: &str, value: &str) -> Result<(), String> {
        let (n, v) = (cstr(name)?, cstr(value)?);
        // SAFETY: the caller's contract; both strings are NUL-terminated.
        let r = unsafe { (self.av_opt_set)(obj, n.as_ptr(), v.as_ptr(), AV_OPT_SEARCH_CHILDREN) };
        if r < 0 {
            Err(self.err(&format!("option {name}={value}"), r))
        } else {
            Ok(())
        }
    }

    /// # Safety
    /// As for [`Lib::set`].
    pub unsafe fn set_int(&self, obj: *mut c_void, name: &str, value: i64) -> Result<(), String> {
        let n = cstr(name)?;
        // SAFETY: the caller's contract; the name is NUL-terminated.
        let r = unsafe { (self.av_opt_set_int)(obj, n.as_ptr(), value, AV_OPT_SEARCH_CHILDREN) };
        if r < 0 {
            Err(self.err(&format!("option {name}={value}"), r))
        } else {
            Ok(())
        }
    }
}

fn cstr(s: &str) -> Result<CString, String> {
    CString::new(s).map_err(|_| format!("{s:?} has a NUL byte"))
}
