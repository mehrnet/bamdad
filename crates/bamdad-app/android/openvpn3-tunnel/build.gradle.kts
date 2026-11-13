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
        // Only these ABIs have vendored OpenVPN 3 dependencies. A release
        // build covers both; a debug build can narrow this with
        // -PbamdadAbis=x86_64 to skip the other cross-build.
        val abis = (project.findProperty("bamdadAbis") as String?)
            ?.split(',')
            ?.map(String::trim)
            ?.filter(String::isNotEmpty)
            ?: listOf("arm64-v8a", "x86_64")
        ndk { abiFilters += abis }
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
