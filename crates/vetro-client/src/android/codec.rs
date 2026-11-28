//! [`Decoder`] on NDK `AMediaCodec` in async mode, decoding straight to the `SurfaceView`'s
//! `ANativeWindow` (zero-copy, ARCHITECTURE section 6.2, ADR-0010).
//!
//! - Low-latency configuration: `low-latency = 1`, `priority = 0` (real time),
//!   `operating-rate = i16::MAX` (as fast as possible), and on Qualcomm codecs
//!   `vendor.qti-ext-dec-low-latency.enable = 1`.
//! - Input: the codec announces free input buffers on its own thread; their indices go through
//!   a bounded channel. [`Decoder::decode`] never waits for one: with none free, the access unit
//!   is dropped and the session requests a keyframe.
//! - Output: every decoded buffer is released for rendering at once (latency-first
//!   presentation). The presentation time carries the frame id, so output and render events map
//!   back to frames.
//! - Render times come from `AMediaCodec_setOnFrameRenderedCallback` (API 33), looked up at run
//!   time because `minSdk` is 30; below API 33 `rendered_us` stays 0 (unknown).

use crate::decoder::{Decoder, DecoderError, DecoderEvent, DecoderEvents};
use ndk_sys as sys;
use std::ffi::{CStr, c_char, c_void};
use std::ptr::{NonNull, null_mut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use vetro_protocol::datagram::FrameInfo;
use vetro_protocol::messages::{Configure, Profile};

/// More than any codec has input buffers, so an announced index is never lost.
const INPUT_SLOTS: usize = 256;

/// One reference to an `ANativeWindow`, released on drop.
pub(crate) struct Window(NonNull<sys::ANativeWindow>);

// SAFETY: ANativeWindow is reference counted and safe to use from any thread; this type only
// hands the pointer to AMediaCodec_configure and releases its one reference once.
unsafe impl Send for Window {}

impl Window {
    /// # Safety
    /// `env` is the current thread's valid `JNIEnv` and `surface` a live reference to an
    /// `android.view.Surface`.
    pub(crate) unsafe fn from_surface(
        env: *mut jni_sys::JNIEnv,
        surface: jni_sys::jobject,
    ) -> Option<Self> {
        // SAFETY: guaranteed by the caller; the returned window carries a reference we own.
        NonNull::new(unsafe { sys::ANativeWindow_fromSurface(env, surface) }).map(Self)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: releases the one reference ANativeWindow_fromSurface gave us.
        unsafe { sys::ANativeWindow_release(self.0.as_ptr()) }
    }
}

pub(crate) struct MediaCodecDecoder {
    // Field order: the codec is deleted before the window it renders to is released.
    codec: Option<Codec>,
    window: Window,
}

impl MediaCodecDecoder {
    pub(crate) fn new(window: Window) -> Self {
        Self {
            codec: None,
            window,
        }
    }
}

/// What the codec's callback thread may touch.
struct Shared {
    inputs: mpsc::SyncSender<usize>,
    events: DecoderEvents,
    render_events: bool,
    /// The codec's async error status, 0 while healthy. Sticky, because the error event can be
    /// dropped when the event queue is full: `decode` fails from then on.
    error: AtomicI32,
}

/// A started codec. Drop stops and deletes it before `shared`, which its callbacks read, is
/// freed.
struct Codec {
    ptr: NonNull<sys::AMediaCodec>,
    inputs: mpsc::Receiver<usize>,
    shared: Box<Shared>,
}

// SAFETY: AMediaCodec may be called from any thread. `Codec` is used by one thread at a time
// (`&mut self`); the callback thread only reads `Shared`, which is `Sync`.
unsafe impl Send for Codec {}

impl Drop for Codec {
    fn drop(&mut self) {
        // SAFETY: `ptr` is a codec we own. AMediaCodec_delete stops its looper, so no callback
        // runs after it returns and `shared` (dropped after this body) is no longer read.
        unsafe {
            sys::AMediaCodec_stop(self.ptr.as_ptr());
            sys::AMediaCodec_delete(self.ptr.as_ptr());
        }
    }
}

fn mime(p: Profile) -> Option<&'static CStr> {
    match p {
        Profile::HevcMain => Some(c"video/hevc"),
        Profile::H264ConstrainedBaseline | Profile::H264Main | Profile::H264High => {
            Some(c"video/avc")
        }
        // Main10 and AV1 (experimental, ADR-0005) are not offered until verified on devices.
        Profile::HevcMain10 | Profile::Av1Main => None,
    }
}

fn status(s: sys::media_status_t, what: &'static str) -> Result<(), DecoderError> {
    if s.0 == 0 {
        Ok(())
    } else {
        Err(DecoderError { what, code: s.0 })
    }
}

/// Creates a decoder for `mime`; the caller owns it.
fn create(mime: &CStr) -> Option<NonNull<sys::AMediaCodec>> {
    // SAFETY: `mime` is NUL-terminated; the result is null or a codec the caller now owns.
    NonNull::new(unsafe { sys::AMediaCodec_createDecoderByType(mime.as_ptr()) })
}

fn probe_profiles() -> Vec<Profile> {
    let has = |mime: &CStr| {
        create(mime).is_some_and(|c| {
            // SAFETY: a probe codec we own and never use again.
            unsafe { sys::AMediaCodec_delete(c.as_ptr()) };
            true
        })
    };
    let mut out = Vec::new();
    if has(c"video/hevc") {
        out.push(Profile::HevcMain);
    }
    if has(c"video/avc") {
        out.extend([
            Profile::H264High,
            Profile::H264Main,
            Profile::H264ConstrainedBaseline,
        ]);
    }
    out
}

impl Decoder for MediaCodecDecoder {
    fn profiles(&self) -> Vec<Profile> {
        // Probing creates codecs (tens of ms each) on the session thread, where `stop` cannot
        // interrupt it: do it once per process.
        static PROFILES: OnceLock<Vec<Profile>> = OnceLock::new();
        PROFILES.get_or_init(probe_profiles).clone()
    }

    fn configure(&mut self, cfg: &Configure, events: DecoderEvents) -> Result<(), DecoderError> {
        // One hardware decoder at a time: the old one goes first.
        self.codec = None;
        let mime = mime(cfg.profile).ok_or(DecoderError {
            what: "profile not supported",
            code: 0,
        })?;
        let ptr = create(mime).ok_or(DecoderError {
            what: "no decoder for the profile",
            code: 0,
        })?;
        let set_rendered = frame_rendered_setter();
        let (tx, inputs) = mpsc::sync_channel(INPUT_SLOTS);
        let codec = Codec {
            ptr,
            inputs,
            shared: Box::new(Shared {
                inputs: tx,
                events,
                render_events: set_rendered.is_some(),
                error: AtomicI32::new(0),
            }),
        };
        let userdata = (&raw const *codec.shared).cast_mut().cast::<c_void>();
        let c = ptr.as_ptr();

        let format = Format::new()?;
        format.set_str(c"mime", mime);
        // Mode fields are validated by the protocol (at most 16 384, refresh at most 1000 Hz).
        format.set_i32(c"width", cfg.width as i32);
        format.set_i32(c"height", cfg.height as i32);
        format.set_i32(c"low-latency", 1);
        format.set_i32(c"priority", 0);
        format.set_i32(c"operating-rate", i32::from(i16::MAX));
        if is_qualcomm(c) {
            format.set_i32(c"vendor.qti-ext-dec-low-latency.enable", 1);
        }

        let callbacks = sys::AMediaCodecOnAsyncNotifyCallback {
            onAsyncInputAvailable: Some(on_input),
            onAsyncOutputAvailable: Some(on_output),
            onAsyncFormatChanged: Some(on_format_changed),
            onAsyncError: Some(on_error),
        };
        // SAFETY: `c` is our codec, not started yet; `userdata` points to `Shared`, which lives
        // until the codec is deleted (see `Codec`).
        let s = unsafe { sys::AMediaCodec_setAsyncNotifyCallback(c, callbacks, userdata) };
        status(s, "setAsyncNotifyCallback")?;
        if let Some(set) = set_rendered {
            // SAFETY: as above; `set` is the NDK function with this exact signature.
            let s = unsafe { set(c, Some(on_rendered), userdata) };
            status(s, "setOnFrameRenderedCallback")?;
        }
        let window = self.window.0.as_ptr();
        // SAFETY: `c` and `format` are valid; the window outlives the codec (field order of
        // `MediaCodecDecoder`); no crypto.
        let s = unsafe { sys::AMediaCodec_configure(c, format.0.as_ptr(), window, null_mut(), 0) };
        status(s, "configure")?;
        // SAFETY: configured codec we own.
        status(unsafe { sys::AMediaCodec_start(c) }, "start")?;
        self.codec = Some(codec);
        Ok(())
    }

    fn decode(&mut self, info: &FrameInfo, data: &[u8]) -> Result<bool, DecoderError> {
        let codec = self.codec.as_mut().ok_or(DecoderError {
            what: "not configured",
            code: 0,
        })?;
        let code = codec.shared.error.load(Ordering::Acquire);
        if code != 0 {
            return Err(DecoderError {
                what: "codec error",
                code,
            });
        }
        let Ok(idx) = codec.inputs.try_recv() else {
            return Ok(false);
        };
        let c = codec.ptr.as_ptr();
        let mut cap = 0usize;
        // SAFETY: `idx` was announced free by the codec and is ours until queued; the buffer is
        // valid for `cap` bytes until then.
        let buf = unsafe { sys::AMediaCodec_getInputBuffer(c, idx, &mut cap) };
        let fits = !buf.is_null() && data.len() <= cap;
        if fits {
            // SAFETY: `buf` holds at least `data.len()` bytes and is codec memory, so the
            // ranges cannot overlap.
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len()) };
        }
        // A unit that does not fit is dropped, but the buffer must go back (empty).
        let len = if fits { data.len() } else { 0 };
        let pts = u64::from(info.frame_id);
        // SAFETY: `idx` is ours, `len <= cap`. The frame id is the presentation time.
        let s = unsafe { sys::AMediaCodec_queueInputBuffer(c, idx, 0, len, pts, 0) };
        status(s, "queueInputBuffer")?;
        Ok(fits)
    }
}

fn is_qualcomm(c: *mut sys::AMediaCodec) -> bool {
    let mut name: *mut c_char = null_mut();
    // SAFETY: `c` is a valid codec; on success `name` is a NUL-terminated string we release.
    if unsafe { sys::AMediaCodec_getName(c, &mut name) }.0 != 0 || name.is_null() {
        return false;
    }
    // SAFETY: as above.
    let n = unsafe { CStr::from_ptr(name) }.to_bytes();
    let qcom = n.starts_with(b"c2.qti.") || n.starts_with(b"OMX.qcom.");
    // SAFETY: releases the string AMediaCodec_getName gave us.
    unsafe { sys::AMediaCodec_releaseName(c, name) };
    qcom
}

/// An `AMediaFormat`, deleted on drop.
struct Format(NonNull<sys::AMediaFormat>);

impl Format {
    fn new() -> Result<Self, DecoderError> {
        // SAFETY: no arguments; the result is null or a format we own.
        NonNull::new(unsafe { sys::AMediaFormat_new() })
            .map(Self)
            .ok_or(DecoderError {
                what: "AMediaFormat_new",
                code: 0,
            })
    }

    fn set_i32(&self, key: &CStr, v: i32) {
        // SAFETY: valid format, NUL-terminated key (copied by the NDK).
        unsafe { sys::AMediaFormat_setInt32(self.0.as_ptr(), key.as_ptr(), v) }
    }

    fn set_str(&self, key: &CStr, v: &CStr) {
        // SAFETY: valid format, NUL-terminated key and value (both copied by the NDK).
        unsafe { sys::AMediaFormat_setString(self.0.as_ptr(), key.as_ptr(), v.as_ptr()) }
    }
}

impl Drop for Format {
    fn drop(&mut self) {
        // SAFETY: a format we own; the codec copied what it needs in configure.
        unsafe { sys::AMediaFormat_delete(self.0.as_ptr()) };
    }
}

type SetFrameRendered = unsafe extern "C" fn(
    *mut sys::AMediaCodec,
    sys::AMediaCodecOnFrameRendered,
    *mut c_void,
) -> sys::media_status_t;

/// `AMediaCodec_setOnFrameRenderedCallback`, present from API 33. Linking it directly would
/// stop the library from loading on API 30–32.
fn frame_rendered_setter() -> Option<SetFrameRendered> {
    static SETTER: OnceLock<Option<SetFrameRendered>> = OnceLock::new();
    *SETTER.get_or_init(|| {
        // SAFETY: libmediandk is a system library this crate already links; the handle is
        // never closed, which keeps the symbol valid for the life of the process.
        let lib = unsafe { libc::dlopen(c"libmediandk.so".as_ptr(), libc::RTLD_NOW) };
        if lib.is_null() {
            return None;
        }
        // SAFETY: valid handle, NUL-terminated name.
        let sym = unsafe { libc::dlsym(lib, c"AMediaCodec_setOnFrameRenderedCallback".as_ptr()) };
        // SAFETY: NdkMediaCodec.h declares this symbol with exactly the `SetFrameRendered`
        // signature.
        (!sym.is_null())
            .then(|| unsafe { std::mem::transmute::<*mut c_void, SetFrameRendered>(sym) })
    })
}

/// # Safety
/// `userdata` is the `Shared` registered in `configure`, alive while the codec exists.
unsafe fn shared<'a>(userdata: *mut c_void) -> &'a Shared {
    // SAFETY: guaranteed by the caller.
    unsafe { &*userdata.cast::<Shared>() }
}

unsafe extern "C" fn on_input(_: *mut sys::AMediaCodec, userdata: *mut c_void, index: i32) {
    // SAFETY: called by the codec with the userdata registered in `configure`.
    let s = unsafe { shared(userdata) };
    if let Ok(i) = usize::try_from(index) {
        let _ = s.inputs.try_send(i);
    }
}

unsafe extern "C" fn on_output(
    codec: *mut sys::AMediaCodec,
    userdata: *mut c_void,
    index: i32,
    info: *mut sys::AMediaCodecBufferInfo,
) {
    // SAFETY: called by the codec with the userdata registered in `configure` and a buffer
    // info valid for the duration of the call.
    let (s, info) = unsafe { (shared(userdata), &*info) };
    let Ok(idx) = usize::try_from(index) else {
        return;
    };
    let config = info.flags & sys::AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG != 0;
    let render = info.size > 0 && !config;
    // SAFETY: `idx` is the output buffer the codec just handed us; releasing it from the
    // callback thread is allowed. Latency-first: render at once.
    unsafe { sys::AMediaCodec_releaseOutputBuffer(codec, idx, render) };
    if render {
        s.events.send(DecoderEvent::Decoded {
            frame_id: info.presentationTimeUs as u32,
            at_us: s.events.now_us(),
            render_pending: s.render_events,
        });
    }
}

unsafe extern "C" fn on_format_changed(
    _: *mut sys::AMediaCodec,
    _: *mut c_void,
    _: *mut sys::AMediaFormat,
) {
}

unsafe extern "C" fn on_error(
    _: *mut sys::AMediaCodec,
    userdata: *mut c_void,
    error: sys::media_status_t,
    _action_code: i32,
    _detail: *const c_char,
) {
    // SAFETY: called by the codec with the userdata registered in `configure`.
    let s = unsafe { shared(userdata) };
    // Any status, even 0, marks the codec failed.
    let code = if error.0 == 0 { -1 } else { error.0 };
    s.error.store(code, Ordering::Release);
    // Wakes the session if it is idle; `decode` reports it otherwise.
    s.events.send(DecoderEvent::Error(DecoderError {
        what: "codec error",
        code,
    }));
}

unsafe extern "C" fn on_rendered(
    _: *mut sys::AMediaCodec,
    userdata: *mut c_void,
    media_time_us: i64,
    system_nano: i64,
) {
    // SAFETY: called by the codec with the userdata registered in `configure`.
    let s = unsafe { shared(userdata) };
    // `system_nano` is CLOCK_MONOTONIC; render callbacks may come in batches, so convert by age.
    let age_us = u64::try_from(monotonic_ns().saturating_sub(system_nano) / 1000).unwrap_or(0);
    s.events.send(DecoderEvent::Rendered {
        frame_id: media_time_us as u32,
        at_us: s.events.now_us().saturating_sub(age_us),
    });
}

// `time_t` and `c_long` are i32 on 32-bit ARM.
#[allow(clippy::useless_conversion)]
fn monotonic_ns() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid out-pointer; CLOCK_MONOTONIC always exists.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    i64::from(ts.tv_sec)
        .saturating_mul(1_000_000_000)
        .saturating_add(i64::from(ts.tv_nsec))
}
