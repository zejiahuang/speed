package dev.detour.ui.components

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import dev.detour.ui.Destination
import dev.detour.ui.theme.LocalDetourGlass
import dev.detour.ui.theme.LocalDetourMotion
import kotlin.math.roundToInt

// The bar's internal metrics, shared by the items and the indicator.
//
// They are file-level rather than locals in [FloatingGlassBar] because the
// indicator is drawn by the bar but has to match boxes that are built by
// [FloatingBarItem]: the pill's height is the icon box's height and its top is
// where the item's own padding ends. Two copies of "4.dp" that are only
// accidentally equal is exactly the drift that would misalign the pill by a few
// pixels and be blamed on the animation.
//
// The two 4dp paddings are separate names on purpose even though they are the
// same number: one is the item column's top padding (which positions the pill)
// and the other is the icon box's padding (which sizes it). A single constant
// would compile and would also let a future change to one silently move the
// other.
private val BarItemGap = 4.dp
private val BarItemVerticalPadding = 4.dp
private val BarIconBoxVerticalPadding = 4.dp
private val BarIconSize = 24.dp
private val BarIndicatorShape = RoundedCornerShape(16.dp)

/**
 * The floating navigation bar.
 *
 * **Drawn by hand, not with `ShortNavigationBar`.** The Material 3 Expressive
 * `ShortNavigationBar` was tried first and failed on device: only the selected
 * destination rendered, spanning the full width, while the other three were laid
 * out at zero width — which Compose then drops from the semantics tree, so they
 * could neither be seen nor tapped. The dumped hierarchy showed a single
 * `selected=true` node with the bounds of the whole bar.
 *
 * That is not something the caller can fix, and the bytecode says why:
 *
 * * `ShortNavigationBarItem`'s first parameter is `boolean`, not `RowScope` — it
 *   is **not** a `RowScope` extension, so there is no `Modifier.weight(1f)` the
 *   caller could supply to give the items width.
 * * `ShortNavigationBar`'s content is a plain composable lambda, not a
 *   `RowScope.() -> Unit`, so the content cannot lay itself out either.
 * * The bar's real layout lives behind `LocalShortNavigationBarOverride` /
 *   `ShortNavigationBarOverrideScope`, an internal hook with no public way in.
 *
 * So the bar is a `Row` with four equally weighted children. This is the least
 * interesting possible layout, which is the point: it is fully under our control
 * and does not depend on the implicit behaviour of an experimental component.
 *
 * The floating look is unchanged and was never the component's doing — it is the
 * 16dp/12dp outer margins, the 28dp radius, the `navigationBars` inset and the
 * `liquidGlass` surface. Only the four children inside it are new.
 *
 * Each child is a `Modifier.selectable` with `Role.Tab`, so it emits one
 * semantics node carrying `selected` and the label — which is what makes the
 * bar verifiable with `uiautomator dump`.
 *
 * **The selected pill is one shared indicator that slides, not four that
 * cross-fade.** Each item used to paint its own `secondaryContainer` background
 * and fade it in and out in place, so the "indicator" never moved — the owner
 * read that as a switch, not a selection. The pill is now a single `Box` behind
 * the row, positioned by an animated x-offset, and the four items draw on top of
 * it with no background of their own. The item's own `Box` keeps its padding
 * even though it no longer paints anything: the indicator is sized and offset
 * to match that box, so removing the padding would move the icon and leave the
 * pill behind.
 *
 * The motion is the `jelly` spring from `LocalDetourMotion` and not `spatial`.
 * `spatial` is the house spring for anything that moves and settles, and it
 * overshoots by about a pixel; `jelly` overshoots visibly and comes back, which
 * is the whole effect the owner asked for. Substituting `spatial` would not be a
 * weaker version of the animation — it would be the animation switched off.
 */
@Composable
fun FloatingGlassBar(
    destinations: List<Destination>,
    selected: Int,
    onSelect: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    val glass = LocalDetourGlass.current
    val motion = LocalDetourMotion.current
    val shape = RoundedCornerShape(28.dp)

    BoxWithConstraints(
        modifier = modifier
            .fillMaxWidth()
            // Floating margins. Outside the glass, so the surface sits inset
            // from the screen edge rather than being clipped by it.
            .padding(horizontal = 16.dp, vertical = 12.dp)
            .windowInsetsPadding(WindowInsets.navigationBars)
            .liquidGlass(shape, glass)
            // Inside the glass, so the items never touch the lit edge.
            .padding(horizontal = 8.dp, vertical = 4.dp),
    ) {
        // `BoxWithConstraints` is what lets the indicator be sized from the
        // real inner width rather than from a hard-coded pixel count. The four
        // items are equal-weight with a fixed gap, so one item's width is the
        // width left over after the gaps, divided four ways — and the distance
        // the pill travels per step is that width plus one gap.
        val itemWidth = (maxWidth - BarItemGap * (destinations.size - 1)) / destinations.size
        val density = LocalDensity.current
        val stepPx = with(density) { (itemWidth + BarItemGap).toPx() }

        val indicatorOffsetPx by animateFloatAsState(
            targetValue = stepPx * selected,
            animationSpec = motion.jelly,
            label = "bar-indicator-offset",
        )

        // The indicator. Drawn first so the row of items lands on top of it —
        // a `Box` paints its children in order, and the reverse order would put
        // the icons behind the pill.
        Box(
            modifier = Modifier
                .offset {
                    IntOffset(
                        x = indicatorOffsetPx.roundToInt(),
                        // The item's own top padding, because the pill tracks the
                        // icon box and not the whole column (which also holds the
                        // label below).
                        y = BarItemVerticalPadding.roundToPx(),
                    )
                }
                .size(
                    width = itemWidth,
                    // The icon box's height: its vertical padding twice, plus the
                    // icon.
                    height = BarIconSize + BarIconBoxVerticalPadding * 2,
                )
                .clip(BarIndicatorShape)
                // Fully opaque, unlike the old per-item fade: a shared pill that
                // slides is always present, and only its position carries the
                // selection. The colour still cross-fades per item below.
                .background(MaterialTheme.colorScheme.secondaryContainer),
        )

        Row(
            modifier = Modifier
                .fillMaxWidth()
                .selectableGroup(),
            horizontalArrangement = Arrangement.spacedBy(BarItemGap),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            destinations.forEachIndexed { index, destination ->
                FloatingBarItem(
                    destination = destination,
                    selected = selected == index,
                    onClick = { onSelect(index) },
                    // The weight is what the experimental component could not give
                    // us: without it the four items collapse into one.
                    modifier = Modifier.weight(1f),
                )
            }
        }
    }
}

/** One destination: an icon and a label, over the shared indicator. */
@Composable
private fun FloatingBarItem(
    destination: Destination,
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val motion = LocalDetourMotion.current
    // Drives the icon's colour crossfade and its slight swell. It deliberately
    // no longer drives any background: the pill is one shared node owned by the
    // bar, and an item that also animated its own fill would be a second
    // indicator sitting still while the real one slid past it.
    val progress by animateFloatAsState(
        targetValue = if (selected) 1f else 0f,
        animationSpec = motion.fastSpatial,
        label = "bar-item-progress",
    )
    val contentColor = lerp(
        MaterialTheme.colorScheme.onSurfaceVariant,
        MaterialTheme.colorScheme.onSecondaryContainer,
        progress,
    )

    Column(
        modifier = modifier
            .clip(RoundedCornerShape(20.dp))
            .selectable(selected = selected, role = Role.Tab, onClick = onClick)
            .padding(vertical = BarItemVerticalPadding),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        // No background here any more, and no clip either. The pill is the
        // shared, sliding indicator in [FloatingGlassBar]; painting one here as
        // well would stack a second, stationary pill under the icon and the two
        // would fight. The padding stays, because it is what gives the icon box
        // the size and position the shared indicator is drawn against — the
        // clip went with the background, since there is nothing left to round.
        Box(
            modifier = Modifier
                .padding(horizontal = 16.dp, vertical = BarIconBoxVerticalPadding),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = destination.icon,
                contentDescription = null,
                tint = contentColor,
                modifier = Modifier
                    .size(BarIconSize)
                    .scale(0.92f + 0.08f * progress),
            )
        }
        Text(
            text = stringResource(destination.labelRes),
            style = MaterialTheme.typography.labelMedium,
            color = contentColor,
            maxLines = 1,
        )
    }
}
