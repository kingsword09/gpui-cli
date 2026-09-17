// Packages the Rust library produced by `gpui build android`.

import java.util.Properties

plugins {
    id("com.android.application")
}

android {
    namespace = "dev.gpui.mobile"
    compileSdk = 34

    // Use the same NDK as cargo-ndk, including its native symbol stripper.
    providers.environmentVariable("ANDROID_NDK_HOME").orNull
        ?.takeIf { it.isNotBlank() }
        ?.let { ndkHome ->
            val ndkDirectory = file(ndkHome)
            val ndkProperties = Properties().apply {
                ndkDirectory.resolve("source.properties").inputStream().use { load(it) }
            }
            ndkPath = ndkDirectory.absolutePath
            // AGP also checks the version, even when an explicit path is set.
            ndkVersion = ndkProperties.getProperty("Pkg.Revision")
                ?: error("Missing Pkg.Revision in ANDROID_NDK_HOME/source.properties")
        }

    defaultConfig {
        applicationId = "{{BUNDLE_ID}}"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "1.0.0"

        // Tell NativeActivity which .so to load.
        // This must match the cdylib / example output name.
        ndk {
            abiFilters += listOf("arm64-v8a")
        }

        // Forward the library name to the manifest via a placeholder.
        manifestPlaceholders["nativeLibraryName"] = "{{APP_LIB_NAME}}"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
        debug {
            isDebuggable = true
            isJniDebuggable = true
        }
    }

    // We do NOT use CMake / ndk-build — the native library is compiled
    // externally via cargo-ndk and placed directly into jniLibs.
    //
    // Disable the built-in native build system so Gradle doesn't look for
    // a CMakeLists.txt or Android.mk.
    externalNativeBuild {
        // Intentionally left empty.
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }

    // Tell Gradle where the pre-built .so files live.
    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }

    packaging {
        // gpui-mobile is linked into the app. cargo-ndk also copies its
        // standalone cdylib, which NativeActivity does not load.
        // Gradle strips the packaged app library; full debug symbols remain
        // in Cargo's target directory for native debugging.
        jniLibs {
            excludes += listOf("**/libgpui_mobile.so", "**/libgpui_mobile-*.so")
        }
    }

    lint {
        abortOnError = false
        checkReleaseBuilds = false
    }
}

dependencies {
    // AndroidX core for NotificationCompat (used by GpuiNotifications)
    implementation("androidx.core:core:1.12.0")
    // AndroidX SplashScreen compat (used by GpuiActivity to hold splash until native init)
    implementation("androidx.core:core-splashscreen:1.0.1")
    // AndroidX Biometric for BiometricPrompt (used by GpuiAuthActivity)
    implementation("androidx.biometric:biometric:1.1.0")
    // AndroidX Media for MediaSessionCompat (used by GpuiMediaSession for system controls)
    implementation("androidx.media:media:1.7.1")
}
