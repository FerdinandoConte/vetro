plugins {
    id("com.android.application")
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
        ndk { abiFilters += "arm64-v8a" }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
