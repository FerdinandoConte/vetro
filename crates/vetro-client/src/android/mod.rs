//! The JNI surface the Kotlin app calls (M1-6 owns the Kotlin side). Kept to what starting and
//! stopping a stream needs; input (M1-10) and HUD statistics add their own functions later.
//!
//! ```kotlin
//! package dev.vetro.app
//!
//! object VetroCore {
//!     init { System.loadLibrary("vetro_client") }
//!
//!     /** Starts a session on its own thread. Returns a handle, or 0 if an argument is invalid. */
//!     @JvmStatic external fun nativeStart(
//!         surface: android.view.Surface, // the SurfaceView's surface; decoded frames go here
//!         host: String,                  // IP literal from the QR or mDNS, e.g. "192.168.1.20"
//!         port: Int,
//!         hostFingerprint: ByteArray,    // 32 bytes, SHA-256 of the host certificate (QR `fp`)
//!         pairToken: ByteArray?,         // 16 bytes on the first connection (QR `t`), else null
//!         identityPath: String,          // app-private file for this device's key, e.g. filesDir
//!         width: Int, height: Int,       // native panel mode
//!         refreshMhz: Int,               // e.g. 120000 for 120 Hz
//!         densityDpi: Int,
//!     ): Long
//!
//!     /** One of the STATE_* values below. */
//!     @JvmStatic external fun nativeState(handle: Long): Int
//!
//!     /** The host sent `Configure` at some point in this session: on a first connection the
//!      *  pairing is confirmed, so persist the host. Stays true after the session ends. */
//!     @JvmStatic external fun nativePaired(handle: Long): Boolean
//!
//!     /** Stops the session and frees the handle. Call exactly once per handle, before the
//!      *  surface is destroyed. Blocks until the session thread has ended. */
//!     @JvmStatic external fun nativeStop(handle: Long)
//! }
//! ```
//!
//! States: `0` connecting, `1` streaming (the host sent `Configure`), `2` ended (stopped, or the
//! host said `Bye`), `3` not paired (wrong, used or expired token; also when the host closes with
//! code 2 right after connecting), `4` failed (anything else, a panic included).
//!
//! `nativeStart` returns 0 for: a non-IP `host`, port 0 or out of range, a fingerprint that is
//! not 32 bytes, a token that is not null or 16 bytes, a path that is not valid UTF-8, a mode
//! outside the protocol limits (1..=16384 pixels, 1..=1000 Hz) or density 0, a null surface.
//!
//! The session thread runs a current-thread tokio runtime at `THREAD_PRIORITY_URGENT_DISPLAY`
//! (ARCHITECTURE section 10): it is the thread that queues every access unit.

mod codec;

use crate::clock::Clock;
use crate::session::{self, Config, Error};
use crate::telemetry::Stats;
use codec::{MediaCodecDecoder, Window};
use jni_sys::{
    JNI_FALSE, JNI_TRUE, JNIEnv, jboolean, jbyte, jbyteArray, jclass, jint, jlong, jobject, jstring,
};
use std::ffi::CStr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::oneshot;
use vetro_protocol::messages::{Display, MAX_DIMENSION, MAX_REFRESH_MHZ, PairToken};
use vetro_transport::{Client, Fingerprint, Identity};

const STATE_CONNECTING: i32 = 0;
const STATE_STREAMING: i32 = 1;
const STATE_ENDED: i32 = 2;
const STATE_NOT_PAIRED: i32 = 3;
const STATE_FAILED: i32 = 4;

/// How long the session thread waits for the close packet to leave.
const CLOSE_FLUSH: Duration = Duration::from_millis(100);

/// Android `THREAD_PRIORITY_URGENT_DISPLAY`.
const URGENT_DISPLAY: libc::c_int = -8;

struct Handle {
    stop: oneshot::Sender<()>,
    thread: JoinHandle<()>,
    /// `STATE_CONNECTING` while running, then the final state.
    state: Arc<AtomicI32>,
    stats: Arc<Stats>,
}

/// # Safety
/// Called by the JVM: `env` is valid for this thread and every reference argument is a live
/// local reference (or null where Kotlin allows it).
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "system" fn Java_dev_vetro_app_VetroCore_nativeStart(
    env: *mut JNIEnv,
    _class: jclass,
    surface: jobject,
    host: jstring,
    port: jint,
    host_fingerprint: jbyteArray,
    pair_token: jbyteArray,
    identity_path: jstring,
    width: jint,
    height: jint,
    refresh_mhz: jint,
    density_dpi: jint,
) -> jlong {
    // Arguments come from the app; check every one before anything starts.
    let parsed = (|| {
        // SAFETY: guaranteed by the JVM (see above), for every helper call below.
        let ip: IpAddr = unsafe { string(env, host) }?.parse().ok()?;
        let port = u16::try_from(port).ok().filter(|&p| p != 0)?;
        // SAFETY: as above.
        let fp = Fingerprint(unsafe { bytes::<32>(env, host_fingerprint) }?);
        let token = if pair_token.is_null() {
            None
        } else {
            // SAFETY: as above.
            Some(PairToken(unsafe { bytes::<16>(env, pair_token) }?))
        };
        // SAFETY: as above.
        let path = PathBuf::from(unsafe { string(env, identity_path) }?);
        let display = Display {
            width: u32::try_from(width).ok()?,
            height: u32::try_from(height).ok()?,
            refresh_mhz: u32::try_from(refresh_mhz).ok()?,
            density_dpi: u16::try_from(density_dpi).ok()?,
        };
        // The protocol's limits, so `Capabilities` cannot fail to encode later.
        let size = 1..=MAX_DIMENSION;
        if !size.contains(&display.width)
            || !size.contains(&display.height)
            || !(1..=MAX_REFRESH_MHZ).contains(&display.refresh_mhz)
            || display.density_dpi == 0
            || surface.is_null()
        {
            return None;
        }
        // SAFETY: as above; `surface` is a non-null android.view.Surface.
        let window = unsafe { Window::from_surface(env, surface) }?;
        let cfg = Config {
            host: SocketAddr::new(ip, port),
            host_fingerprint: fp,
            pair_token: token,
            display,
            clock: Clock::new(),
        };
        Some((cfg, path, window))
    })();
    let Some((cfg, path, window)) = parsed else {
        return 0;
    };

    let state = Arc::new(AtomicI32::new(STATE_CONNECTING));
    let stats = Arc::new(Stats::default());
    let (stop, stop_rx) = oneshot::channel();
    let (st, ss) = (state.clone(), stats.clone());
    let thread = std::thread::Builder::new()
        .name("vetro-session".into())
        .spawn(move || {
            let decoder = MediaCodecDecoder::new(window);
            // A panic must still end in a final state, or the app polls "connecting" forever.
            let end = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                session_thread(cfg, &path, decoder, &ss, stop_rx)
            }));
            st.store(end.unwrap_or(STATE_FAILED), Ordering::Release);
        });
    match thread {
        Ok(thread) => Box::into_raw(Box::new(Handle {
            stop,
            thread,
            state,
            stats,
        })) as jlong,
        Err(_) => 0,
    }
}

/// # Safety
/// `handle` is 0 or a value from `nativeStart` not yet passed to `nativeStop`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_vetro_app_VetroCore_nativeState(
    _env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
) -> jint {
    if handle == 0 {
        return STATE_FAILED;
    }
    // SAFETY: guaranteed by the caller: a live `Handle` from `nativeStart`.
    let h = unsafe { &*(handle as *const Handle) };
    match h.state.load(Ordering::Acquire) {
        STATE_CONNECTING if h.stats.snapshot().streaming => STATE_STREAMING,
        s => s,
    }
}

/// # Safety
/// `handle` is 0 or a value from `nativeStart` not yet passed to `nativeStop`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_vetro_app_VetroCore_nativePaired(
    _env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
) -> jboolean {
    if handle == 0 {
        return JNI_FALSE;
    }
    // SAFETY: guaranteed by the caller: a live `Handle` from `nativeStart`.
    let h = unsafe { &*(handle as *const Handle) };
    if h.stats.snapshot().streaming {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

/// # Safety
/// `handle` is 0 or a value from `nativeStart`, passed here exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_vetro_app_VetroCore_nativeStop(
    _env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }
    // SAFETY: guaranteed by the caller: we take back the `Box` from `nativeStart`, once.
    let h = unsafe { Box::from_raw(handle as *mut Handle) };
    let _ = h.stop.send(());
    // The decoder (and with it the window reference) is dropped on that thread before this
    // returns, so the app may destroy the surface afterwards.
    let _ = h.thread.join();
}

fn session_thread(
    cfg: Config,
    identity: &std::path::Path,
    mut decoder: MediaCodecDecoder,
    stats: &Stats,
    stop: oneshot::Receiver<()>,
) -> i32 {
    // SAFETY: a plain syscall; `who = 0` is the calling thread on Linux. Best effort.
    unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, URGENT_DISPLAY) };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return STATE_FAILED;
    };
    let result = rt.block_on(async {
        let identity = Identity::load_or_generate(identity)?;
        let any = match cfg.host.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        let client = Client::bind(SocketAddr::new(any, 0), &identity)?;
        let stop = async {
            let _ = stop.await;
        };
        let r = session::run(&client, &cfg, &mut decoder, stats, stop).await;
        // `run` closed the connection; let the close packet leave before the runtime is
        // dropped, or the host only notices after its idle timeout. Bounded, because
        // `nativeStop` blocks the caller until this thread ends.
        let _ = tokio::time::timeout(CLOSE_FLUSH, client.wait_idle()).await;
        r
    });
    match result {
        Ok(_) => STATE_ENDED,
        Err(Error::NotPaired) => STATE_NOT_PAIRED,
        Err(_) => STATE_FAILED,
    }
}

/// A Java string as UTF-8. JNI gives modified UTF-8, which equals UTF-8 for every string
/// without NUL or characters outside the BMP; anything else is refused.
///
/// # Safety
/// `env` is valid for this thread and `s` is null or a live `java.lang.String` reference.
unsafe fn string(env: *mut JNIEnv, s: jstring) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: `env` is valid; every JNI version has these table entries.
    let iface = unsafe { &**env };
    let (get, release) = (iface.GetStringUTFChars?, iface.ReleaseStringUTFChars?);
    // SAFETY: valid env and string; null `isCopy` is allowed.
    let chars = unsafe { get(env, s, null_mut()) };
    if chars.is_null() {
        return None;
    }
    // SAFETY: JNI returns a NUL-terminated buffer, valid until released below.
    let out = unsafe { CStr::from_ptr(chars) }
        .to_str()
        .ok()
        .map(str::to_owned);
    // SAFETY: releases exactly the buffer `get` returned.
    unsafe { release(env, s, chars) };
    out
}

/// A Java `byte[]` of exactly `N` bytes.
///
/// # Safety
/// `env` is valid for this thread and `a` is null or a live `byte[]` reference.
unsafe fn bytes<const N: usize>(env: *mut JNIEnv, a: jbyteArray) -> Option<[u8; N]> {
    if a.is_null() {
        return None;
    }
    // SAFETY: `env` is valid; every JNI version has these table entries.
    let iface = unsafe { &**env };
    let (len, region) = (iface.GetArrayLength?, iface.GetByteArrayRegion?);
    // SAFETY: valid env and array.
    if usize::try_from(unsafe { len(env, a) }).ok()? != N {
        return None;
    }
    let mut out = [0u8; N];
    // SAFETY: the array holds exactly N bytes, so the region is in bounds and `out` has room.
    unsafe { region(env, a, 0, N as jint, out.as_mut_ptr().cast::<jbyte>()) };
    Some(out)
}
