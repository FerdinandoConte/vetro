package dev.vetro.app

import android.app.Activity
import android.graphics.Color
import android.graphics.Typeface
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.Choreographer
import android.view.Gravity
import android.view.MotionEvent
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.TextView
import kotlin.math.roundToInt

/**
 * Full-screen stream: an immersive SurfaceView the Rust client decodes into, held at the panel's
 * highest refresh rate, screen kept on, with a small HUD (state and the vsync rate this window
 * really gets; per-stage latency arrives with the telemetry JNI of M1-11). A three-finger tap
 * shows or hides the HUD (ARCHITECTURE section 6.1).
 */
class StreamActivity : Activity(), SurfaceHolder.Callback {
    companion object {
        const val EXTRA_HOST = "host"
        const val EXTRA_PORT = "port"
        const val EXTRA_FP = "fp"
        const val EXTRA_TOKEN = "token"
    }

    private val ui = Handler(Looper.getMainLooper())
    private lateinit var hud: TextView
    private lateinit var store: HostStore
    private lateinit var host: PairedHost
    private var token: ByteArray? = null // first connection only; dropped once the pairing is confirmed
    private var handle = 0L
    private var hudVisible = true
    private var vsyncCount = 0
    private var vsyncFps = 0
    private var message = "Connecting..."

    // The Rust session thread is the only writer of state; reading it here is cheap, so poll.
    private val poll = object : Runnable {
        override fun run() {
            refresh()
            if (handle != 0L) ui.postDelayed(this, 250)
        }
    }
    private val vsync = object : Choreographer.FrameCallback {
        override fun doFrame(frameTimeNanos: Long) {
            vsyncCount++
            Choreographer.getInstance().postFrameCallback(this)
        }
    }
    private val fpsTick = object : Runnable {
        override fun run() {
            vsyncFps = vsyncCount
            vsyncCount = 0
            showHud()
            ui.postDelayed(this, 1000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val fp = PairUri.hex(intent.getStringExtra(EXTRA_FP), 32)
        val addr = intent.getStringExtra(EXTRA_HOST)
        val port = intent.getIntExtra(EXTRA_PORT, 0)
        if (fp == null || addr == null || port !in 1..65535) {
            finish()
            return
        }
        host = PairedHost(fp, addr, port)
        token = PairUri.hex(intent.getStringExtra(EXTRA_TOKEN), 16)
        store = HostStore(this)

        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        window.attributes = window.attributes.also {
            it.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
            display?.let { d -> it.preferredDisplayModeId = bestMode(d).modeId }
        }
        // From API 35 (our target) windows are edge-to-edge already and the call is deprecated.
        if (Build.VERSION.SDK_INT < 35) window.setDecorFitsSystemWindows(false)
        hideSystemBars()

        val surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(this)
        hud = TextView(this).apply {
            setTextColor(Color.WHITE)
            setBackgroundColor(0x99000000.toInt())
            typeface = Typeface.MONOSPACE
            textSize = 14f
            setPadding(24, 12, 24, 12)
        }
        setContentView(
            FrameLayout(this).apply {
                setBackgroundColor(Color.BLACK)
                addView(surfaceView, ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
                addView(hud, FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP or Gravity.START))
            },
        )
        showHud()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) hideSystemBars()
    }

    override fun onResume() {
        super.onResume()
        if (!::hud.isInitialized) return // bad extras: onCreate finished the activity
        Choreographer.getInstance().postFrameCallback(vsync)
        ui.post(fpsTick)
    }

    override fun onPause() {
        Choreographer.getInstance().removeFrameCallback(vsync)
        ui.removeCallbacks(fpsTick)
        super.onPause()
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (event.actionMasked == MotionEvent.ACTION_POINTER_DOWN && event.pointerCount == 3) {
            hudVisible = !hudVisible
            showHud()
        }
        return true
    }

    private fun hideSystemBars() {
        window.insetsController?.let {
            it.hide(WindowInsets.Type.systemBars())
            it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
    }

    /** The highest refresh rate the panel offers at its current resolution. */
    private fun bestMode(d: android.view.Display): android.view.Display.Mode {
        val cur = d.mode
        return d.supportedModes
            .filter { it.physicalWidth == cur.physicalWidth && it.physicalHeight == cur.physicalHeight }
            .maxByOrNull { it.refreshRate } ?: cur
    }

    override fun surfaceCreated(holder: SurfaceHolder) {}

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        if (handle != 0L) return
        val d = display ?: return
        val mode = bestMode(d)
        // Ask the compositor to hold the panel at the stream rate. API 30 has no strategy argument;
        // from API 31 CHANGE_FRAME_RATE_ALWAYS also lets it switch modes (even with a visible seam) to get there.
        if (Build.VERSION.SDK_INT >= 31) {
            holder.surface.setFrameRate(
                mode.refreshRate,
                Surface.FRAME_RATE_COMPATIBILITY_FIXED_SOURCE,
                Surface.CHANGE_FRAME_RATE_ALWAYS,
            )
        } else {
            holder.surface.setFrameRate(mode.refreshRate, Surface.FRAME_RATE_COMPATIBILITY_FIXED_SOURCE)
        }
        val w = maxOf(mode.physicalWidth, mode.physicalHeight)
        val h = minOf(mode.physicalWidth, mode.physicalHeight) // the activity is landscape
        handle = VetroCore.nativeStart(
            holder.surface,
            host.address,
            host.port,
            host.fingerprint,
            token,
            filesDir.resolve("identity.bin").path,
            w,
            h,
            (mode.refreshRate * 1000).roundToInt(),
            resources.displayMetrics.densityDpi,
        )
        message = if (handle == 0L) "Invalid connection settings." else "Connecting to ${host.address}:${host.port}..."
        if (handle != 0L) ui.post(poll)
        showHud()
    }

    /** Stops the session first: the Surface must outlive the Rust decoder that writes into it. */
    override fun surfaceDestroyed(holder: SurfaceHolder) {
        ui.removeCallbacks(poll)
        if (handle == 0L) return
        savePairing()
        VetroCore.nativeStop(handle)
        handle = 0L
    }

    /** Persist the host only once the host confirmed the pairing, never before. */
    private fun savePairing() {
        if (token != null && VetroCore.nativePaired(handle)) {
            store.save(host)
            token = null
        }
    }

    private fun refresh() {
        savePairing()
        message = when (VetroCore.nativeState(handle)) {
            VetroCore.STATE_CONNECTING -> "Connecting to ${host.address}:${host.port}..."
            VetroCore.STATE_STREAMING -> "Streaming"
            VetroCore.STATE_ENDED -> "The session ended."
            VetroCore.STATE_NOT_PAIRED -> "Not paired: the code was wrong, used or expired. Go back and pair again."
            else -> "Connection failed. Check the Wi-Fi, the address and that the PC engine is running."
        }
        showHud()
    }

    private fun showHud() {
        // Problems stay on screen even with the HUD switched off.
        val problem = message.startsWith("Not paired") || message.startsWith("Connection failed") || message.startsWith("Invalid")
        hud.visibility = if (hudVisible || problem) View.VISIBLE else View.GONE
        val rate = display?.refreshRate?.roundToInt() ?: 0
        hud.text = "$message\npanel $rate Hz, vsync $vsyncFps/s"
    }
}
