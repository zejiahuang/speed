// Plugin versions live here and are applied in :app, so the two cannot drift.
//
// Library versions come from the Compose BOM declared in app/build.gradle.kts.
// The BOM resolves material3 to 1.4.0. In 1.4.0 the expressive *theme* API
// (`MaterialExpressiveTheme`, `MotionScheme`, `ExperimentalMaterial3ExpressiveApi`)
// is `internal` and cannot be called from an app module, so the app builds its
// theme on the standard `MaterialTheme` and supplies its own shapes and springs.
// Nothing is pinned here; the module inherits the version from the BOM.
plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.kotlin.android) apply false
    // The Compose compiler ships with Kotlin 2.x and is applied by this plugin.
    // Without it the build fails with a message about a missing compiler plugin.
    alias(libs.plugins.kotlin.compose) apply false
}
