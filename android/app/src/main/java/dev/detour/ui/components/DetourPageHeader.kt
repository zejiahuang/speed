package dev.detour.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

/**
 * The bar every full page opens with: a title, an optional second line, and the
 * row of icon actions that belong to the page as a whole.
 *
 * This exists because the rule page and the log page each hand-rolled the same
 * header and the two had already drifted — one put its title straight in the
 * row, the other wrapped it in a column, so a change to the type scale would
 * have moved one page and not the other. Everything now goes through this
 * function, so "what a page title looks like" is one edit.
 *
 * **The padding is load-bearing and must not be "tidied".** `start = 20.dp` is
 * the same gutter the search field, the source chips and the list below use, and
 * `top = 12.dp` is the value the rule page (`RulesScreen`) and the log page
 * (`LogsScreen`) both shipped with. This is a refactor of an existing header, not
 * a redesign: changing either number slides the title relative to the content
 * under it on two screens at once, which is precisely the drift the component
 * was extracted to stop.
 *
 * `actions` is a [RowScope] lambda rather than a list of `IconButton`s because
 * the caller owns what the buttons *do* — the header only owns where they sit.
 * It is arranged with a small gap, and it is not `fillMaxWidth`: the buttons
 * stay a natural width on the right while the title takes what is left.
 *
 * Colours come from the theme and nowhere else. The app draws its own wallpaper
 * behind a frosted surface, so a header that named a grey would be reading a
 * background that is not there — see the note on the log row in `LogsScreen`,
 * where the same rule is spelled out for the densest list in the app.
 */
@Composable
fun DetourPageHeader(
    title: String,
    subtitle: String? = null,
    modifier: Modifier = Modifier,
    actions: @Composable RowScope.() -> Unit = {},
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .padding(start = 20.dp, end = 8.dp, top = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // weight(1f), so a long title pushes the actions to the edge instead of
        // being pushed off it — and so the actions never wrap under the title.
        Column(Modifier.weight(1f)) {
            Text(
                title,
                style = MaterialTheme.typography.headlineSmall,
            )
            if (subtitle != null) {
                // `labelMedium`, matching the primary line the rule page already
                // showed here. The subtitle is a caption, not body copy: it must
                // sit clearly under the title without competing with it.
                Text(
                    subtitle,
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) { actions() }
    }
}
