package dev.detour.ui

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Settings
import androidx.compose.ui.graphics.vector.ImageVector
import dev.detour.R
import dev.detour.ui.icons.DetourIcons

/**
 * The four destinations behind the floating bar.
 *
 * Public and in its own file because the bar that renders it lives in
 * `ui/components/` and cannot see a private enum in `MainActivity`. The enum is
 * the contract between the bar and the shell: the bar draws whatever it is
 * given, and the shell decides which screen a selection maps to.
 *
 * The icons are picked so that no two tabs read the same way. Two of them used
 * to say the wrong thing: `LOGS` drew `Icons.Filled.Info` (ⓘ), which is the
 * glyph for "about this app", and `HOME` drew the very same `PlayArrow` as the
 * home screen's power button — so the tab and the button looked identical while
 * meaning different things. Logs now draws [DetourIcons.Subject] and home the
 * ordinary house. `Subject` rather than a second `List` is the point: the rules
 * tab already draws a list, and two tabs that both read as "a list" is exactly
 * the ambiguity this choice removes.
 */
enum class Destination(val labelRes: Int, val icon: ImageVector) {
    HOME(R.string.nav_home, Icons.Filled.Home),
    RULES(R.string.nav_rules, Icons.AutoMirrored.Filled.List),
    LOGS(R.string.nav_logs, DetourIcons.Subject),
    SETTINGS(R.string.nav_settings, Icons.Filled.Settings),
}
