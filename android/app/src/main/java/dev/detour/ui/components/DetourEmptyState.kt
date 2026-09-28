package dev.detour.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The one "there is nothing here" state.
 *
 * Two pages had grown their own: the rule page drew a bare line of grey text and
 * the log page a second copy of it, and neither said anything about *why* the
 * screen was empty or what could change that. This gives the state a shape —
 * optional icon, a title, an explanation — so an empty screen reads as a
 * deliberate answer rather than a page that failed to load.
 *
 * **There is deliberately no action button.** The obvious next step ("go add a
 * source", "go connect") lives on another screen, so a button here would mean
 * this component reaching into navigation and `MainActivity`. That is a
 * different change with a different blast radius; until it is made on purpose,
 * the detail line names the step in words and the caller stays in charge.
 *
 * The icon is optional and its absence must look intentional: `icon = null`
 * simply omits the circle, it does not leave a gap, so a caller that has no
 * meaningful glyph does not have to invent one to keep the layout balanced.
 *
 * Every colour is a theme role. The circle is a `primaryContainer` tint with an
 * `onPrimaryContainer` glyph — the theme's own "quiet brand" pair — rather than a
 * hand-picked light grey, because the app renders over a user wallpaper and a
 * fixed grey would stop matching the moment the palette changed underneath it.
 */
@Composable
fun DetourEmptyState(
    title: String,
    detail: String? = null,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        if (icon != null) {
            Box(
                modifier = Modifier
                    .size(48.dp)
                    .clip(CircleShape)
                    .background(MaterialTheme.colorScheme.primaryContainer),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    icon,
                    // Decorative: the title right below says the same thing, so
                    // an announced icon would make a screen reader repeat it.
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onPrimaryContainer,
                )
            }
            Spacer(Modifier.height(16.dp))
        }
        Text(
            title,
            // Slightly heavier than body copy, so the title reads as the heading
            // of the state rather than as one more line of the explanation.
            style = MaterialTheme.typography.titleSmall,
            textAlign = TextAlign.Center,
        )
        if (detail != null) {
            Spacer(Modifier.height(4.dp))
            Text(
                detail,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
                // Opened up from the style's default. The detail is a sentence or
                // two of guidance, and at this size the default leading runs it
                // together into a block; the extra room is what makes it read as
                // prose instead of a paragraph-shaped smudge.
                lineHeight = 20.sp,
            )
        }
    }
}
