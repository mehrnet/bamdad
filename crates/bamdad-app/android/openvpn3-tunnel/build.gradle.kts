plugins {
    id("com.android.library")
}

android {
    namespace = "com.mehrnet.bamdad.openvpn3"
    compileSdk = 35
    buildToolsVersion = "35.0.0"
    ndkVersion = "27.0.12077973"

    defaultConfig {
        minSdk = 29
    }

    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    lint {
        abortOnError = false
        checkReleaseBuilds = false
    }
}
