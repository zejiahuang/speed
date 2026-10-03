package dev.detour.ui

import androidx.compose.animation.Crossfade
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.PlayArrow
import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import dev.detour.R
import dev.detour.core.BuildFlags
import dev.detour.core.KernelState
import dev.detour.core.Prefs
import dev.detour.core.Rate
import dev.detour.core.RulesRepository
import dev.detour.ui.components.DetourAlertDialog
import dev.detour.ui.components.DetourAssistChip
import dev.detour.ui.components.DetourCard
import dev.detour.ui.components.DetourCardStyle
import dev.detour.ui.components.DetourDivider
import dev.detour.ui.components.DetourKeyValueRow
import dev.detour.ui.components.DetourSectionCard
import dev.detour.ui.components.LocalBottomBarClearance
import dev.detour.ui.icons.DetourIcons
import kotlinx.coroutines.delay

/**
 * The hero screen: one control, and what it is doing.
 *
 * The information architecture is a single column on purpose. A tunnel is a
 * binary thing — it is up or it is not — and the first question anyone asks is
 * "is it working". Everything else on this screen answers the second question,
 * "what is it doing", and nothing competes with the control for attention.
 */
@Composable
fun HomeScreen() {
    val status by KernelState.status.collectAsState()
    val stats by KernelState.stats.collectAsState()
    val context = LocalContext.current
    val prefs = Prefs.of(context)
    val developerView = prefs.developerView

    // How much room the floating glass bar takes at the bottom of this screen.
    // Read once, in composition, for the trailing `Spacer` inside the scroll
    // below — this column has no `contentPadding` to put it in, which is why it
    // is a spacer and not a padding.
    val barClearance = LocalBottomBarClearance.current

    // The rate is sampled by the service; this only re-reads it so the number
    // moves. Polling here rather than plumbing another flow keeps the service
    // as the single writer of everything else.
    var tick by remember { mutableLongStateOf(0L) }
    LaunchedEffect(status.phase) {
        while (status.phase == KernelState.Phase.ON) {
            delay(500)
            tick++
        }
    }
    val down = remember(tick) { Rate.downBytesPerSecond }
    val up = remember(tick) { Rate.upBytesPerSecond }
    // Hoisted out of the delegated property: `status` is a `by`-delegate, and a
    // smart cast on a delegate is not allowed — the compiler cannot promise the
    // value does not change between the check and the use.
    val failure = status.message.takeIf { status.phase == KernelState.Phase.ERROR }

    // Disconnect is the one destructive action on this screen and it sits exactly
    // where the connect button sat a moment earlier, so a mis-tap ends a download.
    // When the user has asked for a confirmation, the button opens this instead of
    // tearing the tunnel down; when they have not, it disconnects immediately.
    var showDisconnectConfirm by remember { mutableStateOf(false) }

    // The column is scrollable because the content (a 168dp hero control plus
    // the rate and session cards) is taller than the viewport on a short screen;
    // without this the last card is clipped at the bottom rather than being
    // reachable. `weight` is deliberately not used anywhere on this
    // column: a weighted child inside a `verticalScroll` is measured with an
    // unbounded height and throws, so the two flexible gaps are fixed instead.
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Spacer(Modifier.height(32.dp))

        // Header: the app name top-left, the mode top-right. It used to be two
        // centred lines, which spent the two most valuable lines of a short screen
        // on a label that never changes, and pushed the power control down.
        //
        // The name is `titleLarge`/`SemiBold` in `onSurface` rather than
        // `headlineMedium` in the inherited content colour. At headline size it read
        // as the subject of the page and competed with the power control, which is
        // what the screen is actually for; smaller, heavier and darker turns it into
        // a title bar — present, but no longer the loudest thing here.
        //
        // `showModePicker` is hoisted above the Row so the picker is not a Row
        // child. It renders in its own window and so would contribute nothing to the
        // layout either way, but keeping it out leaves the Row holding exactly the
        // two header items and nothing else.
        var showModePicker by remember { mutableStateOf(false) }
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = stringResource(R.string.app_name),
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
                color = MaterialTheme.colorScheme.onSurface,
            )
            // Tappable, and the only place the mode can be changed. Before this the
            // mode was displayed and never settable, which made the choice the user
            // was asked for at design time unreachable at run time.
            //
            // Under [BuildFlags.TUN_ONLY] the mode is not a choice, so it is shown as
            // a plain badge — no chevron, nothing to tap, and no picker sheet to
            // open. It keeps the pill silhouette so the header reads the same in both
            // builds; the whole affordance hangs off the one flag, so restoring the
            // picker is flipping that flag and changing nothing else.
            if (BuildFlags.TUN_ONLY) {
                Surface(
                    shape = RoundedCornerShape(percent = 50),
                    color = MaterialTheme.colorScheme.surfaceContainerHigh,
                ) {
                    Text(
                        text = stringResource(R.string.home_mode_vpn),
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 12.dp, vertical = 5.dp),
                    )
                }
            } else {
                DetourAssistChip(
                    onClick = { if (!status.isRunning) showModePicker = true },
                    enabled = !status.isRunning,
                    label = stringResource(
                        when (status.mode) {
                            KernelState.Mode.PROXY -> R.string.home_mode_proxy
                            KernelState.Mode.VPN -> R.string.home_mode_vpn
                        },
                    ),
                    trailingIcon = {
                        Icon(
                            Icons.Filled.KeyboardArrowDown,
                            contentDescription = stringResource(R.string.home_switch_mode),
                            modifier = Modifier.size(18.dp),
                        )
                    },
                )
            }
        }
        if (showModePicker) {
            ModePicker(
                current = status.mode,
                onDismiss = { showModePicker = false },
                onPick = { picked ->
                    // Stored as well as shown: the holder is a process singleton,
                    // so a choice that only lived there would be forgotten the
                    // next time the app was killed.
                    prefs.updateMode(picked.name.lowercase())
                    KernelState.setStatus { it.copy(mode = picked) }
                    showModePicker = false
                },
            )
        }

        Spacer(Modifier.height(24.dp))

        PowerControl(
            status = status,
            onToggle = {
                if (status.isRunning) {
                    // Read here rather than captured, so toggling the setting takes
                    // effect on the very next tap.
                    if (prefs.confirmDisconnect) {
                        showDisconnectConfirm = true
                    } else {
                        KernelState.onDisconnect?.invoke()
                    }
                } else {
                    KernelState.onConnect?.invoke(context, status.mode)
                }
            },
        )

        Spacer(Modifier.height(20.dp))

        Text(
            text = statusText(status),
            style = MaterialTheme.typography.titleLarge,
            textAlign = TextAlign.Center,
        )

        if (failure != null) {
            Spacer(Modifier.height(8.dp))
            Text(
                text = failure,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                textAlign = TextAlign.Center,
            )
            // The kernel's message is precise and assumes a reader who knows what
            // a rule document is. This is the sentence for everyone else.
            Spacer(Modifier.height(4.dp))
            Text(
                text = stringResource(R.string.home_error_hint),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }

        Spacer(Modifier.height(24.dp))

        // Hidden rather than shown as "0 B/s". The row exists to answer "is it
        // moving", and a pair of zeros answers that question wrongly — it reads as
        // a stalled tunnel. Turning the setting off removes the question.
        if (prefs.homeShowRate) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                RateCard(
                    modifier = Modifier.weight(1f),
                    label = stringResource(R.string.home_speed_down),
                    icon = Icons.Filled.KeyboardArrowDown,
                    bytesPerSecond = down,
                )
                RateCard(
                    modifier = Modifier.weight(1f),
                    label = stringResource(R.string.home_speed_up),
                    icon = Icons.Filled.KeyboardArrowUp,
                    bytesPerSecond = up,
                )
            }
        }

        // Hidden entirely under TUN_ONLY: there is no proxy listener to point
        // anything at, so the card would be an address that does not work.
        if (!BuildFlags.TUN_ONLY &&
            status.isRunning &&
            status.mode == KernelState.Mode.PROXY &&
            status.proxyPort > 0
        ) {
            Spacer(Modifier.height(12.dp))
            ProxyAddressCard(status.proxyPort)
        }

        Spacer(Modifier.height(12.dp))

        SessionCard(stats, developerView)

        // Which rules are in force and how old they are. The home screen used to
        // say nothing about the rule set at all, so "am I running last week's
        // blocklist" could only be answered by opening another screen. Read from
        // `Prefs`/the cache file rather than plumbed through the kernel: it is
        // local configuration, not tunnel state.
        Spacer(Modifier.height(12.dp))
        val sourceLabel = prefs.selectedSource.labelRes
            ?.let { stringResource(it) }
            ?: prefs.selectedSource.label
        val cache = RulesRepository.cacheFile(context)
        // The age label changes at minute granularity, so it gets its own slow
        // clock rather than riding the 500 ms stats tick. The fast tick is gated
        // on the tunnel running; this card is always visible, and with the tunnel
        // off the fast tick never fires — which would freeze the age at whatever
        // it read the last time anything else happened to recompose. A 30 s
        // period keeps the text right in every state at 1/60th the cost of the
        // fast tick.
        var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
        LaunchedEffect(Unit) {
            while (true) {
                delay(30_000L)
                now = System.currentTimeMillis()
            }
        }
        // `formatAge` is `@Composable`, so the whole value has to be produced
        // here in the composable body — it cannot be built inside a plain lambda.
        val cacheValue = if (!cache.isFile) {
            stringResource(R.string.home_rule_cache_none)
        } else {
            // Fresh means younger than one refresh interval. `refreshHours` is the
            // user's own "how often should this update" answer, so it is the right
            // yardstick for "is this stale" rather than a number picked here.
            // The comparison and the age below read the *same* `now`: two separate
            // clock reads could straddle the boundary and print "已过期" beside
            // "刚刚" in the same row.
            val freshRes = if (now - cache.lastModified() <
                prefs.refreshHours * 3_600_000L
            ) {
                R.string.home_rule_cache_fresh
            } else {
                R.string.home_rule_cache_stale
            }
            // The " · " is punctuation, not prose, so it is built in Kotlin and
            // not in a string resource that would then need translating.
            "${stringResource(freshRes)} · " + stringResource(
                R.string.home_rule_cache_detail,
                formatBytes(cache.length()),
                formatAge(cache.lastModified(), now),
            )
        }
        DetourSectionCard(title = stringResource(R.string.home_rule_card_title)) {
            DetourKeyValueRow(stringResource(R.string.home_rule_source_label), sourceLabel)
            DetourDivider()
            DetourKeyValueRow(stringResource(R.string.home_rule_cache_label), cacheValue)
        }

        // Only while the tunnel is up: with nothing running these counters are a
        // row of zeros that reads as "the tunnel is broken". The question they
        // answer — is traffic going through the rules or straight out — only
        // exists while there is traffic.
        if (status.isRunning) {
            Spacer(Modifier.height(12.dp))
            val matched = stats.flowsMatchedRules
            val direct = stats.flowsDirect
            DetourSectionCard(title = stringResource(R.string.home_hits_title)) {
                // The ratio is the headline of this card, so it sits above the
                // divider as a sentence rather than as one more key/value row.
                // Padded to line up with the rows below, which pad themselves.
                Text(
                    text = if (matched + direct == 0L) {
                        stringResource(R.string.home_hits_none)
                    } else {
                        stringResource(
                            R.string.home_hits_ratio,
                            "${matched * 100 / (matched + direct)}%",
                        )
                    },
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                DetourDivider()
                DetourKeyValueRow(stringResource(R.string.home_hits_matched), matched.toString())
                DetourDivider()
                DetourKeyValueRow(stringResource(R.string.home_hits_direct), direct.toString())
                DetourDivider()
                DetourKeyValueRow(stringResource(R.string.home_hits_live), stats.liveFlows.toString())
                DetourDivider()
                // The two rows below answer the question the three above raise when
                // the ratio is low: a client that resolves over DoH arrives as a
                // bare address, so the rule set cannot be consulted at all and the
                // flow is counted as direct whatever the rules say. "无名流量" is
                // how much of the traffic that was; "其中握手补名" is how much of it
                // the client's own TLS handshake put back under the rules. Read
                // together they say whether the tunnel is actually steering, which
                // the ratio on its own cannot.
                DetourKeyValueRow(
                    stringResource(R.string.home_hits_unnamed),
                    stats.flowsWithoutName.toString(),
                )
                DetourDivider()
                DetourKeyValueRow(
                    stringResource(R.string.home_hits_named_by_sni),
                    stats.flowsNamedBySni.toString(),
                )
            }
        }

        Spacer(Modifier.height(24.dp))

        // The bar floats over the content now rather than being a `Scaffold`
        // `bottomBar`, so nothing else reserves its height and this column would
        // otherwise let its last card end up underneath it. It is a child of the
        // scrolling `Column`, not a modifier on the `verticalScroll`: placed
        // outside, it would push the whole column up and the content would stop
        // short of the bar — which is exactly the "content does not pass under
        // the bar" complaint this change is meant to fix. As the last child it
        // simply extends the scrollable range by the bar's height, so the final
        // card can be scrolled clear while everything above still runs beneath
        // the glass.
        Spacer(Modifier.height(barClearance))
    }

    if (showDisconnectConfirm) {
        DisconnectConfirm(
            onDismiss = { showDisconnectConfirm = false },
            onConfirm = {
                showDisconnectConfirm = false
                KernelState.onDisconnect?.invoke()
            },
        )
    }
}

/**
 * The confirmation the power button shows when "断开前二次确认" is on.
 *
 * Its own composable rather than an inline `AlertDialog` so the two buttons read
 * the same way every time: confirming is the affirmative action, dismissing is the
 * safe one, and the safe one is the plain text button.
 */
@Composable
private fun DisconnectConfirm(onDismiss: () -> Unit, onConfirm: () -> Unit) {
    DetourAlertDialog(
        onDismissRequest = onDismiss,
        title = stringResource(R.string.home_disconnect_confirm_title),
        text = { Text(stringResource(R.string.home_disconnect_confirm_text)) },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text(stringResource(R.string.home_disconnect_confirm_ok))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(stringResource(android.R.string.cancel))
            }
        },
    )
}

@Composable
private fun statusText(status: KernelState.Status): String = stringResource(
    when (status.phase) {
        KernelState.Phase.OFF -> R.string.home_state_off
        KernelState.Phase.STARTING -> R.string.home_state_connecting
        KernelState.Phase.ON -> R.string.home_state_on
        KernelState.Phase.ERROR -> R.string.home_state_error
    },
)

/**
 * The control.
 *
 * Material 3 Expressive's contribution here is the motion: the button scales on
 * press and the colour crosses over rather than cutting. On a screen whose whole
 * job is a binary state, a hard cut reads as a glitch.
 */
@Composable
private fun PowerControl(status: KernelState.Status, onToggle: () -> Unit) {
    val running = status.isRunning
    val busy = status.phase == KernelState.Phase.STARTING

    val target = when {
        running -> MaterialTheme.colorScheme.primary
        status.phase == KernelState.Phase.ERROR -> MaterialTheme.colorScheme.errorContainer
        else -> MaterialTheme.colorScheme.surfaceVariant
    }
    val container by animateColorAsState(target, label = "power-container")
    val content = when {
        running -> MaterialTheme.colorScheme.onPrimary
        status.phase == KernelState.Phase.ERROR -> MaterialTheme.colorScheme.onErrorContainer
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    val scale by animateFloatAsState(if (busy) 0.94f else 1f, label = "power-scale")
    // A soft halo that fades on while the tunnel is up. Drawn *behind* the circle
    // (first child of the Box below) so the control reads as "lit" at a glance
    // without touching the circle's own colours. Transparent rather than absent
    // when idle, so the alpha can animate instead of popping.
    val halo by animateColorAsState(
        if (running) {
            MaterialTheme.colorScheme.primary.copy(alpha = 0.12f)
        } else {
            Color.Transparent
        },
        label = "power-halo",
    )

    Box(contentAlignment = Alignment.Center) {
        Box(
            modifier = Modifier
                .size(192.dp)
                .background(halo, CircleShape),
        )
        Surface(
            onClick = onToggle,
            enabled = !busy,
            shape = CircleShape,
            color = container,
            contentColor = content,
            modifier = Modifier
                .size(168.dp)
                .scale(scale),
        ) {
            Box(contentAlignment = Alignment.Center) {
                // The glyph crosses over instead of cutting: on a screen whose
                // entire job is a binary state, a hard swap reads as a glitch.
                // The ternary is what makes the control honest — it used to draw
                // `PlayArrow` in *both* states, so the one button that exists to
                // say which way it will go said nothing.
                Crossfade(targetState = running, label = "power-icon") { isRunning ->
                    Icon(
                        imageVector = if (isRunning) DetourIcons.Stop else Icons.Filled.PlayArrow,
                        // Still keyed on `running`, not the crossfade frame: the
                        // control must announce 断开 while running and 连接 otherwise.
                        contentDescription = stringResource(
                            if (running) R.string.home_action_disconnect else R.string.home_action_connect,
                        ),
                        modifier = Modifier.size(64.dp),
                    )
                }
            }
        }
    }
}

/**
 * The address to paste into whatever needs proxying.
 *
 * A copy button rather than selectable text: the destination is another app's
 * settings field, and retyping `127.0.0.1:1080` from a screenshot is the kind of
 * friction that makes a working feature look broken.
 */
@Composable
private fun ProxyAddressCard(port: Int) {
    val context = LocalContext.current
    val clipboard = context.getSystemService(ClipboardManager::class.java)
    var copied by remember { mutableStateOf(false) }
    val address = "127.0.0.1:" + port

    LaunchedEffect(copied) {
        if (copied) {
            delay(2000)
            copied = false
        }
    }

    // Glass, and the two labels below read `onSurfaceVariant` rather than the
    // `onSecondaryContainer` they carried while this was a secondaryContainer
    // block. The glass tint is `surfaceContainer`, so a text colour named for a
    // secondary container would be tuned for a background this card no longer
    // draws — a legibility bug that only shows up over a wallpaper.
    DetourCard(
        modifier = Modifier.fillMaxWidth(),
        style = DetourCardStyle.Glass,
    ) {
        Row(
            modifier = Modifier.padding(start = 16.dp, end = 8.dp, top = 8.dp, bottom = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(
                    stringResource(R.string.home_proxy_address),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(address, style = MaterialTheme.typography.titleMedium)
                Text(
                    stringResource(R.string.home_proxy_how),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            TextButton(
                onClick = {
                    clipboard?.setPrimaryClip(ClipData.newPlainText("proxy", address))
                    copied = true
                },
            ) {
                Text(stringResource(if (copied) R.string.home_copied else R.string.home_copy))
            }
        }
    }
}

/**
 * Mode chooser, with the cost of each written down.
 *
 * The two are not interchangeable and the dialog says so: one needs no
 * permission and only helps apps that honour a proxy, the other takes over
 * everything and needs the system's consent. A picker that showed only the names
 * would leave the user guessing which one they wanted.
 */
@Composable
private fun ModePicker(
    current: KernelState.Mode,
    onDismiss: () -> Unit,
    onPick: (KernelState.Mode) -> Unit,
) {
    DetourAlertDialog(
        onDismissRequest = onDismiss,
        title = stringResource(R.string.home_mode_pick),
        text = {
            Column {
                ModeRow(
                    title = stringResource(R.string.home_mode_proxy),
                    detail = stringResource(R.string.home_mode_proxy_desc),
                    selected = current == KernelState.Mode.PROXY,
                    onClick = { onPick(KernelState.Mode.PROXY) },
                )
                Spacer(Modifier.height(8.dp))
                ModeRow(
                    title = stringResource(R.string.home_mode_vpn),
                    detail = stringResource(R.string.home_mode_vpn_desc),
                    selected = current == KernelState.Mode.VPN,
                    onClick = { onPick(KernelState.Mode.VPN) },
                )
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(android.R.string.cancel)) }
        },
    )
}

@Composable
private fun ModeRow(title: String, detail: String, selected: Boolean, onClick: () -> Unit) {
    // Filled on purpose — do not "finish the job" by moving this to Glass. This
    // row only ever renders inside `ModePicker`, which is a `Dialog`, and a
    // Compose `Dialog` is a separate window: it has no access to the page's
    // pixels, so a glass surface here would have no backdrop to be glass *of*
    // and would just be a translucent card on the dialog's own dim scrim. It
    // would also be glass-on-glass, since the picker already floats over the
    // frosted home screen. The selected/unselected container colours below are
    // the whole signal this row carries, and they need an opaque surface to read
    // against.
    DetourCard(
        onClick = onClick,
        style = DetourCardStyle.Filled,
        colors = CardDefaults.cardColors(
            containerColor = if (selected) {
                MaterialTheme.colorScheme.primaryContainer
            } else {
                MaterialTheme.colorScheme.surfaceContainerHigh
            },
        ),
    ) {
        Column(Modifier.padding(12.dp)) {
            Text(title, style = MaterialTheme.typography.titleSmall)
            Text(
                detail,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun RateCard(
    modifier: Modifier = Modifier,
    label: String,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    bytesPerSecond: Long,
) {
    DetourCard(
        modifier = modifier,
        style = DetourCardStyle.Glass,
    ) {
        Column(Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    icon,
                    contentDescription = null,
                    modifier = Modifier.size(16.dp),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.size(6.dp))
                Text(
                    label,
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.height(6.dp))
            Text(
                text = formatRate(bytesPerSecond),
                style = MaterialTheme.typography.headlineSmall,
            )
        }
    }
}

@Composable
private fun SessionCard(stats: dev.detour.core.Kernel.Stats, developerView: Boolean) {
    DetourCard(
        modifier = Modifier.fillMaxWidth(),
        style = DetourCardStyle.Glass,
    ) {
        Column(Modifier.padding(16.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(
                    stringResource(R.string.home_total),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    formatBytes(stats.bytesTotal),
                    style = MaterialTheme.typography.titleMedium,
                )
            }

            Spacer(Modifier.height(8.dp))
            Text(
                text = stringResource(R.string.home_domains_saved, stats.dnsAnsweredLocally),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // Everything below is kernel detail. It is genuinely useful when
            // something is wrong and noise when it is not, which is why it is
            // behind a setting rather than always on.
            if (developerView) {
                Spacer(Modifier.height(12.dp))
                DeveloperRows(stats)
            }
        }
    }
}

@Composable
private fun DeveloperRows(stats: dev.detour.core.Kernel.Stats) {
    // Every label comes from `strings.xml` — the same reason as everywhere else:
    // a hard-coded Chinese literal is a string that can never be translated.
    val rows = listOf(
        stringResource(R.string.dev_tcp_open_close) to "${stats.tcpOpened} / ${stats.tcpClosed}",
        stringResource(R.string.dev_tcp_failures) to stats.tcpConnectFailures.toString(),
        stringResource(R.string.dev_tcp_rejected) to stats.tcpRejected.toString(),
        stringResource(R.string.dev_udp_open_evict) to "${stats.udpOpened} / ${stats.udpEvicted}",
        stringResource(R.string.dev_dns_local) to "${stats.dnsAnsweredLocally} / ${stats.dnsQueries}",
        stringResource(R.string.dev_dns_trimmed) to stats.dnsTrimmed.toString(),
        // Answered over queries, the same shape as the row above. Both stay zero
        // until an upstream resolver is configured and a query for a name the
        // rule set does not own actually leaves — which is the whole point of
        // showing it: it is the only on-screen proof the resolver is in use.
        stringResource(R.string.dev_dns_upstream) to
            "${stats.dnsUpstreamAnswered} / ${stats.dnsUpstreamQueries}",
        stringResource(R.string.dev_live_flows) to stats.liveFlows.toString(),
        stringResource(R.string.dev_packets) to "${stats.packetsIn} / ${stats.packetsOut}",
    )
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        for ((label, value) in rows) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(
                    label,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(value, style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

// `formatBytes` / `formatRate` / `formatAge` live in `ui/Format.kt` now. They are
// in this same package, so the call sites above need no import — they moved so
// the rules screen and this screen print an age the same way instead of each
// keeping its own copy.
