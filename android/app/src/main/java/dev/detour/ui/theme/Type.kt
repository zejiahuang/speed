package dev.detour.ui.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.LineHeightStyle
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.sp

/**
 * Material 3 Expressive typography.
 *
 * Two departures from the stock ramps, both for readability in this app rather
 * than for looks:
 *
 * * **Numbers are tabular.** The home screen shows a byte counter that changes
 *   several times a second, and proportional digits make it jitter sideways as
 *   the values change. `FontFeatureSetting("tnum")` pins every digit to the same
 *   advance.
 * * **Body line height is a touch looser than default.** Log lines and rule
 *   descriptions are dense and frequently contain hostnames, which have no
 *   spaces to break at; the extra leading keeps wrapped lines from reading as a
 *   single block.
 */
private val TabularNumbers = TextStyle(fontFeatureSettings = "tnum")

private val LooseLineHeight = LineHeightStyle(
    alignment = LineHeightStyle.Alignment.Center,
    trim = LineHeightStyle.Trim.None,
)

val DetourTypography = Typography().let { base ->
    base.copy(
        // The hero readout on the home screen. Larger than `displaySmall` because
        // it is read at arm's length while the phone sits on a desk.
        displayMedium = base.displayMedium.copy(
            fontFeatureSettings = "tnum",
            fontWeight = FontWeight.Medium,
        ),
        displaySmall = base.displaySmall.copy(fontFeatureSettings = "tnum"),
        headlineMedium = base.headlineMedium.copy(fontFeatureSettings = "tnum"),
        headlineSmall = base.headlineSmall.copy(fontFeatureSettings = "tnum"),
        titleLarge = base.titleLarge.copy(fontFeatureSettings = "tnum"),

        bodyLarge = base.bodyLarge.copy(lineHeightStyle = LooseLineHeight),
        bodyMedium = base.bodyMedium.copy(lineHeightStyle = LooseLineHeight),

        // Log lines. Monospace, because they are columns of hostnames, addresses
        // and counts that are compared vertically down the screen.
        bodySmall = base.bodySmall.copy(
            fontFamily = FontFamily.Monospace,
            lineHeight = 16.sp,
            fontFeatureSettings = "tnum",
        ),
        labelLarge = base.labelLarge.copy(fontFeatureSettings = "tnum"),
        labelMedium = base.labelMedium.copy(fontFeatureSettings = "tnum"),
        labelSmall = base.labelSmall.copy(fontFeatureSettings = "tnum"),
    )
}

/**
 * The typography, scaled by the user's font-size setting.
 *
 * **Every** size and line height is multiplied, not only `fontSize`: scaling the
 * glyphs while leaving the leading alone makes a wrapped log line overlap the one
 * below it, and this app has a screen that is nothing but log lines.
 *
 * `TextUnit.Unspecified` is passed through untouched — multiplying it produces an
 * invalid unit rather than a larger one, so a style that never named a line height
 * must not be scaled into one that does.
 */
fun detourTypography(fontScale: Float): Typography {
    if (fontScale == 1f) return DetourTypography
    val base = DetourTypography
    fun TextStyle.scaled(): TextStyle = copy(
        fontSize = fontSize.scaled(fontScale),
        lineHeight = lineHeight.scaled(fontScale),
    )
    return base.copy(
        displayLarge = base.displayLarge.scaled(),
        displayMedium = base.displayMedium.scaled(),
        displaySmall = base.displaySmall.scaled(),
        headlineLarge = base.headlineLarge.scaled(),
        headlineMedium = base.headlineMedium.scaled(),
        headlineSmall = base.headlineSmall.scaled(),
        titleLarge = base.titleLarge.scaled(),
        titleMedium = base.titleMedium.scaled(),
        titleSmall = base.titleSmall.scaled(),
        bodyLarge = base.bodyLarge.scaled(),
        bodyMedium = base.bodyMedium.scaled(),
        bodySmall = base.bodySmall.scaled(),
        labelLarge = base.labelLarge.scaled(),
        labelMedium = base.labelMedium.scaled(),
        labelSmall = base.labelSmall.scaled(),
    )
}

/** Multiply a specified size, and pass `Unspecified` through. */
private fun TextUnit.scaled(factor: Float): TextUnit =
    if (this == TextUnit.Unspecified) this else this * factor
