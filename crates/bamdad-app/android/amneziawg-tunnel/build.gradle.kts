plugins {
    id("com.android.library")
}

android {
    namespace = "org.amnezia.awg.tunnel"
    compileSdk = 35
    buildToolsVersion = "35.0.0"
    ndkVersion = "27.0.12077973"

    defaultConfig {
        minSdk = 29
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    externalNativeBuild {
        cmake {
            path = file("../vendor/amneziawg-android/tunnel/tools/CMakeLists.txt")
        }
    }

    buildTypes {
        all {
            externalNativeBuild {
                cmake {
                    targets("libwg-go.so", "libwg.so", "libwg-quick.so")
                    arguments(
                        "-DGRADLE_USER_HOME=${project.gradle.gradleUserHomeDir}",
                        "-DANDROID_PACKAGE_NAME=com.mehrnet.bamdad",
                    )
                }
            }
        }
    }

    sourceSets {
        getByName("main") {
            java.srcDirs("../vendor/amneziawg-android/tunnel/src/main/java")
            manifest.srcFile("../vendor/amneziawg-android/tunnel/src/main/AndroidManifest.xml")
        }
    }

    lint {
        abortOnError = false
        checkReleaseBuilds = false
    }
}

dependencies {
    implementation("androidx.annotation:annotation:1.7.1")
    implementation("androidx.collection:collection:1.4.0")
    compileOnly("com.google.code.findbugs:jsr305:3.0.2")
}
