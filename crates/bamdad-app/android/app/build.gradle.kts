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
        versionName = System.getenv("BAMDAD_VERSION_NAME") ?: "0.5.7"
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

    // Publish standalone per-ABI APKs instead of split or universal packages:
    // each one installs directly and stays around 60 MB, even as more cores
    // are added later.
    flavorDimensions += "abi"
    productFlavors {
        create("arm64") {
            dimension = "abi"
            ndk { abiFilters += "arm64-v8a" }
        }
        create("x86_64") {
            dimension = "abi"
            ndk { abiFilters += "x86_64" }
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
    // WireGuard and AmneziaWG use the vendored userspace backend.
    implementation(project(":amneziawg-tunnel"))
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
    implementation("net.lingala.zip4j:zip4j:2.11.5")
}
