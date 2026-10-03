package dev.detour.ui.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import dev.detour.R

/**
 * The battery-optimization reminder.
 *
 * ## Why it has no dismiss button
 *
 * This reminder is meant to come back, so it is not given a control that would read
 * as "and stop telling me". The window is still closable — [DetourAlertDialog]
 * passes Material's default `DialogProperties`, so the back gesture and a tap
 * outside both close it — and that is precisely the line between a reminder and a
 * gate. `DisclaimerDialog` turns both of those routes *off* because it must be
 * answered; this one must not, because the app is fully usable without the
 * exemption and blocking it would be trading a working app for a setting. What is
 * closed here is closed for this launch only: nothing is written down, so the next
 * launch asks again — see [dev.detour.core.BatteryPolicy] for why no flag is
 * stored rather than an omission of one.
 *
 * ## Why the body says what it says
 *
 * The first paragraph names the symptom rather than the setting, because "省电策略"
 * on its own is a name the user has never had to learn: what they can recognise is
 * a connection that reports itself up while nothing loads. The second paragraph
 * exists to stop the window from over-promising — only the exemption is testable,
 * so the OEM-only switches are handed to the user instead of being silently
 * implied to be covered.
 */
@Composable
fun BatteryPolicyDialog(
    onOpenSettings: () -> Unit,
    onDismiss: () -> Unit,
) {
    DetourAlertDialog(
        onDismissRequest = onDismiss,
        title = stringResource(R.string.battery_title),
        text = {
            Column {
                Text(
                    text = stringResource(R.string.battery_body),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(12.dp))
                Text(
                    text = stringResource(R.string.battery_oem_hint),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
        confirmButton = {
            DetourButton(onClick = onOpenSettings) {
                Text(stringResource(R.string.battery_open_settings))
            }
        },
    )
}
