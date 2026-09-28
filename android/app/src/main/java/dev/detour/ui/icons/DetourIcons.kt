package dev.detour.ui.icons

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.PathBuilder
import androidx.compose.ui.graphics.vector.path
import androidx.compose.ui.unit.dp

/**
 * The icons this app needs that `material-icons-core` does not ship.
 *
 * **Why these are hand-built rather than a dependency.** The alternative is
 * `material-icons-extended`, and it is not an option: it is deprecated, and it
 * would add tens of thousands of vector assets to an APK that needs three. The
 * three paths below are the official Material Icons 24dp geometry, transcribed
 * into `ImageVector`s so they are compiled in rather than linked.
 *
 * **Why the commands are spelled out instead of using `materialIcon`.** That
 * helper lives in the icons artifact and is not part of the public surface a
 * consumer may rely on, so an icon built with it can stop compiling on a version
 * bump with no deprecation to warn anyone. `ImageVector.Builder` and `PathBuilder`
 * are stable public API, so the icons are built from those directly.
 *
 * Every icon is 24x24 with the same viewport as the core set, so a hand-built
 * icon and a core icon sit next to each other in the navigation bar without a
 * size jump. `Icon()` tints them, so the fill colour here is irrelevant — it
 * matches the core icons' `Color.Black` for the same reason they use it.
 */
object DetourIcons {

    /**
     * A filled square — "stop".
     *
     * The power control's icon for the *connected* state. The unconnected state
     * uses the core `PlayArrow`, and the two being different shapes is the whole
     * point: the button used to draw a play triangle in both states, so the one
     * control whose entire job is to say which way it will go said nothing.
     */
    val Stop: ImageVector by lazy {
        detourIcon("Detour.Stop") {
            moveTo(6f, 6f)
            horizontalLineTo(18f)
            verticalLineTo(18f)
            horizontalLineTo(6f)
            close()
        }
    }

    /**
     * Two bars — "pause".
     *
     * The log screen's freeze control. It used to draw a cross (`Icons.Filled.Clear`),
     * which is the glyph every other app uses for "close", sitting next to a
     * dustbin that means "clear" — two buttons that read as "close" and "delete"
     * while doing "pause" and "clear".
     */
    val Pause: ImageVector by lazy {
        detourIcon("Detour.Pause") {
            moveTo(6f, 19f)
            horizontalLineToRelative(4f)
            verticalLineTo(5f)
            horizontalLineTo(6f)
            verticalLineToRelative(14f)
            close()
            moveToRelative(8f, -14f)
            verticalLineToRelative(14f)
            horizontalLineToRelative(4f)
            verticalLineTo(5f)
            horizontalLineToRelative(-4f)
            close()
        }
    }

    /**
     * Two overlapping pages — "copy".
     *
     * The log screen's "copy all" action. The icon this wants is
     * `Icons.Filled.ContentCopy`, and it is **not** available: this project
     * depends on `material-icons-core` on purpose (the `build.gradle.kts` comment
     * on that dependency says why), and `ContentCopy` only ships in the
     * deprecated `material-icons-extended`. So its official 24dp geometry is
     * transcribed here, exactly the way [Stop], [Pause] and [Subject] are.
     *
     * Deliberately not a substitute from the core set. `Share` is already the
     * button beside it and means "send the file out"; the two actions sit next
     * to each other, so reusing one glyph for both would leave the pair
     * indistinguishable at a glance — which is the whole reason a copy icon is
     * being hand-built instead of borrowed.
     */
    val Copy: ImageVector by lazy {
        detourIcon("Detour.Copy") {
            moveTo(16f, 1f)
            horizontalLineTo(4f)
            curveToRelative(-1.1f, 0f, -2f, 0.9f, -2f, 2f)
            verticalLineToRelative(14f)
            horizontalLineToRelative(2f)
            verticalLineTo(3f)
            horizontalLineToRelative(12f)
            verticalLineTo(1f)
            close()
            moveToRelative(3f, 4f)
            horizontalLineTo(8f)
            curveToRelative(-1.1f, 0f, -2f, 0.9f, -2f, 2f)
            verticalLineToRelative(14f)
            curveToRelative(0f, 1.1f, 0.9f, 2f, 2f, 2f)
            horizontalLineToRelative(11f)
            curveToRelative(1.1f, 0f, 2f, -0.9f, 2f, -2f)
            verticalLineTo(7f)
            curveToRelative(0f, -1.1f, -0.9f, -2f, -2f, -2f)
            close()
            moveToRelative(0f, 16f)
            horizontalLineTo(8f)
            verticalLineTo(7f)
            horizontalLineToRelative(11f)
            verticalLineToRelative(14f)
            close()
        }
    }

    /**
     * A page of text lines — "log".
     *
     * The log destination's icon. It used to be `Icons.Filled.Info` (ⓘ), which is
     * the glyph for "about this app", so the third tab of four said the wrong
     * thing. Deliberately not the bulleted `List` the rules tab uses: two tabs
     * that both read as "a list" is the ambiguity this icon exists to remove.
     */
    val Subject: ImageVector by lazy {
        detourIcon("Detour.Subject") {
            moveTo(14f, 17f)
            horizontalLineTo(4f)
            verticalLineToRelative(2f)
            horizontalLineToRelative(10f)
            verticalLineToRelative(-2f)
            close()
            moveToRelative(6f, -8f)
            horizontalLineTo(4f)
            verticalLineToRelative(2f)
            horizontalLineToRelative(16f)
            verticalLineTo(9f)
            close()
            moveTo(4f, 15f)
            horizontalLineToRelative(16f)
            verticalLineToRelative(-2f)
            horizontalLineTo(4f)
            verticalLineToRelative(2f)
            close()
            moveTo(4f, 5f)
            verticalLineToRelative(2f)
            horizontalLineToRelative(16f)
            verticalLineTo(5f)
            horizontalLineTo(4f)
            close()
        }
    }
}

private fun detourIcon(name: String, block: PathBuilder.() -> Unit): ImageVector =
    ImageVector.Builder(
        name = name,
        defaultWidth = 24.dp,
        defaultHeight = 24.dp,
        viewportWidth = 24f,
        viewportHeight = 24f,
    ).path(
        fill = SolidColor(Color.Black),
        pathBuilder = block,
    ).build()
