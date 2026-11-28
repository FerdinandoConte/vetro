plugins {
    // AGP 9 ships built-in Kotlin: no separate kotlin-android plugin.
    id("com.android.application") version "9.5.3" apply false
    // Same version as the Kotlin that AGP 9.5.3 bundles (2.2.10).
    id("org.jetbrains.kotlin.plugin.compose") version "2.2.10" apply false
}
