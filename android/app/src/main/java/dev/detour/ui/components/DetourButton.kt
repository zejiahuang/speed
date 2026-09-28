package dev.detour.ui.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import dev.detour.ui.theme.DetourTheme

/** The four text-button emphases, from loudest to quietest. */
enum class DetourButtonVariant {
    Filled,
    Tonal,
    Outlined,
    Text,
}

/**
 * The one button.
 *
 * A single entry point over the four Material buttons, so that "the quiet
 * action" is one `variant` value rather than a screen's choice between
 * `TextButton` and `OutlinedButton` made from memory.
 */
@Composable
fun DetourButton(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    variant: DetourButtonVariant = DetourButtonVariant.Filled,
    content: @Composable RowScope.() -> Unit,
) {
    when (variant) {
        DetourButtonVariant.Filled -> Button(onClick, modifier, enabled, content = content)
        DetourButtonVariant.Tonal -> FilledTonalButton(onClick, modifier, enabled, content = content)
        DetourButtonVariant.Outlined -> OutlinedButton(onClick, modifier, enabled, content = content)
        DetourButtonVariant.Text -> TextButton(onClick, modifier, enabled, content = content)
    }
}

/**
 * The one icon button.
 *
 * Wrapped rather than left raw so that the whole app's icon-button touch target
 * and colour come from one place.
 */
@Composable
fun DetourIconButton(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    content: @Composable () -> Unit,
) {
    IconButton(onClick, modifier, enabled, content = content)
}

@Preview
@Composable
private fun DetourButtonPreview() {
    DetourTheme {
        Column(modifier = Modifier.padding(12.dp)) {
            DetourButton(onClick = {}) { Text("Filled") }
            DetourButton(
                onClick = {},
                modifier = Modifier.padding(top = 8.dp),
                variant = DetourButtonVariant.Tonal,
            ) { Text("Tonal") }
            DetourButton(
                onClick = {},
                modifier = Modifier.padding(top = 8.dp),
                variant = DetourButtonVariant.Text,
            ) { Text("Text") }
        }
    }
}
