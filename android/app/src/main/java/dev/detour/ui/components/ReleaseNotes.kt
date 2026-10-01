package dev.detour.ui.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import dev.detour.R
import dev.detour.core.UpdateChecker

/**
 * The "a new version is available" dialog: version, release notes, two actions.
 *
 * **No glass here, and that is a hard constraint rather than a style choice.** A
 * `Dialog` is a separate window whose pixels do not come from the page, so
 * `LiquidGlassSurface` inside one has no backdrop to refract and degrades to a flat
 * tint — a surface that looks like glass while refracting nothing. That is exactly
 * how this project has always treated fake glass: fall back to a plain Material
 * surface rather than draw something that calls itself glass and is not. So this
 * goes through [DetourAlertDialog], which uses a plain Material surface.
 *
 * The body scrolls and has an upper bound: the length of a release body is not
 * under our control, and a dialog that pushes its buttons off the screen has no
 * buttons.
 *
 * The notes are Markdown from a GitHub release body and are **rendered**, not
 * flattened — see [MarkdownText] for what that replaced and why. This dialog and
 * the first-launch disclaimer share that one component on purpose: two renderers
 * would be two opinions about what `**` means, and they would drift the moment
 * either was touched.
 */
@Composable
fun UpdateAvailableDialog(
    release: UpdateChecker.Release,
    onDismiss: () -> Unit,
    onDownload: () -> Unit,
) {
    DetourAlertDialog(
        onDismissRequest = onDismiss,
        title = stringResource(R.string.update_dialog_title),
        text = {
            Column {
                Text(
                    stringResource(R.string.update_dialog_version, release.versionName),
                    style = MaterialTheme.typography.bodyLarge,
                )
                val notes = release.notes
                // The body can be empty (a publisher who wrote no notes), and then no
                // blank scroll area is drawn — the dialog is just the version and the
                // two buttons. An empty notes area only makes people think it failed
                // to load.
                if (!notes.isNullOrBlank()) {
                    Spacer(Modifier.height(12.dp))
                    MarkdownText(
                        markdown = notes,
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(max = 320.dp),
                    )
                }
            }
        },
        confirmButton = {
            DetourButton(onClick = onDownload) {
                Text(stringResource(R.string.about_update_download))
            }
        },
        dismissButton = {
            DetourButton(onClick = onDismiss, variant = DetourButtonVariant.Text) {
                Text(stringResource(R.string.update_dialog_later))
            }
        },
    )
}
