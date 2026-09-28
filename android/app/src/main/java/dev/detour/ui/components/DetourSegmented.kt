package dev.detour.ui.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import dev.detour.ui.theme.DetourTheme

/**
 * A row of segmented buttons, one selected.
 *
 * The segmented-button counterpart to [DetourChoiceRow] for the two- or
 * three-option case. A segmented button and a chip row are not interchangeable:
 * a chip row reads as "filter this list", a segmented button reads as "choose
 * one of these few", and the settings screen has both kinds of question. Four or
 * more options no longer fit a segmented button, so those stay on the chip row.
 *
 * The label and the surrounding padding match [DetourChoiceRow] exactly, so a
 * call site can move between the two by changing one function name.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DetourSegmentedRow(
    label: String,
    options: List<Pair<String, Int>>,
    selected: String,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
        Text(label, style = MaterialTheme.typography.bodyLarge)
        Spacer(Modifier.height(8.dp))
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
            options.forEachIndexed { index, (value, labelRes) ->
                SegmentedButton(
                    selected = selected == value,
                    onClick = { onSelect(value) },
                    shape = SegmentedButtonDefaults.itemShape(index = index, count = options.size),
                    label = { Text(stringResource(labelRes)) },
                )
            }
        }
    }
}

@Preview
@Composable
private fun DetourSegmentedPreview() {
    DetourTheme {
        DetourSegmentedRow(
            label = "分段",
            options = listOf("a" to android.R.string.ok, "b" to android.R.string.cancel),
            selected = "a",
            onSelect = {},
        )
    }
}
