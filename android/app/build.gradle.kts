plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "dev.vetro.app"
    compileSdk = 37
    ndkVersion = "28.2.13676358" // r28c, same NDK as cargo-ndk in CI

    defaultConfig {
        applicationId = "dev.vetro.app"
        minSdk = 30
        targetSdk = 37
        versionCode = 1
        versionName = "0.0.0"
        // x86_64 is for the emulator (ARCHITECTURE section 14); the tablet runs arm64-v8a.
        ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2026.11.01"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.activity:activity-compose:1.13.0")

    // QR scanning without Google Play Services (ADR-0010).
    val camerax = "1.7.0"
    implementation("androidx.camera:camera-camera2:$camerax")
    implementation("androidx.camera:camera-lifecycle:$camerax")
    implementation("androidx.camera:camera-view:$camerax")
    implementation("com.google.zxing:core:3.5.4")

    testImplementation("junit:junit:4.13.2")
}
