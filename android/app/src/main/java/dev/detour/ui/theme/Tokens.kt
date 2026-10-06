package dev.detour.ui.theme

import android.content.Context
import androidx.compose.animation.core.FiniteAnimationSpec
import androidx.compose.animation.core.LinearOutSlowInEasing
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.VisibilityThreshold
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.material3.ColorScheme
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import dev.detour.core.Prefs

/**
 * The three design tokens the component library reads, provided once by
 * `DetourTheme`.
 *
 * They are composition locals rather than plain top-level `val`s so that a
 * `@Preview` can swap one out (a low-end device's glass, a dark palette) without
 * every component taking the value as a parameter. A component that hard-coded
 * `16.dp` or `MotionScheme.expressive()` directly could not be previewed in the
 * other configuration, which is exactly how the two duplicated `Card` styles
 * this library replaces came about.
 */

/** Spacing scale. Every margin in the library comes from one of these. */
data class DetourSpacing(
    val xs: Dp = 4.dp,
    val sm: Dp = 8.dp,
    val md: Dp = 12.dp,
    val lg: Dp = 16.dp,
    val xl: Dp = 24.dp,
    val screenH: Dp = 20.dp,
)

val LocalDetourSpacing = staticCompositionLocalOf { DetourSpacing() }

/**
 * Motion, as the expressive scheme's springs, reproduced by hand.
 *
 * `MotionScheme.expressive()` is `internal` in material3 1.4.0, so the specs it
 * would have returned are declared here instead: the spatial specs are springs
 * (which is what "expressive" means in practice — a thing that overshoots
 * slightly and settles), and the effects spec is a short ease-out for colour and
 * alpha, which should not bounce.
 *
 * Keeping them behind one type means a component reads `LocalDetourMotion`
 * rather than inventing its own `tween(300)`, which is what makes two screens
 * feel like one app.
 */
data class DetourMotion(
    val spatial: FiniteAnimationSpec<Float> = spring(
        dampingRatio = 0.8f,
        stiffness = Spring.StiffnessMediumLow,
    ),
    val fastSpatial: FiniteAnimationSpec<Float> = spring(
        dampingRatio = 0.8f,
        stiffness = Spring.StiffnessMedium,
    ),
    val effects: FiniteAnimationSpec<Float> = tween(
        durationMillis = 200,
        easing = LinearOutSlowInEasing,
    ),
    /**
     * The spec for a *size* change — a section opening, a card folding away.
     *
     * Separate from [spatial] because `expandVertically` / `shrinkVertically`
     * animate an `IntSize`, and a `FiniteAnimationSpec<Float>` will not do: the
     * types do not unify. The visibility threshold is what tells the spring when
     * a height is close enough to its target to stop, and without it a collapsing
     * section settles with a visible pixel of height left over.
     */
    val sizeSpatial: FiniteAnimationSpec<IntSize> = spring(
        dampingRatio = 0.8f,
        stiffness = Spring.StiffnessMediumLow,
        visibilityThreshold = IntSize.VisibilityThreshold,
    ),
    /**
     * The spec for a *slide* — a page arriving or leaving.
     *
     * A third type, for the same reason [sizeSpatial] is a second one: the slide
     * transitions animate an `IntOffset`, and a `FiniteAnimationSpec<Float>` does
     * not unify with that. The spring is [spatial]'s, to the figure, so a page
     * that slides and a card that folds are the same motion at two scales rather
     * than two motions that happen to be near each other.
     *
     * The visibility threshold is what tells the spring that an offset within a
     * pixel of its target is there; without it a page settles with a column of
     * pixels still showing along the edge it came from.
     */
    val offsetSpatial: FiniteAnimationSpec<IntOffset> = spring(
        dampingRatio = 0.8f,
        stiffness = Spring.StiffnessMediumLow,
        visibilityThreshold = IntOffset.VisibilityThreshold,
    ),
    /**
     * The spec for a thing that should read as *elastic* rather than merely
     * quick — currently only the navigation bar's indicator, which slides
     * between destinations.
     *
     * Deliberately underdamped compared with [spatial]'s `0.8`. That figure is
     * the house style for "a thing that moves and settles": it overshoots by a
     * pixel or two, which reads as weight. The owner asked for the indicator to
     * move "like jelly", and jelly is the one case where the overshoot *is* the
     * point — at `0.55` the indicator visibly passes its target and comes back,
     * which is legible as a material property rather than as a slow animation.
     * Raising this back towards `0.8` would quietly turn the effect off, so the
     * number is not a tuning knob to be tidied.
     *
     * `StiffnessMediumLow` rather than `StiffnessMedium` because a stiffer
     * spring finishes the bounce before the eye can register it: the whole
     * gesture is a few hundred milliseconds, and the wobble needs room inside
     * it. Both figures were chosen by eye on the emulator, not derived.
     */
    val jelly: FiniteAnimationSpec<Float> = spring(
        dampingRatio = 0.55f,
        stiffness = Spring.StiffnessMediumLow,
    ),
)

val LocalDetourMotion = staticCompositionLocalOf { DetourMotion() }

/**
 * The glass capability the device and the user have settled on.
 *
 * This is the *only* thing a component may read to decide how much frosted
 * surface to draw. The drawing itself lives in `LiquidGlass`, which samples the
 * app's backdrop through `io.github.kyant0:backdrop`; this type carries the
 * numbers that sampling spends, and whether it happens at all.
 *
 * `blurRadius` is a real blur now. It spent its whole life as a placeholder
 * precisely so this would not have to change shape: the tier already said "8dp"
 * or "16dp", and the value only needed a renderer that could spend it. That
 * renderer is here, so the number reaches the pixels for the first time.
 *
 * The two lens fields are new and are what makes the glass *refract* rather
 * than merely blur. Both are no-ops below Android 13, where `lens` has no
 * `RuntimeShader` to run in — see `LiquidGlass` for the API-level ladder.
 */
data class DetourGlass(
    val enabled: Boolean,
    val blurRadius: Dp,
    val tint: Color,
    val borderWidth: Dp,
    val highlightAlpha: Float,
    /**
     * The height of the band the lens bends, in dp.
     *
     * Not a free choice: the effect requires it to fit inside the shape's
     * smallest corner radius, so `LiquidGlass` clamps it against the shape it is
     * actually drawing. Zero means "no lens" — the `off` tier, and every device
     * below Android 13.
     */
    val lensHeight: Dp = 0.dp,
    /**
     * How far the lens pulls the backdrop sideways, in dp. Clamped at draw time
     * to the surface's smaller dimension, which is the effect's own ceiling.
     * Zero means "no lens".
     */
    val lensAmount: Dp = 0.dp,
) {
    companion object {
        val Disabled = DetourGlass(
            enabled = false,
            blurRadius = 0.dp,
            tint = Color.Transparent,
            borderWidth = 0.dp,
            highlightAlpha = 0f,
        )
    }
}

val LocalDetourGlass = staticCompositionLocalOf { DetourGlass.Disabled }

/**
 * Resolve the glass material from the single switch and the five numbers.
 *
 * The material is two halves and **the one switch drives both**:
 *
 * * **liquid** — the refraction (the lens), plus the specular highlight and the
 *   lit edge;
 * * **frost** — the blur: it samples and blurs the backdrop.
 *
 * They were two independently switchable halves until the owner merged them
 * (2026-10-01), on the argument that two master switches for one visual idea
 * read as two features. **The merge has a cost and it is the blur:** the halves
 * were independent, so "refraction over a sharp backdrop" — liquid glass without
 * the frosted panel, which is what the material looks like on a real device —
 * is no longer reachable, and every user who turns glass on now pays for the
 * blur, which is the expensive half (the hint string says so). If a cheap glass
 * mode is ever wanted, the switch to add is one that turns the *blur* off, not a
 * second master switch: the two booleans this replaced were the wrong shape
 * because they were symmetric, and the asymmetry is what the merge exposes.
 *
 * With the switch off the result is a flat opaque card: `enabled = false` and
 * the **opaque** `scheme.surfaceContainer` as the tint. That last part is not
 * incidental — `LiquidGlass` draws `glass.tint` as a plain fill when `enabled`
 * is false, and its comment records that the setting is "no glass", not "no
 * surface". A transparent tint here would make the card vanish instead of going
 * flat.
 *
 * The tint is shared by both halves rather than owned by one, because both
 * materials draw a base fill and text has to stay legible over whatever is
 * behind them.
 *
 * The numbers come from `Prefs` through `context`, so **the theme's `remember`
 * must key on the switch and the five numbers** or flipping the switch or
 * dragging a slider would not repaint — the "control that does nothing" defect
 * this project has already paid for twice. `DetourTheme` carries that key list.
 */
fun resolveDetourGlass(scheme: ColorScheme, context: Context): DetourGlass {
    val prefs = Prefs.of(context)
    if (!prefs.glassEnabled) {
        return DetourGlass(
            enabled = false,
            blurRadius = 0.dp,
            tint = scheme.surfaceContainer,
            borderWidth = 0.dp,
            highlightAlpha = 0f,
        )
    }
    return DetourGlass(
        enabled = true,
        blurRadius = prefs.glassBlur.dp,
        tint = scheme.surfaceContainer.copy(alpha = prefs.glassTint / 100f),
        borderWidth = (prefs.glassBorder / 100f * 2f).dp,
        highlightAlpha = prefs.glassHighlight / 100f,
        lensHeight = (prefs.glassLens * 0.75f).dp,
        lensAmount = prefs.glassLens.dp,
    )
}
