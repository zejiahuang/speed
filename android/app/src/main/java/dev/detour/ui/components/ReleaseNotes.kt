package dev.detour.ui.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import dev.detour.R
import dev.detour.core.UpdateChecker

/**
 * One link-definition line, of the form `[0.1.0]: https://…`.
 *
 * A bracketed label at the start of the line, a colon, then a URL with no space in
 * it, and that is the whole line. The pattern matches the entire line rather than
 * discarding anything containing `]:`, because `]:` can also occur inside an
 * ordinary sentence; only a line whose whole shape is this counts as a definition.
 */
private val LINK_DEFINITION = Regex("^\\[[^\\]]+]:\\s+\\S+\\s*$")

/**
 * Flattens the very small amount of Markdown in a GitHub release body into
 * readable plain text.
 *
 * Four notations are handled: a leading `#` heading marker (marker dropped, text
 * kept), inline `**` and backtick delimiters (delimiters dropped, text kept),
 * link-definition lines of the form `[0.1.0]: https://…` (line dropped whole), and
 * runs of blank lines (collapsed to one). Everything else is kept verbatim.
 *
 * **Why not pull in a Markdown library for this.** What is wanted here is not
 * "render Markdown prettily" but "let a person read a release note", and a release
 * body only ever uses the notations listed above. Pulling in a parser plus a
 * renderer for those (most renderers drag in a stack of layout dependencies) buys
 * a whole grammar nobody here needs, a rendering behaviour that changes with the
 * library's version, and a dependency that has to be kept current — costs far
 * larger than the benefit. If a body ever contains a table or a nested list, that
 * is the moment to reconsider, and the question then will be "should tables be
 * supported" rather than "why was a library not adopted up front".
 *
 * A pure function — no Compose, no state — so it can be tested on its own and
 * computed once inside `remember`.
 */
fun releaseNotesPlainText(markdown: String): String {
    val kept = mutableListOf<String>()
    for (raw in markdown.lines()) {
        val line = raw.trimEnd()

        // Link-definition lines are dropped whole: Markdown uses them to register
        // the target of a reference link such as `[0.1.0]`, and they are not a
        // sentence anyone reads. Left in the body they are a line of URL that dilutes
        // the actual content. The `trimStart` before matching is because a definition
        // is occasionally indented, and leading whitespace should not let one slip
        // through.
        if (LINK_DEFINITION.matches(line.trimStart())) continue

        // A leading `#` is a heading marker and `**`/backticks are inline
        // emphasis/code markers; in both cases only the marker goes. Deleting by
        // character rather than parsing by grammar is safe here because in a release
        // body these markers essentially never appear anywhere they should not be
        // deleted from.
        var text = line.trimStart()
        while (text.startsWith("#")) text = text.removePrefix("#")
        text = text.trimStart().replace("**", "").replace("`", "").trimEnd()

        if (text.isEmpty()) {
            // Runs of blank lines collapse to one, and leading/trailing blanks are
            // dropped outright — otherwise the body would open with a gap.
            if (kept.isEmpty() || kept.last().isEmpty()) continue
            kept += ""
        } else {
            kept += text
        }
    }
    // Only a blank following a non-blank line was appended above, so at most one
    // trailing blank can remain; remove it.
    while (kept.isNotEmpty() && kept.last().isEmpty()) kept.removeAt(kept.size - 1)
    return kept.joinToString("\n")
}

/**
 * The release-notes body: plain text, scrollable.
 *
 * **The caller must give this a bounded height (`heightIn` / `height`); it is not
 * optional.** The component scrolls internally, and when its parent is itself a
 * vertical scroll container — which the whole About page is — the child receives an
 * infinite maximum height, and `verticalScroll` throws on an infinite constraint.
 * With an upper bound the overflow scrolls inside this component and the page does
 * not have to make room for a body of unpredictable length.
 *
 * The style is the theme's `bodyMedium` in `onSurfaceVariant`: this is explanatory
 * content and must not compete with the headings. The Markdown flattening runs once
 * inside `remember(notes)`, so no frame of recomposition re-parses it.
 */
@Composable
fun ReleaseNotesText(notes: String, modifier: Modifier = Modifier) {
    val plain = remember(notes) { releaseNotesPlainText(notes) }
    Text(
        text = plain,
        modifier = modifier.verticalScroll(rememberScrollState()),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

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
                    ReleaseNotesText(
                        notes = notes,
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
