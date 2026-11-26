plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.vetro.app"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.vetro.app"
        minSdk = 30
        targetSdk = 35
        versionCode = 1
        versionName = "0.0.0"
        ndk { abiFilters += "arm64-v8a" }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    jvmToolchain(17)
}
