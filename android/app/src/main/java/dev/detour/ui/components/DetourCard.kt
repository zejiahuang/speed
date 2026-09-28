package dev.detour.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.CardColors
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ElevatedCard
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import dev.detour.ui.theme.DetourTheme

/** The four card treatments this app uses. */
enum class DetourCardStyle {
    /** A flat container. The default; used for grouped settings and stats. */
    Filled,

    /** A hairline outline. Used where a card sits on a same-coloured surface. */
    Outlined,

    /** A shadow. Used sparingly, for something that should read as lifted. */
    Elevated,

    /** The frosted surface. See [LiquidGlassSurface]; degrades to a plain tint. */
    Glass,
}

/**
 * The one card in the app.
 *
 * Before this there were two styles hand-rolled across four screens — a bare
 * `Card` with `surfaceContainerHigh` here, `surfaceContainer` there — which is
 * how the same visual element drifted between pages. Everything now goes through
 * this function, so changing "what a card looks like" is one edit.
 *
 * `onClick` is optional; when given, the ripple is clipped to [shape] so a
 * large-radius card does not leak a square ripple at its corners.
 */
@Composable
fun DetourCard(
    modifier: Modifier = Modifier,
    style: DetourCardStyle = DetourCardStyle.Filled,
    shape: Shape = CardDefaults.shape,
    colors: CardColors = CardDefaults.cardColors(),
    onClick: (() -> Unit)? = null,
    content: @Composable ColumnScope.() -> Unit,
) {
    val clickable = if (onClick != null) {
        modifier.clip(shape).clickable(onClick = onClick)
    } else {
        modifier
    }
    when (style) {
        DetourCardStyle.Filled -> Card(clickable, shape, colors, content = content)
        DetourCardStyle.Outlined -> OutlinedCard(clickable, shape, colors, content = content)
        DetourCardStyle.Elevated -> ElevatedCard(clickable, shape, colors, content = content)
        DetourCardStyle.Glass -> LiquidGlassSurface(modifier = clickable, shape = shape) {
            Column(content = content)
        }
    }
}

@Preview
@Composable
private fun DetourCardPreview() {
    DetourTheme {
        Column(modifier = Modifier.padding(12.dp)) {
            DetourCard { Text("Filled", Modifier.padding(16.dp)) }
            DetourCard(
                modifier = Modifier.padding(top = 12.dp),
                style = DetourCardStyle.Glass,
            ) {
                Text("Glass", Modifier.padding(16.dp))
            }
        }
    }
}
