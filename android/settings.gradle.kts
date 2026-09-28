// Plugin resolution. `google()` is where the Android Gradle plugin lives, and it
// has to be declared here rather than only in the module, because Gradle resolves
// plugins before it reads any module.
pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    // A module declaring its own repository is almost always a mistake here: the
    // build would then resolve against a different set than everything else.
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "detour"
include(":app")
