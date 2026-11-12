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
        versionName = System.getenv("BAMDAD_VERSION_NAME") ?: "0.4.4"
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

    lint {
        // Release CI only needs to assemble signed APKs. Skipping lint avoids
        // a second dependency graph (groovy/kotlin-reflect) that can fail on
        // transient Maven outages and has no effect on the app.
        checkReleaseBuilds = false
        abortOnError = false
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            if (keystorePath != null) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    // Always build a single universal APK. Publishing ABI splits led users to
    // download a split APK directly, which Android rejects at install time
    // ("There's a problem with the app file").
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
