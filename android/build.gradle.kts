// Plugin and library versions live in gradle/libs.versions.toml. Plugins are
// applied in :app, so the root and the module cannot drift.
buildscript {
    repositories {
        google()
        mavenCentral()
    }
    dependencies {
        // AGP 9 ships Kotlin 2.2.10 as its built-in Kotlin baseline. The Compose
        // compiler plugin is 2.4.20 and must run against the same KGP, so raise
        // KGP explicitly. Keep this in sync with `kotlin` in the version catalog.
        classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:2.4.20")
    }
}
plugins {
    alias(libs.plugins.android.application) apply false
    // AGP 9 compiles Kotlin itself, so there is no `kotlin.android` plugin here.
    // The Compose compiler ships with Kotlin 2.x and is applied by this plugin.
    // Without it the build fails with a message about a missing compiler plugin.
    alias(libs.plugins.kotlin.compose) apply false
}
