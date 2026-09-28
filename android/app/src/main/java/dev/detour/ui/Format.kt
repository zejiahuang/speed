package dev.detour.ui

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import dev.detour.R

/**
 * The number and age formatters the screens share.
 *
 * These used to live at the bottom of `HomeScreen.kt`, which was fine while the
 * home screen was the only thing that printed a byte count. It is not any more:
 * the rules screen now prints an age per address and the home screen prints the
 * age of the rule cache, and three copies of "how do I say 3 分钟前" is how two
 * screens come to disagree about how to say it.
 *
 * [formatAge] is `@Composable` because the units are Chinese text and therefore
 * live in `strings.xml` — a pure function returning a hard-coded "分钟" would be
 * the one place in the app that could not be translated.
 */

/** Bytes as a person reads them: one decimal, unit always present. */
internal fun formatBytes(bytes: Long): String {
    val units = listOf("B", "KB", "MB", "GB", "TB")
    var value = bytes.toDouble()
    var unit = 0
    while (value >= 1024 && unit < units.lastIndex) {
        value /= 1024
        unit++
    }
    return if (unit == 0) "$bytes B" else String.format("%.1f %s", value, units[unit])
}

internal fun formatRate(bytesPerSecond: Long): String =
    if (bytesPerSecond <= 0) "0 B/s" else "${formatBytes(bytesPerSecond)}/s"

/**
 * How long ago [atMillis] was, in the coarsest unit that still says something.
 *
 * Coarse on purpose. The two callers ask "is this fresh" and "was this address
 * judged recently", and neither question is answered better by "2 小时 14 分钟前"
 * than by "2 小时前" — while the longer form is a lot harder to read in a row
 * that also has to hold an address and a switch.
 */
@Composable
internal fun formatAge(atMillis: Long, now: Long = System.currentTimeMillis()): String {
    val seconds = ((now - atMillis).coerceAtLeast(0L)) / 1000
    return when {
        seconds < 5 -> stringResource(R.string.age_just_now)
        seconds < 60 -> stringResource(R.string.age_seconds, seconds.toInt())
        seconds < 3_600 -> stringResource(R.string.age_minutes, (seconds / 60).toInt())
        seconds < 86_400 -> stringResource(R.string.age_hours, (seconds / 3_600).toInt())
        else -> stringResource(R.string.age_days, (seconds / 86_400).toInt())
    }
}
