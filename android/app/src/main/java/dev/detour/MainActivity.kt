package dev.detour

import android.Manifest
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
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
import com.kyant.backdrop.backdrops.layerBackdrop
import com.kyant.backdrop.backdrops.rememberLayerBackdrop
import dev.detour.core.DetourVpnService
import dev.detour.core.KernelState
import dev.detour.core.Prefs
import dev.detour.core.Rate
import dev.detour.core.UpdateChecker
import dev.detour.core.UpdateState
import dev.detour.core.WallpaperStore
import dev.detour.ui.AboutScreen
import dev.detour.ui.Destination
import dev.detour.ui.HomeScreen
import dev.detour.ui.LogsScreen
import dev.detour.ui.RulesScreen
import dev.detour.ui.SettingsScreen
import dev.detour.ui.components.FloatingGlassBar
import dev.detour.ui.components.LocalBottomBarClearance
import dev.detour.ui.components.LocalLayerBackdrop
import dev.detour.ui.components.UpdateAvailableDialog
import dev.detour.ui.openInBrowser
import dev.detour.ui.theme.DetourTheme
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

        askForNotifications()

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
                DetourAppBody()
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

@Composable
private fun DetourAppBody() {
    var selected by rememberSaveable { mutableIntStateOf(0) }
    // Whether 关于 is on top of everything. A boolean rather than a fifth
    // `Destination`, and `rememberSaveable` for the same reason `selected` uses
    // it: the page survives a rotation instead of snapping back to the settings
    // list. Why it is not a destination is explained where the overlay is drawn.
    var showAbout by rememberSaveable { mutableStateOf(false) }
    val destinations = Destination.entries
    val scheme = MaterialTheme.colorScheme
    val context = LocalContext.current
    val prefs = Prefs.of(context)

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
                        when (destinations[selected]) {
                            Destination.HOME -> HomeScreen()
                            Destination.RULES -> RulesScreen()
                            Destination.LOGS -> LogsScreen()
                            Destination.SETTINGS -> SettingsScreen(onOpenAbout = { showAbout = true })
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
        if (showAbout) {
            AboutScreen(onClose = { showAbout = false })
        }

        // The "a new version is available" dialog, drawn last and beside 关于.
        //
        // It is a `Dialog`, i.e. a separate window, so its position in the
        // composition tree does not decide its stacking — being the last child of the
        // `Box` only keeps it readable next to the other overlay. The real constraint
        // is inside the window: a `Dialog` cannot sample the page's pixels, so glass
        // in it has nothing to refract, and this dialog therefore uses a plain
        // Material surface — see `ReleaseNotes.kt`.
        //
        // The branch is only entered when `shouldPrompt` is true, which implies
        // `result` is `Available`, so the cast below is safe — `result` is mutable
        // state, Kotlin does not smart-cast it, and an explicit cast is the only way
        // to say that. `release` is hoisted into a local because both callbacks below
        // need it, and a lambda should capture the result this frame saw.
        if (UpdateState.shouldPrompt) {
            val release = (UpdateState.result as UpdateChecker.Result.Available).release
            UpdateAvailableDialog(
                release = release,
                // 稍后 and a tap outside both mean "not this time", not "never remind
                // me again" — the flag is scoped to this process, for the reason given
                // on `UpdateState.promptShown`.
                onDismiss = { UpdateState.markPromptShown() },
                onDownload = {
                    // Open the browser first, then mark: even on a device with nothing
                    // that can open a link (`openInBrowser` swallows that exception),
                    // the user has seen the dialog and made a choice, and it must not
                    // come back just because the link would not open.
                    openInBrowser(context, release.url)
                    UpdateState.markPromptShown()
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
