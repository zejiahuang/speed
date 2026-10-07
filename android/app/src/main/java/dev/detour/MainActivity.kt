package dev.detour

import android.Manifest
import android.app.Activity
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LifecycleResumeEffect
import com.kyant.backdrop.backdrops.layerBackdrop
import com.kyant.backdrop.backdrops.rememberLayerBackdrop
import dev.detour.core.BatteryPolicy
import dev.detour.core.DetourVpnService
import dev.detour.core.Disclaimer
import dev.detour.core.KernelState
import dev.detour.core.Prefs
import dev.detour.core.Rate
import dev.detour.core.UpdateState
import dev.detour.core.WallpaperStore
import dev.detour.ui.AboutScreen
import dev.detour.ui.Destination
import dev.detour.ui.HomeScreen
import dev.detour.ui.LogsScreen
import dev.detour.ui.RulesScreen
import dev.detour.ui.SettingsPage
import dev.detour.ui.SettingsScreen
import dev.detour.ui.components.BatteryPolicyDialog
import dev.detour.ui.components.DisclaimerDialog
import dev.detour.ui.components.FloatingGlassBar
import dev.detour.ui.components.LocalBottomBarClearance
import dev.detour.ui.components.LocalLayerBackdrop
import dev.detour.ui.components.UpdateAvailableDialog
import dev.detour.ui.openInBrowser
import dev.detour.ui.theme.DetourTheme
import dev.detour.ui.theme.LocalDetourMotion
import kotlin.math.max
import kotlin.math.roundToInt

/**
 * The whole app: four destinations behind a floating navigation bar.
 *
 * No navigation library. There are four screens with no arguments, no deep links
 * and no back stack worth restoring — a `when` on an index is smaller than the
 * dependency and does the same job. `rememberSaveable` is what keeps the tab
 * across a rotation.
 */
class MainActivity : ComponentActivity() {

    /**
     * `VpnService.prepare` is the consent dialog, and it has to run from an
     * Activity. The result decides whether the service is asked to start.
     */
    private val requestVpn = registerForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) { result ->
        if (result.resultCode == RESULT_OK) {
            startTunnel(KernelState.Mode.VPN)
        } else {
            KernelState.log(
                KernelState.LogEntry.Level.WARN, "MainActivity", "用户拒绝了 VPN 授权",
            )
        }
    }

    private val requestNotifications = registerForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (!granted) {
            KernelState.log(
                KernelState.LogEntry.Level.WARN, "MainActivity",
                "没有通知权限，前台服务可能被系统限制",
            )
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()

        // The tunnel is driven through the holder rather than by binding, so the
        // screen does not need to know whether the service is up.
        KernelState.onConnect = { _, mode -> connect(mode) }
        KernelState.onDisconnect = {
            startService(DetourVpnService.stopIntent(this))
            Rate.clear()
        }

        // `askForNotifications` is **not** called here, and the reason is measured
        // rather than theoretical. On Android 13+ this Activity's `onCreate` runs a
        // few frames before the first composition, so requesting here put the
        // system's permission window on top of the first-launch disclaimer — a
        // `dumpsys window` during a fresh install showed
        // `GrantPermissionsActivity` focused and `dev.detour.MainActivity` behind
        // it, and `uiautomator` saw only the permission dialog. The consequence was
        // not cosmetic: the disclaimer's countdown had already started, so those
        // ten seconds ran down while the user was looking at a different window and
        // the notice could be dismissed on the first frame it was ever visible. It
        // is requested from the composition instead, once the gate is answered —
        // see `DetourAppBody`.

        setContent {
            val prefs = Prefs.of(this)
            DetourTheme(
                darkTheme = when (prefs.darkMode) {
                    "always" -> true
                    "never" -> false
                    else -> androidx.compose.foundation.isSystemInDarkTheme()
                },
                dynamicColor = prefs.dynamicColor,
                // All three of these only reach the UI through the theme — a
                // palette, a type scale and a shape scale — so they are passed
                // here rather than applied by a screen afterwards. They are read
                // as Compose state, so changing one in the settings screen
                // recomposes the theme and the whole app with it. The glass is
                // deliberately not among them any more: its inputs are two
                // switches and five numbers, and `DetourTheme` reads them from
                // `Prefs` itself rather than taking eight arguments.
                themeColor = prefs.themeColor,
                fontScale = prefs.fontScale,
                cornerStyle = prefs.cornerStyle,
            ) {
                DetourAppBody(onRequestNotifications = ::askForNotifications)
            }
        }
    }

    private fun connect(mode: KernelState.Mode) {
        when (mode) {
            KernelState.Mode.VPN -> {
                // `prepare` returns null when consent has already been given, and
                // an Intent when the system wants to ask. Both outcomes are
                // logged: a null here on a fresh install is the difference between
                // "the dialog is coming" and "the dialog is never coming", and
                // from the outside those look identical.
                val prepare = runCatching { VpnService.prepare(this) }
                    .onFailure {
                        KernelState.log(
                            KernelState.LogEntry.Level.ERROR, "MainActivity",
                            "VpnService.prepare 抛出异常：${it.message}",
                        )
                    }
                    .getOrNull()

                KernelState.log(
                    KernelState.LogEntry.Level.INFO, "MainActivity",
                    if (prepare == null) "VPN 已有授权，直接启动" else "需要授权，弹出系统对话框",
                )

                if (prepare != null) {
                    requestVpn.launch(prepare)
                } else {
                    startTunnel(KernelState.Mode.VPN)
                }
            }
            // No consent dialog: the proxy creates no tunnel and captures no
            // traffic the app did not send it.
            KernelState.Mode.PROXY -> startTunnel(KernelState.Mode.PROXY)
            // Nor here. Root mode asks the *root manager* for its privilege, and
            // that prompt belongs to `su`, not to this app — there is nothing for
            // `VpnService.prepare` to prepare.
            KernelState.Mode.ROOT -> startTunnel(KernelState.Mode.ROOT)
        }
    }

    private fun startTunnel(mode: KernelState.Mode) {
        ContextCompat.startForegroundService(this, DetourVpnService.startIntent(this, mode))
    }

    private fun askForNotifications() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS)
            == android.content.pm.PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
    }
}

/**
 * The app's content, and the one place that owns the first-launch consent gate.
 *
 * [onRequestNotifications] is a parameter rather than a call to a helper because
 * asking for a runtime permission needs an Activity, and the point of the
 * parameter is *when* it is called — see the `LaunchedEffect` below. It defaults
 * to doing nothing so the `@Preview` above can render this without an Activity to
 * ask from.
 */
@Composable
private fun DetourAppBody(onRequestNotifications: () -> Unit = {}) {
    var selected by rememberSaveable { mutableIntStateOf(0) }
    // Whether 关于 is on top of everything. A boolean rather than a fifth
    // `Destination`, and `rememberSaveable` for the same reason `selected` uses
    // it: the page survives a rotation instead of snapping back to the settings
    // list. Why it is not a destination is explained where the overlay is drawn.
    var showAbout by rememberSaveable { mutableStateOf(false) }
    // Which settings group is on top, or null for the settings list itself.
    //
    // A nullable page rather than five booleans, and the same shape as 关于 above
    // rather than a `Destination`: these pages are reached from 设置 and return
    // there, which is a different kind of thing from a tab — see the note where
    // they are drawn. `rememberSaveable` so a rotation does not throw the reader
    // back to the list with the page they were reading lost.
    var settingsPage by rememberSaveable { mutableStateOf<SettingsPage?>(null) }
    val destinations = Destination.entries
    val scheme = MaterialTheme.colorScheme
    val context = LocalContext.current
    val prefs = Prefs.of(context)

    // The first-launch consent gate.
    //
    // The text is read once per `Context` and held rather than read on each
    // recomposition: it is a ~10 KB raw resource and this body recomposes on
    // every state change in the app, so re-reading it would be steady work with
    // no result. `remember(context)` rather than a bare `remember` so that a
    // configuration change re-reads through the new context.
    //
    // **Acceptance is a property of the text, not a flag.** `Disclaimer` stores
    // the digest of what was agreed to, so editing `DISCLAIMER.md` re-asks — see
    // that object for why a stored boolean would be wrong. The read of
    // `prefs.disclaimerAcceptedDigest` inside `isAccepted` happens during
    // composition, so accepting recomposes this body and the dialog leaves on its
    // own; there is no local "dismissed" state to keep in step with the record.
    //
    // `null` text means the build never ran `syncDisclaimer`; `isAccepted` reports
    // that as accepted, so a packaging defect shows no dialog rather than a dialog
    // with an empty body. `DetourApp` logs it.
    val disclaimerText = remember(context) { Disclaimer.text(context) }
    val disclaimerAccepted = Disclaimer.isAccepted(disclaimerText, prefs)

    // The notification permission, asked for only once the gate is answered.
    //
    // **Why this is here and not in `onCreate`.** Measured on a fresh install:
    // requesting from `onCreate` put the system's permission window on top of the
    // disclaimer, and because the disclaimer's countdown starts when it is
    // composed, the ten seconds expired behind a window the user was not looking
    // at. Keying on `disclaimerAccepted` makes the request fire either immediately
    // (an install that has already agreed — same timing as before) or the moment
    // the user agrees, which is the earliest point at which the notice has been
    // read. `LaunchedEffect` and not a plain call in composition: this launches a
    // system Activity, which is a side effect and must not run during composition.
    LaunchedEffect(disclaimerAccepted) {
        if (disclaimerAccepted) onRequestNotifications()
    }

    // The battery-optimization reminder.
    //
    // **Re-checked on every resume, not only at composition.** The action this
    // dialog offers leaves the app for the system's settings list, and coming back
    // changes nothing in this composition — so a one-shot effect would leave the
    // reminder on screen over an app the user has just fixed. Resume is the first
    // moment the answer is knowable, which is why the check is bound to it.
    //
    // **Nothing is recorded.** There is no "asked" flag and no "ignored" flag in
    // `Prefs`, and that is the requirement rather than a gap: the exemption can be
    // revoked — by the user, by a ROM cleanup tool, or by the system after an
    // update — so a stored "we already told them" would go on being true after the
    // condition it described had become false again. Not storing it costs one
    // binder read per resume. See `BatteryPolicy`.
    //
    // `dismissed` is `rememberSaveable` so that rotating the phone does not bring
    // the reminder back, and is not persisted, so the next launch shows it again.
    //
    // `logged` is nullable rather than a `Boolean` so that "not yet logged" stays
    // distinguishable from "logged as restricted"; the WARN then appears on the
    // first resume and on every change, instead of on every resume.
    var batteryUnrestricted by remember { mutableStateOf(BatteryPolicy.isUnrestricted(context)) }
    var batteryDismissed by rememberSaveable { mutableStateOf(false) }
    var batteryLogged by remember { mutableStateOf<Boolean?>(null) }
    LifecycleResumeEffect(Unit) {
        val unrestricted = BatteryPolicy.isUnrestricted(context)
        batteryUnrestricted = unrestricted
        if (batteryLogged != unrestricted) {
            batteryLogged = unrestricted
            KernelState.log(
                if (unrestricted) {
                    KernelState.LogEntry.Level.INFO
                } else {
                    KernelState.LogEntry.Level.WARN
                },
                "MainActivity",
                if (unrestricted) {
                    "省电策略：已设为无限制"
                } else {
                    "省电策略：未设为无限制，系统可能回收隧道"
                },
            )
        }
        onPauseOrDispose { }
    }

    // The wallpaper is decoded off the main thread and re-read whenever the pref's
    // identity changes, so picking a photo lands on the next frame rather than on
    // the next launch. A blank pref is not "decode nothing" — it is the signal to
    // stop holding a bitmap, so the previous one is dropped rather than left
    // resident on a device with 2 GB of RAM.
    var wallpaper by remember { mutableStateOf<ImageBitmap?>(null) }
    LaunchedEffect(prefs.wallpaper) {
        wallpaper = if (prefs.wallpaper.isBlank()) null else WallpaperStore.load(context)
    }
    // Read in composition, not inside the draw lambda, so the modifier updates when
    // the scrim moves and the draw phase never reads snapshot state itself.
    val scrim = prefs.wallpaperScrim

    // The app's backdrop: everything the glass refracts. Its default `onDraw`
    // records the marked node's own drawing into a graphics layer, so the
    // decorative layer below is at once what you see and what the glass samples.
    val backdrop = rememberLayerBackdrop()

    // The bar's own backdrop, and the reason there are two of them.
    //
    // The bar has to refract the *content* — that is the whole complaint being
    // fixed here — but it must never refract itself. A `layerBackdrop` marker
    // that contained the bar would record the bar's own previous frame into the
    // layer the bar then samples, and every drag of the content would smear the
    // bar across itself. So the content is recorded into a second layer that the
    // bar does not belong to: `barBackdrop`'s marked node wraps the decorative
    // layer and the content, and the bar is its *sibling*, drawn after it.
    //
    // The content's own glass cards keep sampling `backdrop`, which is unchanged
    // — pointing them at `barBackdrop` instead would be the same self-feedback
    // problem one level down, because the content is inside that marker too.
    val barBackdrop = rememberLayerBackdrop()

    // The bar's measured height, published to the screens so their scroll
    // containers can leave room for it (see [LocalBottomBarClearance]).
    //
    // It starts at zero and settles on the bar's first layout. Zero is correct
    // for that one frame: before the bar has been measured there is nothing
    // drawn over the content to clear, so padding the content by anything would
    // be guessing. Measured rather than hard-coded because the height is the
    // bar's own size *plus* the navigation-bar inset, and both differ per device
    // and per font scale — a literal 84.dp would be wrong on the next phone.
    val density = LocalDensity.current
    var barHeight by remember { mutableStateOf(0.dp) }

    Box(Modifier.fillMaxSize()) {
        // `barBackdrop`'s marked node: the decorative layer *and* the content,
        // which is exactly what the bar should see behind it. The bar is
        // deliberately left out — see `barBackdrop` above.
        Box(
            modifier = Modifier
                .fillMaxSize()
                .layerBackdrop(barBackdrop),
        ) {
            // The decorative background, still marked with the content-only
            // `backdrop` so the content's glass keeps refracting the wallpaper
            // and the brand blobs rather than the content itself.
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .layerBackdrop(backdrop)
                    .drawBehind { drawDetourBackground(scheme, wallpaper, scrim) },
            )

            CompositionLocalProvider(
                LocalLayerBackdrop provides backdrop,
                // Provided to the content, not to the bar: the screens scroll
                // under the bar and are the ones that need to know how tall it is.
                LocalBottomBarClearance provides barHeight,
            ) {
                Scaffold(
                    // Transparent container, and this is not cosmetic: `Scaffold`
                    // defaults `containerColor` to an opaque `colorScheme.background`
                    // and paints it across the whole screen, which would hide the
                    // decorative layer above and leave the glass refracting a
                    // background nobody can see. `contentColor` is set explicitly
                    // because `contentColorFor` has no answer for a transparent fill.
                    containerColor = Color.Transparent,
                    contentColor = MaterialTheme.colorScheme.onBackground,
                    // No `bottomBar` slot. That slot is what used to reserve the
                    // bar's height as a layout inset, which laid every screen out
                    // *above* the bar — so the bar had no content under it to
                    // sample and could only ever refract the wallpaper. The bar
                    // is now an overlay in the `Box` below, and the content runs
                    // to the bottom of the screen behind it.
                ) { insets ->
                    Surface(
                        modifier = Modifier
                            .fillMaxSize()
                            // Top inset only. The bottom inset is dropped on
                            // purpose: the content is meant to reach the screen
                            // edge and scroll under the bar. Clearing the bar is
                            // the screens' job, via [LocalBottomBarClearance].
                            .padding(top = insets.calculateTopPadding()),
                        // Transparent, so the decorative layer behind shows through
                        // and the glass has a real background to refract. The content
                        // colour is pinned explicitly because `contentColorFor` has no
                        // answer for a transparent fill and would otherwise leave the
                        // text inheriting whatever was above it.
                        color = Color.Transparent,
                        contentColor = MaterialTheme.colorScheme.onSurface,
                    ) {
                        // The four tabs are peers, so switching between them is
                        // not a push — nothing is being opened. It is still
                        // directional, though: the bar puts them in a row and
                        // slides its indicator along it, so the content moves the
                        // way the indicator just moved. A plain fade would leave
                        // the indicator pointing at a direction the content did
                        // not come from.
                        //
                        // Shared-axis X: the outgoing screen leaves towards the
                        // side the incoming one arrives from, so the pair reads as
                        // one object travelling rather than two swapping places. A
                        // third of the width and not the whole of it — at full
                        // width the two are never on screen together and it reads
                        // as a carousel, which is a claim about the tabs being
                        // pages of one document that is not true here.
                        //
                        // **Composing the outgoing screen for the length of the
                        // transition is safe, and that is a fact about these four
                        // rather than a hope.** Every background job they start —
                        // the rule-index parse, the ip-stats fetch, the two 30 s
                        // clocks, the home screen's rate tick — lives in a
                        // `LaunchedEffect` keyed on that screen's own entry or on
                        // the tunnel's state, so the screen that is leaving does
                        // not run any of them a second time. It keeps drawing, and
                        // then it is gone.
                        val motion = LocalDetourMotion.current
                        AnimatedContent(
                            targetState = selected,
                            transitionSpec = {
                                val forward = targetState > initialState
                                val sign = if (forward) 1 else -1
                                (
                                    slideInHorizontally(motion.offsetSpatial) { sign * it / 3 } +
                                        fadeIn(motion.effects)
                                    ) togetherWith
                                    (
                                        slideOutHorizontally(motion.offsetSpatial) { -sign * it / 3 } +
                                            fadeOut(motion.effects)
                                        )
                            },
                            label = "destination",
                        ) { index ->
                            when (destinations[index]) {
                                Destination.HOME -> HomeScreen()
                                Destination.RULES -> RulesScreen()
                                Destination.LOGS -> LogsScreen()
                                Destination.SETTINGS -> SettingsScreen(
                                    // Null: this slot is the settings list. The
                                    // group pages are drawn over the whole app, as a
                                    // sibling of the bar — see where they are.
                                    page = null,
                                    onNavigate = { settingsPage = it },
                                    onOpenAbout = { showAbout = true },
                                )
                            }
                        }
                    }
                }
            }
        }

        // The bar, as a sibling of `barBackdrop`'s marked node so it samples the
        // content without being sampled by itself. It samples `barBackdrop`, and
        // that is load-bearing: were it left in the content's scope it would read
        // `LocalLayerBackdrop.current` as `backdrop` and refract only the
        // wallpaper — and if the local were absent entirely the glass would
        // silently fall back to a flat tint, which is a broken bar that still
        // looks like a bar.
        CompositionLocalProvider(LocalLayerBackdrop provides barBackdrop) {
            FloatingGlassBar(
                destinations = destinations,
                selected = selected,
                onSelect = { selected = it },
                modifier = Modifier
                    // `align` is the outermost modifier on purpose. The bar's
                    // own chain starts with its floating margins, and an `align`
                    // placed after them would be resolved against the padded box
                    // and shift the bar off centre.
                    .align(Alignment.BottomCenter)
                    // Measured here, at the call site, because only the app body
                    // can both see the bar and publish the value to the screens.
                    .onSizeChanged { barHeight = with(density) { it.height.toDp() } },
            )
        }

        // 关于, drawn last so it paints over everything in this `Box` — the
        // content *and* the bar — which is what a full page has to do. Being
        // last is also why it needs no `zIndex`: a `Box` paints its children in
        // order, so the last sibling is already the top one.
        //
        // **Why it is a sibling of the bar rather than a child of the content
        // `Box` above.** That `Box` is the node marked with `layerBackdrop`, so
        // everything inside it is recorded into the layer the glass refracts;
        // the bar is deliberately left outside that scope (see `barBackdrop`),
        // and 关于 covers the bar, so it belongs out here with it. The
        // consequence has to be honoured rather than worked around: this page is
        // *outside* the backdrop scope, so any glass drawn in it would have
        // nothing to sample and would silently fall back to a flat tint — a
        // surface that still looks like glass but refracts nothing. 关于
        // therefore paints an opaque background and uses no glass at all; see
        // `AboutScreen`'s own note.
        //
        // **Why it is not a new `Destination`.** `FloatingGlassBar` renders
        // every `Destination.entries` value as a tab and sizes its sliding
        // indicator from `destinations.size` (see `ui/components/FloatingBar.kt`),
        // so a fifth value would add a fifth bottom tab *and* re-measure the
        // indicator geometry — the bar's whole layout would shift — for a page
        // that is not a peer of 连接/规则/日志/设置. 关于 is a second-level page
        // reached from 设置, and it returns there; that is a different kind of
        // thing from a tab, so it gets a different kind of state.
        // A settings group's own page. Drawn here, beside 关于, for every reason
        // spelled out above: it covers the content *and* the bar, so it has to be
        // a sibling of both, and it is not a `Destination` because it is reached
        // from 设置 and returns there.
        //
        // **It is drawn before 关于 on purpose.** 关于 is opened *from* one of these
        // pages — the 数据与关于 group holds the row — so 关于 has to paint over the
        // page that opened it. Drawn the other way round, the group page would
        // cover the page it had just opened. Being a sibling rather than a child
        // also means closing 关于 lands back on the group page, which is where the
        // reader was, instead of on the settings list.
        // **Both are `AnimatedContent` over a nullable state, and that is the
        // shape the push needs.** The content lambda is called with the *old*
        // state for as long as the outgoing frame is being drawn, so a page that
        // is closing still has its own value to render instead of reading a null
        // and blanking out halfway through sliding away. Hoisting the last
        // non-null page into a second piece of state would have been the other
        // way to get that, and it is worse: it is a write during composition, and
        // it keeps a page alive in memory that nothing is looking at.
        //
        // **Each content is wrapped in a full-size `Box`, and that is load
        // bearing rather than tidiness.** `AnimatedContent` sizes itself to what
        // it is showing and animates that size between states; with `null` on one
        // side and a full-screen page on the other, the container would grow from
        // nothing and the page would appear to unroll out of a corner. A
        // constant full-size child means there is never a size to animate.
        //
        // **From the right, and the direction is the point.** These pages are
        // reached *from* the settings list and return to it, so opening is going
        // deeper: the page arrives from the right and closing takes it back out
        // the same way, which is what makes 返回 undo the opening rather than
        // doing something else.
        //
        // **The list behind does not move, and that is deliberate rather than an
        // oversight.** The list is the settings *tab's* content, drawn by the
        // `AnimatedContent` above; it is not this one's outgoing child. Putting a
        // second copy here to be the outgoing half would give the settings screen
        // two instances — two search fields, two scroll positions — and the one
        // that survived a page opening would not be the one the reader left. So
        // the outgoing slot holds an empty full-size `Box`, and the page simply
        // arrives over a list that holds still. (An earlier draft of this comment
        // claimed the list drifted a third of the way left. It does not: that
        // `slideOutHorizontally` below applies to the empty box.)
        //
        // The spec still describes a full push on both halves, which is what a
        // page-to-page transition would need — a group page opening another. No
        // page does that today, so half of each pair is never seen; it is kept
        // because the alternative is a crossfade the moment one does.
        val motion = LocalDetourMotion.current
        AnimatedContent(
            targetState = settingsPage,
            modifier = Modifier.fillMaxSize(),
            transitionSpec = {
                if (targetState != null) {
                    (
                        slideInHorizontally(motion.offsetSpatial) { it } +
                            fadeIn(motion.effects)
                        ) togetherWith
                        (
                            slideOutHorizontally(motion.offsetSpatial) { -it / 3 } +
                                fadeOut(motion.effects)
                            )
                } else {
                    (
                        slideInHorizontally(motion.offsetSpatial) { -it / 3 } +
                            fadeIn(motion.effects)
                        ) togetherWith
                        (
                            slideOutHorizontally(motion.offsetSpatial) { it } +
                                fadeOut(motion.effects)
                            )
                }
            },
            label = "settings-group-page",
        ) { page ->
            Box(Modifier.fillMaxSize()) {
                if (page != null) {
                    SettingsScreen(
                        page = page,
                        onNavigate = { settingsPage = it },
                        onOpenAbout = { showAbout = true },
                    )
                }
            }
        }

        AnimatedContent(
            targetState = showAbout,
            modifier = Modifier.fillMaxSize(),
            transitionSpec = {
                if (targetState) {
                    (
                        slideInHorizontally(motion.offsetSpatial) { it } +
                            fadeIn(motion.effects)
                        ) togetherWith
                        (
                            slideOutHorizontally(motion.offsetSpatial) { -it / 3 } +
                                fadeOut(motion.effects)
                            )
                } else {
                    (
                        slideInHorizontally(motion.offsetSpatial) { -it / 3 } +
                            fadeIn(motion.effects)
                        ) togetherWith
                        (
                            slideOutHorizontally(motion.offsetSpatial) { it } +
                                fadeOut(motion.effects)
                            )
                }
            },
            label = "about",
        ) { open ->
            Box(Modifier.fillMaxSize()) {
                if (open) {
                    AboutScreen(onClose = { showAbout = false })
                }
            }
        }

        // The "a new version is available" dialog, drawn near the end and beside 关于.
        //
        // It is a `Dialog`, i.e. a separate window, so its position in the
        // composition tree does not decide its stacking — being a late child of the
        // `Box` only keeps it readable next to the other overlay. The real constraint
        // is inside the window: a `Dialog` cannot sample the page's pixels, so glass
        // in it has nothing to refract, and this dialog therefore uses a plain
        // Material surface — see `ReleaseNotes.kt`.
        //
        // **Gated on `disclaimerAccepted` as well as the pending prompt.** Both windows
        // can want to be up at once on the launch after an upgrade — the update check
        // runs from `Application.onCreate`, before this screen exists — and a second
        // dialog over the consent gate turns a choice about the app into a choice
        // between two documents, one of which is not readable while the other is in
        // front of it. The gate wins; the update prompt is still pending, so it
        // appears the moment the gate is answered.
        //
        // The release comes back from `UpdateState` already narrowed to the channel
        // on screen, so this is a null test rather than a boolean test followed by a
        // cast: an `Available` result produced for the *other* channel must not raise
        // a dialog here, and a version number offered for a channel the user is not
        // on is the one thing this dialog must never show.
        val promptRelease = UpdateState.pendingPrompt(prefs.updateChannelValue)?.release
        if (disclaimerAccepted && promptRelease != null) {
            UpdateAvailableDialog(
                release = promptRelease,
                // 稍后 and a tap outside both mean "not this time", not "never remind
                // me again" — the flag is scoped to this process, for the reason given
                // on `UpdateState.promptShown`.
                onDismiss = { UpdateState.markPromptShown() },
                onDownload = {
                    // Open the browser first, then mark: even on a device with nothing
                    // that can open a link (`openInBrowser` swallows that exception),
                    // the user has seen the dialog and made a choice, and it must not
                    // come back just because the link would not open.
                    openInBrowser(context, promptRelease.url)
                    UpdateState.markPromptShown()
                },
            )
        }

        // The battery-optimization reminder, beside the update prompt and under the
        // consent gate.
        //
        // **Gated on the same things the update prompt is, plus one.** The consent
        // gate wins outright — a reminder about the battery must not sit on top of a
        // document the user has to read, which is the reason the update prompt waits
        // as well (see above). It also waits for the update prompt, because two
        // windows that both want to be up turn a reminder into a queue: the check
        // runs on every launch, so being one launch late costs nothing, whereas
        // stacking them means the second one gets answered without having been read.
        if (disclaimerAccepted && promptRelease == null &&
            !batteryUnrestricted && !batteryDismissed
        ) {
            BatteryPolicyDialog(
                onOpenSettings = {
                    // Dismissed before leaving rather than after returning. The
                    // action hands the screen to the system, so there is no "after"
                    // in this process to write to — and if the user comes back still
                    // restricted, the reminder has already been read and should not
                    // reappear until the next launch.
                    batteryDismissed = true
                    if (!BatteryPolicy.openSettings(context)) {
                        KernelState.log(
                            KernelState.LogEntry.Level.WARN,
                            "MainActivity",
                            "省电策略设置页打不开，无法引导用户",
                        )
                    }
                },
                onDismiss = { batteryDismissed = true },
            )
        }

        // The consent gate. Last in the `Box` so its window is created after the
        // update prompt's and therefore sits above it — though in practice only one
        // of the two is ever up, because that prompt is gated on this one having
        // been answered.
        //
        // **Why declining exits the app.** The alternative is a dialog that closes
        // and leaves a running app whose tunnel will never start — a window that
        // asked a question and then ignored the answer. Leaving is the honest
        // outcome, and it costs nothing: nothing has been written, so the next
        // launch asks again. `finish` rather than `finishAffinity` because this is
        // the app's only Activity.
        //
        // The `disclaimerText != null` test is not redundant with `isAccepted`: a
        // missing resource reports "accepted", but if that ever changed, this is
        // what keeps a body-less dialog off the screen.
        if (!disclaimerAccepted && disclaimerText != null) {
            DisclaimerDialog(
                text = disclaimerText,
                onAccept = {
                    // Writing the digest *is* accepting: the gate above is a
                    // comparison against this value, so the dialog leaves on the
                    // next recomposition with no local state to clear. Logged
                    // because a consent record that lives only in
                    // `SharedPreferences` is invisible to the archive the owner
                    // reads when reconstructing what happened.
                    prefs.updateDisclaimerAcceptedDigest(Disclaimer.digest(disclaimerText))
                    KernelState.log(
                        KernelState.LogEntry.Level.INFO, "MainActivity", "用户已接受免责声明",
                    )
                },
                onDecline = {
                    KernelState.log(
                        KernelState.LogEntry.Level.WARN, "MainActivity",
                        "用户不同意免责声明，退出应用",
                    )
                    (context as? Activity)?.finish()
                },
            )
        }
    }
}

/**
 * The decorative background: the user's photo, or the app's own soft brand
 * gradient and a few light blobs when there is none.
 *
 * The no-wallpaper path is drawn from the live [ColorScheme] rather than fixed
 * colours, so it follows the theme colour and the dark-mode setting. With a
 * wallpaper set the layer is the user's photo instead, and the glass refracts
 * that rather than *this app's* palette — which is the point of the feature. In
 * both cases everything is soft on purpose: it sits under the whole UI, so it
 * must read as light falling on the screen and never compete with the text on
 * top of it.
 *
 * **This used to be the bar's whole world, and is not any more.** The content
 * is no longer padded above the bar — it scrolls under it and is sampled by the
 * bar's `barBackdrop` along with this layer — so the bar refracts the wallpaper
 * *and* whatever the user has scrolled into it. What this layer still owns is
 * the other half: it is what the content's own glass cards sample, and it is
 * what shows through wherever the content is transparent.
 */
private fun DrawScope.drawDetourBackground(
    scheme: ColorScheme,
    wallpaper: ImageBitmap?,
    scrimPercent: Int,
) {
    if (wallpaper != null) {
        // Centre-crop: scale the photo so it *covers* the box on both axes, then
        // centre it, so a photo of any aspect ratio fills the screen with no
        // letterboxing. The offset is negative on the axis that overflows, which
        // is what centres it; the canvas clips the overhang.
        val scale = max(size.width / wallpaper.width, size.height / wallpaper.height)
        val drawnWidth = (wallpaper.width * scale).roundToInt()
        val drawnHeight = (wallpaper.height * scale).roundToInt()
        drawImage(
            image = wallpaper,
            dstOffset = IntOffset(
                (size.width.toInt() - drawnWidth) / 2,
                (size.height.toInt() - drawnHeight) / 2,
            ),
            dstSize = IntSize(drawnWidth, drawnHeight),
        )

        // The scrim, tinted with `scheme.surface` and NOT with black. The text
        // drawn over this layer is `onSurface`, which is dark in light theme and
        // light in dark theme — so a scrim that is always dark would guarantee
        // contrast in one theme and destroy it in the other. Tinting with the
        // theme's own surface keeps text legible in both, and follows 主题色 for
        // free. The strength is the user's (0..100) because only they can see
        // how busy their photo is.
        drawRect(color = scheme.surface.copy(alpha = scrimPercent.coerceIn(0, 100) / 100f))
        return
    }

    // No wallpaper: the app's own decorative layer, unchanged. The blobs are
    // this app's stand-in for "something for the glass to refract" and are
    // deliberately *not* drawn over a photo — two decorative layers on top of
    // each other is a fight, not a design.
    // The base. A two-stop gradient rather than a flat fill, so the light the
    // glass refracts has a direction to it.
    drawRect(
        brush = Brush.verticalGradient(
            colors = listOf(scheme.surface, scheme.surfaceContainer),
        ),
    )

    val width = size.width
    val height = size.height
    val minSide = size.minDimension

    // Three blobs in the brand hues, at fixed relative positions rather than
    // random ones: the layer is re-recorded often, and a random position would
    // shimmer.
    blob(scheme.primary.copy(alpha = 0.18f), Offset(width * 0.18f, height * 0.12f), minSide * 0.55f)
    blob(scheme.tertiary.copy(alpha = 0.14f), Offset(width * 0.88f, height * 0.32f), minSide * 0.45f)
    blob(scheme.primary.copy(alpha = 0.10f), Offset(width * 0.55f, height * 0.92f), minSide * 0.60f)
}

/**
 * One soft radial highlight, fading to transparent at its edge.
 *
 * The transparent outer stop is what makes it a light *blob* rather than a disc:
 * without it the circle would have a hard rim, which is the one thing a light
 * source never has.
 */
private fun DrawScope.blob(color: Color, center: Offset, radius: Float) {
    drawCircle(
        brush = Brush.radialGradient(
            colors = listOf(color, Color.Transparent),
            center = center,
            radius = radius,
        ),
        radius = radius,
        center = center,
    )
}

@Preview
@Composable
private fun DetourAppPreview() {
    DetourTheme { DetourAppBody() }
}
