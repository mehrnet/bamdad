import org.gradle.api.tasks.Sync

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
                    targets("libbamdad_awg_go.so")
                    arguments(
                        "-DGRADLE_USER_HOME=${project.gradle.gradleUserHomeDir}",
                        "-DANDROID_PACKAGE_NAME=com.mehrnet.bamdad",
                    )
                }
            }
        }
    }

    val amneziaJava = layout.buildDirectory.dir("generated/amneziaJava")
    val syncAmneziaJava = tasks.register<Sync>("syncAmneziaJava") {
        from("../vendor/amneziawg-android/tunnel/src/main/java") {
            exclude("org/amnezia/awg/backend/GoBackend.java")
        }
        from("src/main/java")
        into(amneziaJava)
    }
    tasks.matching { it.name == "preBuild" }.configureEach {
        dependsOn(syncAmneziaJava)
    }

    sourceSets {
        getByName("main") {
            java.setSrcDirs(listOf(amneziaJava))
            manifest.srcFile("../vendor/amneziawg-android/tunnel/src/main/AndroidManifest.xml")
        }
    }

    val patchAwgLibraryName = tasks.register("patchAwgLibraryName") {
        doLast {
            val tunnelDir = file("../vendor/amneziawg-android/tunnel/tools")
            val makefile = File(tunnelDir, "libwg-go/Makefile")
            val cmake = File(tunnelDir, "CMakeLists.txt")
            makefile.writeText(makefile.readText().replace("libwg-go.so", "libbamdad_awg_go.so"))
            cmake.writeText(cmake.readText().replace("libwg-go.so", "libbamdad_awg_go.so"))
        }
    }
    tasks.matching { it.name == "preBuild" }.configureEach {
        dependsOn(patchAwgLibraryName)
    }
    tasks.matching { it.name.startsWith("configureCMake") }.configureEach {
        dependsOn(patchAwgLibraryName)
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
