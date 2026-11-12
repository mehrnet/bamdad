import com.android.build.api.dsl.SettingsExtension

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
    // The vendored amneziawg tunnel module resolves its own aliases through
    // the "libs" catalog. Use a local catalog so the submodule builds with
    // Bamdad's AGP/NDK versions instead of its pinned upstream toolchain.
    versionCatalogs {
        create("libs") {
            from(files("gradle/amneziawg-libs.versions.toml"))
        }
    }
}

plugins {
    id("com.android.settings") version "8.10.1"
}

rootProject.name = "bamdad-app"
include(":app")
include(":amneziawg-tunnel")
project(":amneziawg-tunnel").projectDir = file("vendor/amneziawg-android/tunnel")

configure<SettingsExtension> {
    buildToolsVersion = "35.0.0"
    compileSdk = 35
    minSdk = 29
    ndkVersion = "27.0.12077973"
}
