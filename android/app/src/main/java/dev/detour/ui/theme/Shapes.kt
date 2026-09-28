package dev.detour.ui.theme

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Shapes
import androidx.compose.ui.unit.dp

/**
 * The corner scale, as the `cornerStyle` setting picks it.
 *
 * Before this file existed the setting was stored, shown and never read: the
 * theme handed `MaterialTheme` its own stock `Shapes`, so "圆角：圆润" moved a chip
 * and changed nothing else. `MaterialTheme(shapes = ...)` is the one place a
 * shape scale is consumed, so that is where the setting has to land.
 *
 * Three tiers rather than a slider because a continuous radius has no visual
 * reference points to aim at; these are the three that read as distinct.
 *
 * | setting  | feel            | anchor          |
 * |----------|-----------------|-----------------|
 * | `small`  | tight, technical | 4dp extraSmall  |
 * | `medium` | the default     | 8dp extraSmall  |
 * | `large`  | soft, expressive | 12dp extraSmall |
 *
 * These replace the stock scale outright rather than layering on the expressive
 * set, because the expressive shape set is only reachable through
 * `MaterialExpressiveTheme`, which is `internal` in material3 1.4.0 (see
 * `Theme.kt`). The values are chosen to be a touch rounder than stock, which is
 * the direction the expressive set moved in.
 */
fun detourShapes(cornerStyle: String): Shapes = when (cornerStyle) {
    "small" -> CompactShapes
    "large" -> RoundShapes
    else -> StandardShapes
}

private val CompactShapes = Shapes(
    extraSmall = RoundedCornerShape(4.dp),
    small = RoundedCornerShape(8.dp),
    medium = RoundedCornerShape(12.dp),
    large = RoundedCornerShape(16.dp),
    extraLarge = RoundedCornerShape(20.dp),
)

private val StandardShapes = Shapes(
    extraSmall = RoundedCornerShape(8.dp),
    small = RoundedCornerShape(12.dp),
    medium = RoundedCornerShape(16.dp),
    large = RoundedCornerShape(20.dp),
    extraLarge = RoundedCornerShape(28.dp),
)

private val RoundShapes = Shapes(
    extraSmall = RoundedCornerShape(12.dp),
    small = RoundedCornerShape(16.dp),
    medium = RoundedCornerShape(20.dp),
    large = RoundedCornerShape(28.dp),
    extraLarge = RoundedCornerShape(32.dp),
)
