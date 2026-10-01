package dev.detour.ui.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import dev.detour.R
import dev.detour.core.Disclaimer
import kotlinx.coroutines.delay

/**
 * How long the disclaimer must stay on screen before either button works.
 *
 * Ten seconds is not a reading time for a document this long — nobody reads it in
 * ten seconds. It is the point below which a tap cannot plausibly be a decision,
 * which is all a delay can honestly enforce: the alternative, waiting for the
 * user to scroll to the end, measures scrolling rather than reading and punishes
 * the person who genuinely did read it on an earlier device.
 */
const val DISCLAIMER_LOCK_SECONDS = 10

/**
 * The first-launch disclaimer: the full text, and two actions that stay disabled
 * for [DISCLAIMER_LOCK_SECONDS].
 *
 * ## Why neither button is available at once
 *
 * The dialog is shown once per install and gates the app, so the honest way to
 * slow it down is to slow down **both** exits. An "agree" that is live while
 * "decline" is not is a prompt, not a choice, and a decline that can be tapped
 * instantly is just a faster way past the text. Both open together.
 *
 * ## Why it cannot be dismissed by accident
 *
 * `dismissOnBackPress` and `dismissOnClickOutside` are both off, which is why
 * this dialog needs [DetourAlertDialog]'s `properties` slot at all. Every other
 * dialog in the app can be waved away because the worst case is that the user is
 * asked again; this one is the app's consent gate, and a gate that opens when you
 * brush the screen is not a gate. The empty `onDismissRequest` below is therefore
 * unreachable by construction rather than by policy.
 *
 * ## Why the countdown is `rememberSaveable`
 *
 * A rotation recreates the composition. With plain `remember` the countdown would
 * start over, and turning the phone would be a way to make the wait longer — the
 * opposite of what a lock is for. Saving it means the ten seconds are ten seconds
 * regardless of what the window does.
 *
 * ## Where the title comes from
 *
 * From the document, not from a string resource — see [Disclaimer.titleAndBody].
 * The same heading would otherwise be drawn twice, and a title maintained apart
 * from the text it titles is a title that drifts from it. The resource is only the
 * fallback for a document that opens with something other than a heading.
 *
 * ## Why the body is rendered rather than flattened
 *
 * [MarkdownText] renders the Markdown. The flattened version this replaced left
 * the document's markup on screen — `[LICENSE](LICENSE)` and `*which address*` —
 * because a flattener can only remove the notations it was taught. See that
 * component for the full reasoning.
 *
 * The body scrolls with an upper bound: the text is long and not under this
 * component's control, and a dialog that pushes its own buttons off the screen has
 * no buttons.
 */
@Composable
fun DisclaimerDialog(
    text: String,
    onAccept: () -> Unit,
    onDecline: () -> Unit,
) {
    var remaining by rememberSaveable { mutableIntStateOf(DISCLAIMER_LOCK_SECONDS) }
    // Keyed on `remaining` so each tick re-runs the effect with the new value;
    // the effect is cancelled and replaced rather than looping, which keeps the
    // countdown correct across a recomposition that happens mid-second.
    LaunchedEffect(remaining) {
        if (remaining > 0) {
            delay(1_000L)
            remaining -= 1
        }
    }
    val unlocked = remaining <= 0

    val (documentTitle, body) = remember(text) { Disclaimer.titleAndBody(text) }
    val fallbackTitle = stringResource(R.string.disclaimer_title)
    val title = if (documentTitle.isEmpty()) fallbackTitle else documentTitle

    DetourAlertDialog(
        // Never called: both dismissal routes are disabled in `properties`. Given
        // a body rather than left absent so the unreachability is visible here
        // instead of depending on a reader remembering the flags below.
        onDismissRequest = {},
        title = title,
        properties = DialogProperties(
            dismissOnBackPress = false,
            dismissOnClickOutside = false,
        ),
        text = {
            Column {
                Text(
                    text = stringResource(R.string.disclaimer_intro),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(12.dp))
                MarkdownText(
                    markdown = body,
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(max = 360.dp),
                )
            }
        },
        confirmButton = {
            DetourButton(onClick = onAccept, enabled = unlocked) {
                Text(
                    if (unlocked) {
                        stringResource(R.string.disclaimer_accept)
                    } else {
                        stringResource(R.string.disclaimer_accept_locked, remaining)
                    },
                )
            }
        },
        dismissButton = {
            DetourButton(
                onClick = onDecline,
                enabled = unlocked,
                variant = DetourButtonVariant.Text,
            ) {
                Text(stringResource(R.string.disclaimer_decline))
            }
        },
    )
}
