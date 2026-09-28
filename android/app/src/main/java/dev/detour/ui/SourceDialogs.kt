package dev.detour.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import dev.detour.R
import dev.detour.core.RuleSource
import dev.detour.ui.components.DetourAlertDialog
import dev.detour.ui.components.DetourButton
import dev.detour.ui.components.DetourButtonVariant

/**
 * The "add a custom rule source" dialog.
 *
 * Shared by the rules screen and the settings screen. Both offer the action and
 * the fields, the validation and the URL rewrite are identical, so this is one
 * implementation rather than two that drift — the app has already paid for that
 * lesson once, in the settings rows that used to be duplicated per screen.
 *
 * The only input that matters is the URL. A name is optional and defaults to the
 * URL, because a source the user added is one they will recognise by its address
 * more reliably than by a label they typed once.
 */
@Composable
fun AddRuleSourceDialog(onDismiss: () -> Unit, onAdd: (RuleSource) -> Unit) {
    var name by remember { mutableStateOf("") }
    var url by remember { mutableStateOf("") }
    var invalid by remember { mutableStateOf(false) }

    DetourAlertDialog(
        onDismissRequest = onDismiss,
        title = stringResource(R.string.rules_add_source),
        text = {
            Column {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                    label = { Text(stringResource(R.string.rules_source_name)) },
                )
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = url,
                    onValueChange = {
                        url = it
                        invalid = false
                    },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                    isError = invalid,
                    label = { Text(stringResource(R.string.rules_source_url)) },
                    supportingText = if (invalid) {
                        { Text(stringResource(R.string.rules_source_invalid_url)) }
                    } else {
                        null
                    },
                )
            }
        },
        confirmButton = {
            DetourButton(
                onClick = {
                    // The rewrite happens here, not on the way in: a GitHub page
                    // URL is what a browser gives the user, and it names the same
                    // file as the raw URL only after the translation.
                    val normalized = RuleSource.normalizeUrl(url)
                    if (!normalized.startsWith("http://") && !normalized.startsWith("https://")) {
                        invalid = true
                    } else {
                        onAdd(
                            RuleSource(
                                id = "custom:$normalized",
                                label = name.trim().ifEmpty { normalized },
                                url = normalized,
                            ),
                        )
                    }
                },
            ) { Text(stringResource(R.string.rules_source_add_ok)) }
        },
        dismissButton = {
            DetourButton(onClick = onDismiss, variant = DetourButtonVariant.Text) {
                Text(stringResource(R.string.common_cancel))
            }
        },
    )
}
