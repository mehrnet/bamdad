import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.mehrnet.bamdad"
    compileSdk = 35
    buildToolsVersion = "35.0.0"
    ndkVersion = "27.0.12077973"

    defaultConfig {
        applicationId = "com.mehrnet.bamdad"
        minSdk = 29
        targetSdk = 35
        versionCode = (System.getenv("BAMDAD_VERSION_CODE") ?: "4").toInt()
        versionName = System.getenv("BAMDAD_VERSION_NAME") ?: "0.1.5"
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlin {
        compilerOptions {
            jvmTarget.set(JvmTarget.JVM_17)
        }
    }

    val keystorePath: String? = System.getenv("ANDROID_KEYSTORE_PATH")
    if (keystorePath != null) {
        signingConfigs {
            create("release") {
                storeFile = file(keystorePath)
                storePassword = System.getenv("ANDROID_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("ANDROID_KEYSTORE_ALIAS")
                keyPassword =
                    System.getenv("ANDROID_KEY_PASSWORD")
                        ?: System.getenv("ANDROID_KEYSTORE_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            if (keystorePath != null) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    // Emulator smoke builds disable ABI splits so the debug APK is a single
    // universal package containing the x86_64 jniLibs built for testing.
    val emulatorBuild = !System.getenv("BAMDAD_ANDROID_EMULATOR_ABI").isNullOrBlank()
    splits {
        abi {
            isEnable = !emulatorBuild
            reset()
            include("arm64-v8a", "x86_64")
            isUniversalApk = true
        }
    }

    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }
}

dependencies {
    // The Android tunnel (tun2socks) is packaged as a native-library
    // executable by scripts/fetch-android-tun2socks.sh; no AAR is needed.
}
