package dev.detour.ui.components

import android.os.Build
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.shape.CornerBasedShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.unit.dp
import com.kyant.backdrop.backdrops.LayerBackdrop
import com.kyant.backdrop.drawBackdrop
import com.kyant.backdrop.effects.blur
import com.kyant.backdrop.effects.lens
import com.kyant.backdrop.effects.vibrancy
import dev.detour.ui.theme.DetourGlass
import dev.detour.ui.theme.LocalDetourGlass

/**
 * The app's backdrop: the layer the glass samples.
 *
 * Provided once, by the app body, from a `rememberLayerBackdrop` whose source
 * node is the decorative background. It is a composition local rather than a
 * parameter because the glass is drawn from a `Modifier` that has no way to
 * reach the app body — and because a caller that could pass its own backdrop
 * could point the glass at nothing and ship a surface that samples a blank
 * layer. Null when there is no app body around the caller (a `@Preview`, or a
 * screen hosted outside it), and the glass then takes the tint-and-border
 * fallback rather than failing to draw.
 */
val LocalLayerBackdrop = staticCompositionLocalOf<LayerBackdrop?> { null }

/**
 * The frosted-glass surface.
 *
 * **This samples the backdrop for real, and that reverses an earlier decision
 * recorded here.** The previous version of this file argued the opposite: that
 * Compose cannot read the pixels behind a composable — `Modifier.blur` blurs the
 * caller's own layer, not what is underneath — that a true refraction would need
 * a third-party sampler, and that the team had decided against one. The owner
 * has since reversed that call, because a tint does not refract and "磨砂玻璃"
 * was only ever an approximation of it. The app now depends on
 * `io.github.kyant0:backdrop` (see `app/build.gradle.kts`) and this surface
 * draws a genuine, blurred, lensed copy of what is behind it.
 *
 * The library's model has two sides, and both are present here:
 *
 * * a **source** — a `LayerBackdrop` the app body builds with
 *   `rememberLayerBackdrop` and marks with `Modifier.layerBackdrop`, which
 *   records the marked node's drawing into a graphics layer ([LocalLayerBackdrop]
 *   carries it here);
 * * a **consumer** — [Modifier.drawBackdrop], which samples that layer through a
 *   `RenderEffect` and clips the result to [shape].
 *
 * Three properties of the effect are not free choices, and each explains a guard
 * below:
 *
 * * **The order is fixed: colour filter → blur → lens.** The library documents
 *   it and a different order is a different picture, so `vibrancy()` runs first,
 *   then `blur()`, then `lens()`.
 * * **The lens needs Android 13.** The colour filters and the blur are
 *   `RenderEffect`s, which arrive with Android 12 (API 31); `lens` is a
 *   `RuntimeShader`, which needs API 33. The tiers therefore degrade in two
 *   steps — below 31 there is no sampling at all, 31–32 gets blur without the
 *   lens, and 33+ gets both — rather than assuming the newest device.
 * * **The lens needs a corner-based shape.** The library throws for anything
 *   else, so the shape is cast rather than trusted before `lens` is called.
 *
 * Below API 31, and wherever there is no backdrop to sample, the surface falls
 * back to exactly what it drew before this change: a translucent fill, a
 * specular border and a top highlight. It is worse than the real glass and
 * deliberately so — a device with no `RenderEffect` cannot sample, and a blank
 * surface would be a worse answer than an honest tint. When
 * [DetourGlass.enabled] is false the surface is a plain opaque fill with no
 * border or highlight, which is the path taken when both glass switches are off.
 */
@Composable
fun Modifier.liquidGlass(shape: Shape, glass: DetourGlass): Modifier {
    // Read here rather than taken as a parameter so a caller cannot hand the
    // glass a backdrop the app did not provide — see [LocalLayerBackdrop].
    val backdrop = LocalLayerBackdrop.current
    return drawGlass(shape, glass, backdrop)
}

/**
 * The actual drawing, split out of [liquidGlass] so the composable half stays a
 * one-line lookup and this half stays a plain, testable modifier factory.
 */
private fun Modifier.drawGlass(shape: Shape, glass: DetourGlass, backdrop: LayerBackdrop?): Modifier {
    val clipped = clip(shape)

    // ① Off. An opaque fill and nothing else: the off tier's tint is a fully
    //    opaque `surfaceContainer`, so this reads as a plain card. It must be
    //    opaque, not blank — the setting is "no glass", not "no surface".
    if (!glass.enabled) {
        return clipped.drawBehind { drawRect(color = glass.tint) }
    }

    // ② On, but nothing to sample: no backdrop was provided, or the device has
    //    no `RenderEffect` (API < 31). `drawBackdrop` would still draw the
    //    layer, unblurred and unrefracted, so it is not used at all here — the
    //    fallback is the tint, the border and the highlight the glass was before.
    //
    //    The API test is written out rather than taken from the library's
    //    `isRenderEffectSupported()`, and that is a deliberate trade. `backdrop`
    //    is pinned to 1.0.6 (see `app/build.gradle.kts` for why), and 1.0.6 has no
    //    `PlatformKt` at all — those two helpers arrived in 2.0.x. The bodies
    //    were read out of 2.0.1's bytecode to make sure inlining them is exact
    //    and not an approximation: `isRenderEffectSupported()` is literally
    //    `SDK_INT >= 31` and `isRuntimeShaderSupported()` literally `SDK_INT >= 33`,
    //    nothing more. So this is the same predicate, not a guess at it.
    if (backdrop == null || Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
        return clipped
            .drawBehind { drawGlassFill(glass) }
            .glassBorder(shape, glass)
    }

    // ③ The real thing.
    return clipped
        .drawBackdrop(
            backdrop = backdrop,
            shape = { shape },
            effects = {
                // Order is mandatory: colour filter, then blur, then lens.
                vibrancy()
                val radius = glass.blurRadius.toPx()
                if (radius > 0f) blur(radius = radius)
                // The lens is a RuntimeShader, so it only exists on Android 13+.
                // Guarding here is belt-and-braces — the library no-ops it on
                // older devices — but it also keeps the shape cast below honest.
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && glass.lensAmount > 0.dp) {
                    // The library throws `UnsupportedOperationException` for a
                    // shape it cannot derive corner radii from, so the cast is
                    // the guard, not a nicety: an arbitrary `Shape` must still
                    // render, it just does not get a lens.
                    val corner = shape as? CornerBasedShape
                    if (corner != null) {
                        // Both lens figures have hard ceilings the library does
                        // not enforce: the refraction height must fit the
                        // smallest corner radius, and the amount must fit the
                        // surface. A height past the corner shows discontinuities
                        // at the corners, so both are clamped before use.
                        val minCorner = minOf(
                            minOf(
                                corner.topStart.toPx(size, this),
                                corner.topEnd.toPx(size, this),
                            ),
                            minOf(
                                corner.bottomEnd.toPx(size, this),
                                corner.bottomStart.toPx(size, this),
                            ),
                        )
                        lens(
                            refractionHeight = glass.lensHeight.toPx().coerceAtMost(minCorner),
                            refractionAmount = glass.lensAmount.toPx().coerceAtMost(size.minDimension),
                        )
                    }
                }
            },
            // The library draws its own specular highlight and drop shadow by
            // default. Both are turned off because this surface already draws a
            // lit edge and a top highlight of its own, and the library's would
            // double them into something neither design asked for.
            highlight = null,
            shadow = null,
            // The surface proper, drawn *over* the sampled backdrop so text
            // stays legible. This is the library's own advice — you trade some
            // beauty for readability — and it is the same fill and highlight the
            // fallback uses, so the two paths look like one material.
            onDrawSurface = { drawGlassFill(glass) },
        )
        .glassBorder(shape, glass)
}

/**
 * The translucent fill and the specular highlight.
 *
 * Shared by the fallback path and the real one so the two are the same glass:
 * the fallback is the whole surface, and the real path draws this over the
 * sampled backdrop as its surface layer.
 */
private fun DrawScope.drawGlassFill(glass: DetourGlass) {
    // The fill. Always drawn: it is the surface itself.
    drawRect(color = glass.tint)
    // The specular highlight, a white vertical gradient across the top half,
    // which is the reflection of the room. Under the content, so text stays
    // legible.
    if (glass.highlightAlpha > 0f) {
        drawRect(
            brush = Brush.verticalGradient(
                colors = listOf(
                    Color.White.copy(alpha = glass.highlightAlpha),
                    Color.Transparent,
                ),
                startY = 0f,
                endY = size.height * 0.5f,
            ),
        )
    }
}

/**
 * The lit edge: a 1dp gradient border, bright at the top and nearly gone at the
 * bottom. Only on the glass path — a plain surface has no lit edge.
 */
private fun Modifier.glassBorder(shape: Shape, glass: DetourGlass): Modifier =
    if (glass.borderWidth > 0.dp) {
        border(width = glass.borderWidth, brush = specularBorderBrush(), shape = shape)
    } else {
        this
    }

/**
 * The border gradient: bright at the top, nearly gone at the bottom.
 *
 * The same white-on-both-ends brush reads on a dark and a light surface, because
 * it is a highlight rather than a colour.
 */
private fun specularBorderBrush(): Brush = Brush.verticalGradient(
    colors = listOf(
        Color.White.copy(alpha = 0.5f),
        Color.White.copy(alpha = 0.05f),
    ),
)

/**
 * A [Modifier.liquidGlass] surface with a shape and the ambient glass capability.
 *
 * The glass capability is read from [LocalDetourGlass] rather than passed, so a
 * caller cannot accidentally hard-code "on" and ship a low-end device a
 * translucent surface it cannot afford. The backdrop it samples comes from
 * [LocalLayerBackdrop], which the app body provides; when that is absent — a
 * preview, or a screen shown outside the app body — the surface degrades to the
 * tint-and-border fallback rather than drawing nothing.
 */
@Composable
fun LiquidGlassSurface(
    modifier: Modifier = Modifier,
    shape: Shape = RoundedCornerShape(28.dp),
    glass: DetourGlass = LocalDetourGlass.current,
    content: @Composable BoxScope.() -> Unit,
) {
    Box(modifier = modifier.liquidGlass(shape, glass), content = content)
}
