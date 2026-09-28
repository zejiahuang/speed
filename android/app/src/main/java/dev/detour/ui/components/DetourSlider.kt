package dev.detour.ui.components

import androidx.compose.material3.Slider
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.Preview
import dev.detour.ui.theme.DetourTheme

/**
 * The one slider.
 *
 * The wrapper exists to fix the *contract*, not the pixels: `onValueChange` is
 * the live drag and `onValueChangeFinished` is the commit, and every caller in
 * this app wants to persist on the commit. Naming both here means a screen
 * cannot accidentally write to storage on every frame of a drag.
 */
@Composable
fun DetourSlider(
    value: Float,
    onValueChange: (Float) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    valueRange: ClosedFloatingPointRange<Float> = 0f..1f,
    steps: Int = 0,
    onValueChangeFinished: (() -> Unit)? = null,
) {
    Slider(
        value = value,
        onValueChange = onValueChange,
        modifier = modifier,
        enabled = enabled,
        valueRange = valueRange,
        steps = steps,
        onValueChangeFinished = onValueChangeFinished,
    )
}

@Preview
@Composable
private fun DetourSliderPreview() {
    DetourTheme {
        DetourSlider(value = 0.5f, onValueChange = {})
    }
}
