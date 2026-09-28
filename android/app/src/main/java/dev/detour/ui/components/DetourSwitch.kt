package dev.detour.ui.components

import androidx.compose.material3.Switch
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.Preview
import dev.detour.ui.theme.DetourTheme

/**
 * The one switch.
 *
 * A thin wrapper, and the point is that there is exactly one: every toggle in
 * the app gets the same thumb and track animation, and the day the switch needs
 * to change it changes in one place. It also means a screen cannot reach for a
 * raw `Switch` and quietly diverge from the rest.
 */
@Composable
fun DetourSwitch(
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
) {
    Switch(
        checked = checked,
        onCheckedChange = onCheckedChange,
        modifier = modifier,
        enabled = enabled,
    )
}

@Preview
@Composable
private fun DetourSwitchPreview() {
    DetourTheme {
        DetourSwitch(checked = true, onCheckedChange = {})
    }
}
