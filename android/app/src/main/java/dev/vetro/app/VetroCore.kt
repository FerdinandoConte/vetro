package dev.vetro.app

import android.view.Surface

/** The JNI surface of `vetro-client` (crates/vetro-client/src/android/mod.rs documents each call). */
object VetroCore {
    const val STATE_CONNECTING = 0
    const val STATE_STREAMING = 1
    const val STATE_ENDED = 2
    const val STATE_NOT_PAIRED = 3
    const val STATE_FAILED = 4

    init {
        System.loadLibrary("vetro_client")
    }

    /** Returns a handle, or 0 when an argument is invalid. */
    @JvmStatic
    external fun nativeStart(
        surface: Surface,
        host: String,
        port: Int,
        hostFingerprint: ByteArray,
        pairToken: ByteArray?,
        identityPath: String,
        width: Int,
        height: Int,
        refreshMhz: Int,
        densityDpi: Int,
    ): Long

    @JvmStatic external fun nativeState(handle: Long): Int

    @JvmStatic external fun nativePaired(handle: Long): Boolean

    /** Once per handle, before the surface is destroyed. */
    @JvmStatic external fun nativeStop(handle: Long)
}
