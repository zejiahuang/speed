package dev.detour.ui.theme

import androidx.compose.material3.ColorScheme
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import kotlin.math.max
import kotlin.math.roundToInt

/**
 * 装饰背景的输入：用户的壁纸与它的遮罩强度。
 *
 * 这两个值由应用主体从 `Prefs` 读一次，然后沿 [LocalDetourBackground] 往下传，
 * 而不是让每个整屏页面自己读 —— 壁纸是解码后的 `ImageBitmap`，再读一次就是再解
 * 一次码；而且两个页面各自读，就可能读到不同的值。
 */
data class DetourBackgroundSpec(
    val wallpaper: ImageBitmap?,
    val scrimPercent: Int,
)

/**
 * 装饰背景的输入，供**不在 `Scaffold` 里的整屏页面**自绘同一张背景。
 *
 * 设置的分组页与关于页是应用主体的兄弟节点（它们盖住底栏），因此拿不到主体画在
 * 下面的那一层。把它们改成透明也不行：下面压着的是它们盖住的设置列表，不是壁纸。
 * 所以它们用同一个 [drawDetourBackground] 自己画一遍 —— 同一份代码，不是两份长得
 * 像的实现。
 */
val LocalDetourBackground = staticCompositionLocalOf { DetourBackgroundSpec(null, 0) }

/**
 * The decorative background: the user's photo, or the app's own soft brand
 * gradient. Shared by the app body and by the full-screen pages that sit outside
 * it, so there is exactly one definition of "what the background is".
 *
 * Opaque on both branches — the photo covers the box, and the no-photo branch
 * starts with a full-box gradient. That is what makes it usable as a page's own
 * background rather than a tint.
 */
internal fun DrawScope.drawDetourBackground(
    scheme: ColorScheme,
    spec: DetourBackgroundSpec,
) {
    val wallpaper = spec.wallpaper
    if (wallpaper != null) {
        // Centre-crop: scale the photo so it *covers* the box on both axes, then
        // centre it, so a photo of any aspect ratio fills the screen with no
        // letterboxing. The offset is negative on the axis that overflows, which
        // is what centres it; the canvas clips the overhang.
        val scale = max(size.width / wallpaper.width, size.height / wallpaper.height)
        val drawnWidth = (wallpaper.width * scale).roundToInt()
        val drawnHeight = (wallpaper.height * scale).roundToInt()
        drawImage(
            image = wallpaper,
            dstOffset = IntOffset(
                (size.width.toInt() - drawnWidth) / 2,
                (size.height.toInt() - drawnHeight) / 2,
            ),
            dstSize = IntSize(drawnWidth, drawnHeight),
        )

        // The scrim, tinted with `scheme.surface` and NOT with black. The text
        // drawn over this layer is `onSurface`, which is dark in light theme and
        // light in dark theme — so a scrim that is always dark would guarantee
        // contrast in one theme and destroy it in the other. Tinting with the
        // theme's own surface keeps text legible in both, and follows 主题色 for
        // free. The strength is the user's (0..100) because only they can see
        // how busy their photo is.
        drawRect(color = scheme.surface.copy(alpha = spec.scrimPercent.coerceIn(0, 100) / 100f))
        return
    }

    // No wallpaper: the app's own decorative layer, unchanged. The blobs are
    // this app's stand-in for "something for the glass to refract" and are
    // deliberately *not* drawn over a photo — two decorative layers on top of
    // each other is a fight, not a design.
    // The base. A two-stop gradient rather than a flat fill, so the light the
    // glass refracts has a direction to it.
    drawRect(
        brush = Brush.verticalGradient(
            colors = listOf(scheme.surface, scheme.surfaceContainer),
        ),
    )

    val width = size.width
    val height = size.height
    val minSide = size.minDimension

    // Three blobs in the brand hues, at fixed relative positions rather than
    // random ones: the layer is re-recorded often, and a random position would
    // shimmer.
    blob(scheme.primary.copy(alpha = 0.18f), Offset(width * 0.18f, height * 0.12f), minSide * 0.55f)
    blob(scheme.tertiary.copy(alpha = 0.14f), Offset(width * 0.88f, height * 0.32f), minSide * 0.45f)
    blob(scheme.primary.copy(alpha = 0.10f), Offset(width * 0.55f, height * 0.92f), minSide * 0.60f)
}

/**
 * One soft radial highlight, fading to transparent at its edge.
 *
 * The transparent outer stop is what makes it a light *blob* rather than a disc:
 * without it the circle would have a hard rim, which is the one thing a light
 * source never has.
 */
private fun DrawScope.blob(color: Color, center: Offset, radius: Float) {
    drawCircle(
        brush = Brush.radialGradient(
            colors = listOf(color, Color.Transparent),
            center = center,
            radius = radius,
        ),
        radius = radius,
        center = center,
    )
}
