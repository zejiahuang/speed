package dev.detour.ui.components

import androidx.compose.runtime.compositionLocalOf
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * How much room a scrolling screen must leave at its bottom so the floating bar
 * does not sit on top of the last row.
 *
 * **Why this exists at all.** The bar used to live in the `Scaffold`'s
 * `bottomBar` slot, and `Scaffold` therefore handed every screen a bottom inset
 * that already cleared it. That is also exactly why the bar could only ever
 * refract the wallpaper: the content was laid out *above* the bar, so there was
 * nothing of the content under it to sample. Letting content scroll under the
 * bar — which is what the owner asked for — means giving up that free inset,
 * and the clearance then has to come from the screen that is doing the
 * scrolling.
 *
 * **Why a composition local rather than a parameter.** Each screen owns its own
 * scroll container (`LazyColumn` / `verticalScroll`), and `contentPadding` is a
 * property of *that* container, so the value has to reach four separate files.
 * Threading it as a parameter would mean four call sites and four chances to
 * forget one; the local makes the correct value the default reading, and a
 * screen shown outside the app body (a `@Preview`) simply gets zero.
 *
 * **Why `compositionLocalOf` and not the `staticCompositionLocalOf` the other
 * tokens use.** The height is measured, not declared — it is the bar's own
 * laid-out size plus the navigation-bar inset, which differs per device. So it
 * starts at zero and settles on the first frame, and every consumer would
 * recompose once on that change. `staticCompositionLocalOf` recomposes the
 * entire subtree beneath it; this one recomposes only the readers. The
 * distinction does not matter for a token that never changes and does matter
 * for a value that changes on the first layout.
 *
 * Zero is the honest default: a caller that has not provided it has not drawn a
 * bar over the content either, so there is nothing to clear.
 */
val LocalBottomBarClearance = compositionLocalOf { 0.dp }

/**
 * The clearance to add to a scroll container's bottom `contentPadding`.
 *
 * A one-liner, but it is the only place the "existing padding plus clearance"
 * arithmetic is written, so the four screens cannot each invent their own
 * version of it and drift.
 */
fun Dp.plusBarClearance(clearance: Dp): Dp = this + clearance
