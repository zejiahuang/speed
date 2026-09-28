package dev.detour.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AssistChip
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import dev.detour.ui.theme.DetourTheme

/**
 * A filter chip, with the label passed as a string rather than a slot.
 *
 * Every call site in this app passed exactly `{ Text(stringResource(...)) }`,
 * so the slot was a degree of freedom nobody used and everybody had to write
 * out.
 *
 * **`maxLines = 1` and `softWrap = false` are a defect fix, not styling.** A
 * chip's label is one short token — 紫罗兰, 跟随壁纸 — and never a sentence, so
 * it should never wrap. Without these it does: when the chip is measured under
 * a width smaller than its label, `Text` breaks the label, and a CJK label
 * breaks *between every glyph*, which is how 紫罗兰 once rendered as three
 * stacked characters inside a stretched chip. `softWrap = false` is the part
 * that actually stops it — `maxLines = 1` alone still permits a break at a word
 * boundary, and CJK text offers one at every character, so `maxLines = 1` would
 * merely truncate the second line rather than refuse to make one.
 *
 * Belt and braces with the scrolling strip in `DetourChoiceRow`: that fixes the
 * known container, and this makes the chip safe in any container, including one
 * a future caller puts it in without thinking about width.
 */
@Composable
fun DetourFilterChip(
    selected: Boolean,
    onClick: () -> Unit,
    label: String,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
) {
    FilterChip(
        selected = selected,
        onClick = onClick,
        label = { Text(label, maxLines = 1, softWrap = false) },
        modifier = modifier,
        enabled = enabled,
    )
}

/**
 * An assist chip — a chip that reads as an action, not a toggle.
 *
 * Used on the home screen for the mode selector, which is the only place a chip
 * is a button.
 */
@Composable
fun DetourAssistChip(
    onClick: () -> Unit,
    label: String,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    trailingIcon: (@Composable () -> Unit)? = null,
) {
    AssistChip(
        onClick = onClick,
        label = { Text(label) },
        modifier = modifier,
        enabled = enabled,
        trailingIcon = trailingIcon,
    )
}

@Preview
@Composable
private fun DetourChipPreview() {
    DetourTheme {
        Row(
            modifier = Modifier.padding(12.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            DetourFilterChip(selected = true, onClick = {}, label = "选中")
            DetourFilterChip(selected = false, onClick = {}, label = "未选")
            DetourAssistChip(onClick = {}, label = "动作")
        }
    }
}
