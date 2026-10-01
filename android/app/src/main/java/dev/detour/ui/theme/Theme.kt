package dev.detour.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import dev.detour.core.Prefs

/**
 * The colour scheme the app is actually rendering with, so screens can read it
 * without re-deriving whether dynamic colour is on.
 */
val LocalDynamicColor = staticCompositionLocalOf { true }

/**
 * A brand palette for devices without dynamic colour.
 *
 * Hand-picked rather than generated, because the generator that produces the
 * tonal ramps at runtime is an Android 12 API. These are the two anchors the
 * rest of the app leans on: a blue that reads as "connected" and a warm grey
 * that stays out of the way.
 */
private val FallbackLight: ColorScheme = lightColorScheme(
    primary = Color(0xFF2B4BD8),
    onPrimary = Color.White,
    primaryContainer = Color(0xFFDCE1FF),
    onPrimaryContainer = Color(0xFF001158),
    secondary = Color(0xFF5A5D72),
    onSecondary = Color.White,
    secondaryContainer = Color(0xFFDFE1F9),
    onSecondaryContainer = Color(0xFF171B2C),
    tertiary = Color(0xFF76546E),
    onTertiary = Color.White,
    tertiaryContainer = Color(0xFFFFD7F3),
    onTertiaryContainer = Color(0xFF2C1229),
    error = Color(0xFFBA1A1A),
    onError = Color.White,
    errorContainer = Color(0xFFFFDAD6),
    onErrorContainer = Color(0xFF410002),
    surface = Color(0xFFFBF8FF),
    onSurface = Color(0xFF1B1B21),
    surfaceVariant = Color(0xFFE2E1EC),
    onSurfaceVariant = Color(0xFF45464F),
    outline = Color(0xFF767680),
)

private val FallbackDark: ColorScheme = darkColorScheme(
    primary = Color(0xFFB8C3FF),
    onPrimary = Color(0xFF00218A),
    primaryContainer = Color(0xFF0F35BC),
    onPrimaryContainer = Color(0xFFDCE1FF),
    secondary = Color(0xFFC3C5DD),
    onSecondary = Color(0xFF2C2F42),
    secondaryContainer = Color(0xFF424659),
    onSecondaryContainer = Color(0xFFDFE1F9),
    tertiary = Color(0xFFE5BAD8),
    onTertiary = Color(0xFF44273F),
    tertiaryContainer = Color(0xFF5D3D57),
    onTertiaryContainer = Color(0xFFFFD7F3),
    error = Color(0xFFFFB4AB),
    onError = Color(0xFF690005),
    errorContainer = Color(0xFF93000A),
    onErrorContainer = Color(0xFFFFDAD6),
    surface = Color(0xFF121318),
    onSurface = Color(0xFFE3E1E9),
    surfaceVariant = Color(0xFF45464F),
    onSurfaceVariant = Color(0xFFC6C5D0),
    outline = Color(0xFF90909A),
)

/**
 * Whether the device can generate a palette from the wallpaper.
 *
 * Material You needs Android 12. Below that there is no `dynamicLightColorScheme`
 * to call, so the fallback above is the palette rather than a degraded version
 * of one.
 */
fun supportsDynamicColor(): Boolean = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S

/**
 * A seed for a hand-built palette: a hue, and how saturated the result should be.
 *
 * Expressed in HSL rather than as an RGB literal because the generator needs the
 * hue to derive the whole ramp, and recovering it from RGB would be a conversion
 * whose only purpose is to be undone. `dynamic` is deliberately absent: it is not
 * a seed, it is the instruction "ask the wallpaper instead".
 */
private data class Seed(val hue: Float, val saturation: Float)

private val Seeds: Map<String, Seed> = mapOf(
    "brand" to Seed(hue = 228f, saturation = 0.72f),
    "green" to Seed(hue = 150f, saturation = 0.52f),
    "orange" to Seed(hue = 28f, saturation = 0.86f),
    "violet" to Seed(hue = 272f, saturation = 0.58f),
)

/**
 * A full `ColorScheme` derived from one seed.
 *
 * One function for every seed rather than eight hand-written schemes: the point of
 * a theme colour is that it is the *same* palette in a different hue, so a per-seed
 * table would be eight chances for the palettes to drift apart. The lightness
 * values are the ones Material's own tonal ramps land on — a 40% primary on light,
 * 80% on dark, a near-white surface, and containers a step either side.
 *
 * The neutral roles (surface, outline) keep a trace of the hue so the result reads
 * as one palette rather than a tinted button on a grey screen.
 */
private fun seededScheme(seed: Seed, dark: Boolean): ColorScheme {
    fun hsl(lightness: Float, saturation: Float = seed.saturation, hueShift: Float = 0f) =
        Color.hsl(
            hue = (seed.hue + hueShift + 360f) % 360f,
            saturation = saturation.coerceIn(0f, 1f),
            lightness = lightness.coerceIn(0f, 1f),
        )

    return if (!dark) {
        lightColorScheme(
            primary = hsl(0.40f),
            onPrimary = Color.White,
            primaryContainer = hsl(0.90f, seed.saturation * 0.55f),
            onPrimaryContainer = hsl(0.18f),
            secondary = hsl(0.44f, seed.saturation * 0.34f),
            onSecondary = Color.White,
            secondaryContainer = hsl(0.90f, seed.saturation * 0.34f),
            onSecondaryContainer = hsl(0.20f, seed.saturation * 0.40f),
            tertiary = hsl(0.42f, seed.saturation * 0.55f, hueShift = 60f),
            onTertiary = Color.White,
            tertiaryContainer = hsl(0.90f, seed.saturation * 0.45f, hueShift = 60f),
            onTertiaryContainer = hsl(0.20f, seed.saturation * 0.60f, hueShift = 60f),
            error = Color(0xFFBA1A1A),
            onError = Color.White,
            errorContainer = Color(0xFFFFDAD6),
            onErrorContainer = Color(0xFF410002),
            surface = hsl(0.98f, seed.saturation * 0.08f),
            onSurface = hsl(0.14f, seed.saturation * 0.10f),
            surfaceVariant = hsl(0.90f, seed.saturation * 0.12f),
            onSurfaceVariant = hsl(0.34f, seed.saturation * 0.14f),
            outline = hsl(0.55f, seed.saturation * 0.12f),
        )
    } else {
        darkColorScheme(
            primary = hsl(0.80f, seed.saturation * 0.72f),
            onPrimary = hsl(0.22f, seed.saturation * 0.85f),
            primaryContainer = hsl(0.34f, seed.saturation * 0.80f),
            onPrimaryContainer = hsl(0.90f, seed.saturation * 0.55f),
            secondary = hsl(0.80f, seed.saturation * 0.26f),
            onSecondary = hsl(0.26f, seed.saturation * 0.30f),
            secondaryContainer = hsl(0.36f, seed.saturation * 0.28f),
            onSecondaryContainer = hsl(0.90f, seed.saturation * 0.30f),
            tertiary = hsl(0.80f, seed.saturation * 0.40f, hueShift = 60f),
            onTertiary = hsl(0.26f, seed.saturation * 0.40f, hueShift = 60f),
            tertiaryContainer = hsl(0.36f, seed.saturation * 0.42f, hueShift = 60f),
            onTertiaryContainer = hsl(0.90f, seed.saturation * 0.40f, hueShift = 60f),
            error = Color(0xFFFFB4AB),
            onError = Color(0xFF690005),
            errorContainer = Color(0xFF93000A),
            onErrorContainer = Color(0xFFFFDAD6),
            surface = hsl(0.09f, seed.saturation * 0.12f),
            onSurface = hsl(0.90f, seed.saturation * 0.08f),
            surfaceVariant = hsl(0.30f, seed.saturation * 0.12f),
            onSurfaceVariant = hsl(0.80f, seed.saturation * 0.12f),
            outline = hsl(0.60f, seed.saturation * 0.12f),
        )
    }
}

/**
 * The app theme.
 *
 * **`MaterialTheme`, not `MaterialExpressiveTheme`, and that is forced rather
 * than chosen.** The design called for the expressive theme, and the previous
 * code claimed in a comment to use it while actually calling `MaterialTheme` —
 * so the claim was wrong either way. The reason it stays `MaterialTheme` is
 * concrete: the only material3 the BOM resolves to is `1.4.0`, and in `1.4.0`
 * `MaterialExpressiveTheme`, `MotionScheme` and the
 * `ExperimentalMaterial3ExpressiveApi` marker are all `internal` — the compiler
 * rejects every reference with "it is internal in file". Both BOMs available
 * here (`2025.12.01` and `2026.09.00`) resolve material3 to the same `1.4.0`, so
 * there is no reachable version where the expressive *theme* is public.
 *
 * What survives is the part that does not need that theme:
 *
 * * `Shapes` — our own `detourShapes(cornerStyle)`, so the corner setting still
 *   takes effect. The expressive shape set was only ever a nicer default; ours
 *   replaces it outright.
 * * Motion — the expressive *springs* are reproduced by hand in [DetourMotion]
 *   (`spring`, which is public), so components that read `LocalDetourMotion`
 *   still move with weight.
 * * The floating bar — `ShortNavigationBar` is public in `1.4.0` (the compiler
 *   did not flag it), so the bar itself is still the expressive component.
 *
 * `themeColor`, `fontScale` and `cornerStyle` are the settings that only mean
 * anything once they reach here — a palette, a type scale and a shape scale —
 * so they are parameters rather than something a screen applies afterwards. A
 * screen cannot apply them: it reads colour and type from `MaterialTheme`, and
 * the theme is the only thing that can set them. The glass is deliberately *not*
 * a parameter any more: its inputs are two switches and five numbers, and the
 * resolver reads `Prefs` directly rather than having eight arguments threaded
 * through every caller.
 */
@Composable
fun DetourTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    dynamicColor: Boolean = true,
    themeColor: String = "dynamic",
    fontScale: Float = 1f,
    cornerStyle: String = "medium",
    content: @Composable () -> Unit,
) {
    val context = LocalContext.current
    // A named seed replaces dynamic colour rather than layering on top of it:
    // "暖橙" and "follow the wallpaper" are answers to the same question, and
    // letting the wallpaper win would make the seed a control that changes
    // nothing — which is exactly the bug this parameter exists to fix.
    val seed = Seeds[themeColor]
    val useDynamic = seed == null && dynamicColor && supportsDynamicColor()

    val scheme = remember(useDynamic, darkTheme, seed, context) {
        when {
            useDynamic && darkTheme -> dynamicDarkColorScheme(context)
            useDynamic -> dynamicLightColorScheme(context)
            seed != null -> seededScheme(seed, darkTheme)
            darkTheme -> FallbackDark
            else -> FallbackLight
        }
    }

    val shapes = remember(cornerStyle) { detourShapes(cornerStyle) }
    // Scaled here rather than applied by a screen afterwards: every screen reads
    // its text sizes from `MaterialTheme.typography`, so scaling at the source is
    // what makes the setting reach all of them.
    val typography = remember(fontScale) { detourTypography(fontScale) }
    // The glass switch and the five glass numbers are read here, in the
    // composable body, so that they are part of the `remember` key set below.
    // **This is load-bearing, not tidiness.** `resolveDetourGlass` reads `Prefs`
    // itself; if this `remember` did not also key on its inputs, flipping the
    // switch or dragging a slider would change the stored value, the theme would
    // recompute nothing, and the screen would not move — the exact "control that
    // does nothing" defect this project treats as its cardinal sin. Keying on
    // them re-runs the resolver on every change.
    //
    // They are read unconditionally, even with the switch off, for two reasons: an
    // `if` around the read would make the composition's subscriptions depend on
    // the switch and re-subscribe on every toggle, and `Prefs.of` is a cached
    // singleton so the reads themselves are free.
    //
    // The alternative was to add seven parameters to `DetourTheme` and have
    // `MainActivity` pass them in. That would spread the knowledge of these
    // settings into every caller of the theme and force an edit to a file this
    // change does not own, for no benefit over reading the one source of truth
    // directly.
    val prefs = Prefs.of(context)
    val glass = remember(
        scheme,
        context,
        prefs.glassEnabled,
        prefs.glassBlur,
        prefs.glassTint,
        prefs.glassLens,
        prefs.glassHighlight,
        prefs.glassBorder,
    ) {
        resolveDetourGlass(scheme, context)
    }
    val spacing = remember { DetourSpacing() }
    val motion = remember { DetourMotion() }

    CompositionLocalProvider(
        LocalDynamicColor provides useDynamic,
        LocalDetourSpacing provides spacing,
        LocalDetourMotion provides motion,
        LocalDetourGlass provides glass,
    ) {
        MaterialTheme(
            colorScheme = scheme,
            shapes = shapes,
            typography = typography,
            content = content,
        )
    }
}
