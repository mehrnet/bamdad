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
}

rootProject.name = "bamdad-app"
include(":app")
include(":amneziawg-tunnel")
project(":amneziawg-tunnel").projectDir = file("amneziawg-tunnel")
include(":openvpn3-tunnel")
project(":openvpn3-tunnel").projectDir = file("openvpn3-tunnel")
