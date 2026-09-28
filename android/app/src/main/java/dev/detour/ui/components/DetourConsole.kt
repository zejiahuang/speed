package dev.detour.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import dev.detour.R
import dev.detour.ui.theme.DetourTheme

/**
 * One executed command and what came back.
 *
 * [output] is the raw JSON, already pretty-printed by the caller; [isError] is
 * the caller's read of whether it carried an `error` key, kept as data so this
 * component can colour it without understanding the payload.
 */
data class ConsoleEntry(val line: String, val output: String, val isError: Boolean)

/**
 * A command console: a text field, and a bounded scrollback of what it ran.
 *
 * The row that lets the app be driven from the phone instead of only over adb.
 * It is deliberately dumb — it holds no state and reads no `Prefs`, like every
 * other component in this package — so the transcript survives the settings
 * list being rebuilt and re-filtered under it. The state lives in
 * `SettingsScreen`, hoisted above the row list for exactly that reason.
 *
 * **The fixed height of the scrollback is load-bearing, not cosmetic.** This
 * row renders inside a `LazyColumn` item, which measures its children with an
 * unbounded maximum height; a vertically scrollable child measured with an
 * infinite constraint throws
 * `IllegalStateException: Vertically scrollable component was measured with an
 * infinity maximum height constraints`. A literal `height` gives the scroll
 * container a finite bound to scroll within.
 */
@Composable
fun DetourConsoleRow(
    label: String,
    hint: String,
    input: String,
    onInputChange: (String) -> Unit,
    onSubmit: () -> Unit,
    onClear: () -> Unit,
    entries: List<ConsoleEntry>,
    running: Boolean,
    modifier: Modifier = Modifier,
) {
    Column(modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                label,
                modifier = Modifier.weight(1f),
                style = MaterialTheme.typography.bodyLarge,
            )
            // Only worth showing once there is something to clear.
            if (entries.isNotEmpty()) {
                TextButton(onClick = onClear) {
                    Text(stringResource(R.string.settings_console_clear))
                }
            }
        }
        Text(
            hint,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(8.dp))
        OutlinedTextField(
            value = input,
            onValueChange = onInputChange,
            modifier = Modifier.fillMaxWidth(),
            singleLine = true,
            placeholder = { Text(stringResource(R.string.settings_console_placeholder)) },
            // `bodySmall` is already monospace (see ui/theme/Type.kt) and goes
            // through `detourTypography(fontScale)`, so the console honours the
            // user's font-size setting. Setting `FontFamily.Monospace` here would
            // bypass that scaling.
            textStyle = MaterialTheme.typography.bodySmall,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
            keyboardActions = KeyboardActions(onSend = { onSubmit() }),
            trailingIcon = {
                // `Send` is in `material-icons-core`, unlike the three icons in
                // `ui/icons/DetourIcons.kt`; verified against the artifact rather
                // than assumed.
                IconButton(onClick = onSubmit) {
                    Icon(
                        Icons.AutoMirrored.Filled.Send,
                        contentDescription = stringResource(R.string.settings_console_run),
                    )
                }
            },
        )
        Spacer(Modifier.height(8.dp))

        val prompt = stringResource(R.string.settings_console_prompt)
        val scrollState = rememberScrollState()
        // Follow the newest entry. Keyed on the entry count, so appending a result
        // scrolls and merely recomposing does not.
        LaunchedEffect(entries.size) {
            scrollState.animateScrollTo(scrollState.maxValue)
        }
        Column(
            Modifier
                .fillMaxWidth()
                .height(180.dp)
                .clip(MaterialTheme.shapes.small)
                .background(MaterialTheme.colorScheme.surfaceVariant),
        ) {
            Column(
                Modifier
                    // `fillMaxWidth` so the scroll gesture is the whole box, not
                    // just a strip as wide as the longest line, and so a long
                    // `dump` line wraps at the box edge instead of being clipped.
                    .fillMaxWidth()
                    .verticalScroll(scrollState)
                    .padding(8.dp),
            ) {
                if (entries.isEmpty()) {
                    Text(
                        stringResource(R.string.settings_console_empty),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                entries.forEach { entry ->
                    Text(
                        "$prompt ${entry.line}",
                        style = MaterialTheme.typography.bodySmall,
                    )
                    Text(
                        entry.output,
                        style = MaterialTheme.typography.bodySmall,
                        color = if (entry.isError) {
                            MaterialTheme.colorScheme.error
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                }
                if (running) {
                    Text(
                        stringResource(R.string.settings_console_running),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Preview
@Composable
private fun DetourConsoleRowPreview() {
    DetourTheme {
        DetourConsoleRow(
            label = "内核命令行",
            hint = "与 adb 控制面同一套命令",
            input = "set mtu 1400",
            onInputChange = {},
            onSubmit = {},
            onClear = {},
            entries = listOf(
                ConsoleEntry("status", "{\n  \"phase\": \"off\"\n}", false),
                ConsoleEntry("set", "{\n  \"error\": \"set needs key\"\n}", true),
            ),
            running = false,
        )
    }
}
