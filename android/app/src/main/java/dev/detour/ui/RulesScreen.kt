package dev.detour.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Clear
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import dev.detour.R
import dev.detour.core.Kernel
import dev.detour.core.KernelState
import dev.detour.core.DetourVpnService
import dev.detour.core.Prefs
import dev.detour.core.RuleIndex
import dev.detour.core.RulesRepository
import dev.detour.ui.components.DetourAlertDialog
import dev.detour.ui.components.DetourCard
import dev.detour.ui.components.DetourCardStyle
import dev.detour.ui.components.DetourEmptyState
import dev.detour.ui.components.DetourFilterChip
import dev.detour.ui.components.DetourPageHeader
import dev.detour.ui.components.DetourSwitch
import dev.detour.ui.components.LocalBottomBarClearance
import dev.detour.ui.components.plusBarClearance
import java.text.DateFormat
import java.util.Date
import kotlin.math.roundToInt
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * The two certificate verdicts that mean "this address does not work for this
 * host". Everything else — 探测中, 可用, or a verdict this build has never heard
 * of — is not something a domain row should shout about, so the rollup counts
 * only these.
 *
 * The same set decides what "只看有问题的" keeps, so a row the page colours red
 * is a row the filter shows: one definition of "broken" for both, because a
 * filter that disagreed with the red text next to it would be worse than no
 * filter at all.
 */
private val BAD_VERDICTS = setOf("wrong_certificate", "unreachable")

/**
 * What the selector thinks of one address, as a value rather than a string.
 *
 * The 选择 line in [AddressRow] and the "只看有问题的" filter both need this
 * judgement, and they must agree: if the filter had its own copy of the rules,
 * the day the two drifted it would hide a row that renders itself as 曾失败 —
 * the exact contradiction a filter exists to prevent.
 *
 * [UNPROBED] is kept apart from the failures on purpose. An address the selector
 * has never dialled is not broken, it is new; folding it into "有问题" would
 * sweep an entire freshly-populated table into the filter the moment the tunnel
 * came up.
 */
private enum class SelectorVerdict { UNPROBED, SILENT, FAILED, OK }

/**
 * The selector's verdict for [stat], or [SelectorVerdict.UNPROBED] when it has no
 * sample for the address.
 *
 * The order of the tests is the point, not an accident:
 *
 * * "Connected and moved nothing" is a more specific diagnosis than the generic
 *   曾失败, so it is tested first.
 * * Cooldown is tested before the health ratio, because a cooling-down address
 *   can still carry a healthy ratio and would otherwise read 可用 while the
 *   selector is refusing to use it.
 */
private fun selectorVerdict(stat: Kernel.AddressStat?): SelectorVerdict = when {
    stat == null -> SelectorVerdict.UNPROBED
    stat.silent > 0 && stat.successes == 0L -> SelectorVerdict.SILENT
    stat.inCooldown -> SelectorVerdict.FAILED
    stat.failures > 0 && stat.successes == 0L -> SelectorVerdict.FAILED
    stat.health >= 0.5 -> SelectorVerdict.OK
    else -> SelectorVerdict.FAILED
}

/**
 * Whether one host/address pair is something "只看有问题的" should keep.
 *
 * Two independent judgements can make an address a problem, and both are shown
 * as columns on the row, so both count here: the certificate the address served
 * for this host (a member of [BAD_VERDICTS]), or the selector having given up on
 * it. A probe still in flight, an unknown future verdict, and an address nobody
 * has dialled yet are all *not* problems — they are simply not decided, and
 * calling them broken would fill the filter with rows that are working.
 */
private fun Kernel.IpStats.isProblem(domain: String, address: String): Boolean {
    if (verdictsFor(domain).any { it.ip == address && it.verdict in BAD_VERDICTS }) return true
    val selector = selectorVerdict(addressStat(address))
    return selector == SelectorVerdict.SILENT || selector == SelectorVerdict.FAILED
}

/**
 * A hairline guide down the left of a nested row, at [x] from the row's edge.
 *
 * The hierarchy here is three levels deep (group -> domain -> address) and the
 * rows below the group are deliberately *not* cards: a group can expand to
 * thousands of address rows, and wrapping each in a frosted surface is the
 * difference between a list that scrolls and one that stutters. A rail drawn
 * with `drawBehind` costs one line per row and says "this is inside that" with
 * no container at all — which is the whole point of choosing it over a nested
 * card.
 *
 * `drawBehind` rather than a spacer `Box` with `fillMaxHeight()`: inside a lazy
 * item the height is unbounded, so a `fillMaxHeight` child would collapse to
 * nothing. `drawBehind` runs after layout and gets the row's real height, so the
 * rail is exactly as tall as the row it belongs to.
 *
 * The colour is a parameter, read from the theme at the call site, because this
 * is a plain modifier function and has no composition to read `MaterialTheme`
 * from — and because a hard-coded grey would break over a wallpaper.
 */
private fun Modifier.hierarchyRail(x: Dp, color: Color): Modifier = drawBehind {
    val px = x.toPx()
    drawLine(
        color = color,
        start = Offset(px, 0f),
        end = Offset(px, size.height),
        strokeWidth = 1.dp.toPx(),
    )
}

/**
 * The rule set, and the three levels at which it can be switched off.
 *
 * Three levels because the three questions people actually ask are different:
 *
 * * *"Do I want game traffic routed?"* — a group.
 * * *"This one domain breaks when routed"* — a domain.
 * * *"This particular address serves the wrong certificate"* — an address.
 *
 * Each level is a filter applied to the document before it reaches the kernel,
 * not a runtime flag the kernel has to know about. The kernel's job is to relay
 * what it is given; deciding what to give it is this screen's.
 */
@Composable
fun RulesScreen() {
    val context = LocalContext.current
    val prefs = Prefs.of(context)
    val scope = rememberCoroutineScope()

    var index by remember { mutableStateOf<RuleIndex?>(null) }
    var loading by remember { mutableStateOf(true) }
    var query by remember { mutableStateOf("") }
    // Two levels of expansion, because the tree has two levels below the group:
    // a group opens onto domains, a domain onto addresses. Held as sets of keys
    // rather than a per-row flag so that a row scrolling out of the lazy list and
    // back keeps its state.
    var expandedGroups by remember { mutableStateOf(setOf<String>()) }
    var expandedDomains by remember { mutableStateOf(setOf<String>()) }
    var showAddSource by remember { mutableStateOf(false) }
    // The last import failure, shown as a dialog and cleared on dismiss. A
    // failure here has no other way to reach the user: the source row it would
    // have added is simply absent, which looks exactly like the picker having
    // done nothing.
    var importError by remember { mutableStateOf<String?>(null) }
    // Whether the list is narrowed to rows with a bad verdict. Off by default:
    // the page's job is to show the rule set, and this filter is a lens on it,
    // not its resting state.
    var problemsOnly by remember { mutableStateOf(false) }
    // Set while a "disable every group" is waiting on a confirmation, holding the
    // exact key set the confirm button will write. The set is captured rather
    // than recomputed on confirm so that the dialog and the action can never
    // disagree about what "all" meant when the button was pressed.
    var pendingDisableAll by remember { mutableStateOf<Set<String>?>(null) }
    // Read from the store rather than held locally: a switch that only changes
    // the screen is a switch that does nothing, and the whole point of this page
    // is that what it shows is what the kernel is running.
    val disabled = prefs.disabledRules

    // The last-verdict columns. The kernel owns these tables and they change
    // continuously, but this page is a rule browser, not a live dashboard: one
    // snapshot on open is what was asked for, and polling would make the screen
    // flicker under a finger that is trying to read it.
    val status by KernelState.status.collectAsState()
    var ipStats by remember { mutableStateOf<Kernel.IpStats?>(null) }
    LaunchedEffect(status.isRunning) {
        // Keyed on isRunning rather than Unit so this both clears the columns when
        // the tunnel stops while the page is open (they are only meaningful while
        // something is running) and takes one fresh snapshot if it comes up with
        // the page already on screen — without paying for a second fetch on the
        // ordinary path, which is one open, one read.
        if (!status.isRunning) {
            ipStats = null
            // And drop the problem filter with it. Without verdict data the
            // filter has nothing to judge by, and leaving it lit would either
            // empty the list (a lie: nothing is known to be broken) or silently
            // do nothing (a lie of a different shape). Clearing it means the
            // chip is unselected exactly when it is disabled, so the two never
            // contradict each other on screen.
            problemsOnly = false
            return@LaunchedEffect
        }
        // Off the main thread on purpose: this is a JNI call that takes the
        // selector and verdict locks, and blocking a frame on it is exactly the
        // stall this screen cannot afford.
        ipStats = withContext(Dispatchers.IO) {
            runCatching { KernelState.ipStats?.invoke() }.getOrNull()
        }
    }

    // A slow clock for the "x 分钟前" ages below. Only the age text reads it, and
    // ages are coarse (the finest bucket is seconds), so a fast tick would burn
    // frames to redraw a string that changes at most once a minute.
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(30_000L)
            now = System.currentTimeMillis()
        }
    }

    // One action for "load a different document", shared by the source chips and
    // the add-source dialog. Writing the selection is only half of it: the cache
    // holds the previous document and a running tunnel holds the previous set, so
    // both have to be told before the new source means anything.
    val loadIndex: () -> Unit = {
        scope.launch {
            loading = true
            index = withContext(Dispatchers.IO) {
                runCatching { RuleIndex.parse(context) }
                    .onFailure {
                        KernelState.log(KernelState.LogEntry.Level.ERROR, "Rules", "解析规则失败：${it.message}")
                    }
                    .getOrNull()
            }
            loading = false
        }
    }

    val selectSource: (String) -> Unit = { id ->
        prefs.updateRuleSource(id)
        RulesRepository.invalidate(context)
        DetourVpnService.reloadRulesIfRunning()
        loadIndex()
    }

    // The rule-file picker. `*/*` because a hosts file has no registered MIME
    // type — providers report `text/plain`, `application/octet-stream`, or
    // nothing — and the content is validated after reading either way, so a
    // narrower filter could only hide files the user is entitled to pick.
    //
    // The work is `RulesRepository.importLocal`, the same call the settings screen
    // makes, so the two entry points cannot drift; this callback only turns its
    // outcome into what this screen shows. A success needs no announcement — the
    // new chip appears and is selected, which is the change the user asked for —
    // while a failure has nothing else to make it visible, hence the dialog.
    val rulesImportPicker = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocument(),
    ) { uri ->
        // Cancelled pick: nothing to report, same as the dialogs here.
        if (uri != null) {
            scope.launch {
                when (val outcome = withContext(Dispatchers.IO) {
                    RulesRepository.importLocal(context, uri)
                }) {
                    is RulesRepository.LocalImport.Added -> loadIndex()
                    is RulesRepository.LocalImport.AlreadyPresent -> loadIndex()
                    is RulesRepository.LocalImport.Failed -> {
                        val reason = outcome.reason
                        KernelState.log(KernelState.LogEntry.Level.ERROR, "Rules", "导入本地规则失败：$reason")
                        importError = reason
                    }
                }
            }
        }
    }

    // One place where a switch becomes a kernel change. Writing to the store is
    // only half of it: a tunnel that is already up is holding the document it
    // started with, and until it is handed the new one the switch is still
    // decoration.
    val applySwitch: (String, Boolean) -> Unit = { key, enabled ->
        prefs.setRuleEnabled(key, enabled)
        DetourVpnService.reloadRulesIfRunning()
    }

    // The one write behind "全部关闭", shared by the immediate path and the
    // confirmed one so the two can never drift apart.
    val disableAllGroups: (Set<String>) -> Unit = { groups ->
        prefs.replaceDisabledRules(groups)
        DetourVpnService.reloadRulesIfRunning()
    }

    LaunchedEffect(Unit) {
        loading = true
        index = withContext(Dispatchers.IO) {
            runCatching { RuleIndex.parse(context) }
                .onFailure {
                    KernelState.log(KernelState.LogEntry.Level.ERROR, "Rules", "解析规则失败：${it.message}")
                }
                .getOrNull()
        }
        loading = false
    }

    Column(Modifier.fillMaxSize()) {
        val current = index
        // The two caption lines the header used to stack (enabled counts, then
        // cache freshness) are folded into one subtitle joined by " · ". The
        // header component gives a subtitle one line and one type scale, and
        // that is a deliberate simplification: two near-identical grey lines
        // under a title read as noise, and neither is worth a second row. The
        // enabled count keeps the more prominent `labelMedium` because it is the
        // line a person acts on; the cache timestamp rides along in the same
        // line rather than being dropped, since "is this stale" is the first
        // question anyone asks of a cached document.
        val subtitle = current?.let { idx ->
            val enabledSummary = stringResource(
                R.string.rules_enabled_summary,
                idx.groups.size - disabled.count { it.startsWith("g:") },
                idx.groups.size,
            )
            val cache = RulesRepository.cacheFile(context)
            val cacheInfo = if (cache.isFile) {
                stringResource(
                    R.string.rules_cache_info,
                    cache.length() / 1024,
                    DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT)
                        .format(Date(cache.lastModified())),
                )
            } else {
                null
            }
            listOfNotNull(enabledSummary, cacheInfo).joinToString(" · ")
        }

        DetourPageHeader(
            title = stringResource(R.string.rules_title),
            subtitle = subtitle,
            actions = {
                // Switching 21 groups on one at a time is the kind of thing that
                // makes a settings screen feel hostile. One action puts every
                // group back, which is also the state most people want after
                // experimenting.
                //
                // Only the disabling direction confirms. Re-enabling is the safe
                // direction — it cannot lose anyone a route they wanted — so it
                // stays immediate. Disabling confirms whenever it would actually
                // switch off at least one group, no matter how few: this used to
                // run silently below a threshold of five, which made it a
                // destructive action with no confirmation precisely when it was
                // hardest to notice. The page shows a switch per group and no
                // total, so nothing on screen says how much a silent "全部关闭"
                // just stopped routing; the dialog is the only place that fact is
                // ever stated, and it now always gets the chance.
                val anyDisabled = disabled.any { it.startsWith("g:") }
                IconButton(
                    onClick = {
                        val groups = index?.groups.orEmpty().map { "g:${it.name}" }.toSet()
                        when {
                            anyDisabled -> {
                                prefs.replaceDisabledRules(emptySet())
                                DetourVpnService.reloadRulesIfRunning()
                            }
                            // isNotEmpty(), not a size threshold: one group is
                            // still a real change to what the kernel routes.
                            groups.isNotEmpty() -> pendingDisableAll = groups
                            // No document, so nothing to disable — a no-op rather
                            // than a dialog that would confirm turning off zero
                            // groups.
                            else -> Unit
                        }
                    },
                ) {
                    Icon(
                        if (anyDisabled) Icons.Filled.Check else Icons.Filled.Clear,
                        contentDescription = stringResource(
                            if (anyDisabled) R.string.rules_enable_all else R.string.rules_disable_all,
                        ),
                    )
                }
                IconButton(
                    onClick = {
                        scope.launch {
                            loading = true
                            index = withContext(Dispatchers.IO) {
                                runCatching { RuleIndex.refresh(context) }.getOrNull()
                            }
                            loading = false
                        }
                    },
                ) {
                    Icon(
                        Icons.Filled.Refresh,
                        contentDescription = stringResource(R.string.rules_refresh),
                    )
                }
            },
        )

        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 20.dp),
            singleLine = true,
            leadingIcon = { Icon(Icons.Filled.Search, contentDescription = null) },
            placeholder = { Text(stringResource(R.string.rules_search_hint)) },
        )

        Spacer(Modifier.height(8.dp))

        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = 20.dp, end = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // One chip per source, generated from the stored list. The old row was
            // three hard-coded values, two of which named the same document, which
            // is how one document came to be displayed as two chips.
            //
            // The chips scroll and the add button does not. A source list is data
            // now, so it can grow past the width of the screen, and a plain row
            // would push the button that adds sources off the edge — the control
            // that fixes an over-long list would be the first casualty of one.
            Row(
                modifier = Modifier
                    .weight(1f)
                    .horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                prefs.ruleSources.forEach { source ->
                    DetourFilterChip(
                        selected = source.id == prefs.ruleSourceId,
                        onClick = { selectSource(source.id) },
                        label = source.labelRes?.let { stringResource(it) } ?: source.label,
                        // `usable` is false for a source with neither an address
                        // nor an imported copy — see RuleSource.usable. No
                        // built-in is in that state since `s302` was deleted, and
                        // an imported source never is, so this guard only ever
                        // hides a row that could not load.
                        enabled = source.usable,
                    )
                }
            }
            // A labelled text button rather than a second icon. Two icon-only
            // buttons would both have to be a plus: `material-icons-core` is the
            // only icon artifact this app ships and it has no document glyph, so
            // the pair would be indistinguishable on screen — the one place where
            // reusing the plus, which is fine on the settings screen's labelled
            // rows, stops being fine.
            TextButton(onClick = { rulesImportPicker.launch(arrayOf("*/*")) }) {
                Text(stringResource(R.string.rules_import_local))
            }
            IconButton(onClick = { showAddSource = true }) {
                Icon(Icons.Filled.Add, contentDescription = stringResource(R.string.rules_add_source))
            }
        }

        Spacer(Modifier.height(8.dp))

        // The verdict filter, on its own line below the sources: it is a lens on
        // the *document*, while the row above chooses which document. Mixing the
        // two into one scrollable row would read as if "只看有问题的" were another
        // source. Left-aligned and inset to the same 20.dp gutter as everything
        // else, so it lines up with the search field and the list.
        Row(
            modifier = Modifier.padding(horizontal = 20.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            DetourFilterChip(
                selected = !problemsOnly,
                onClick = { problemsOnly = false },
                label = stringResource(R.string.rules_filter_all),
            )
            DetourFilterChip(
                selected = problemsOnly,
                onClick = { problemsOnly = true },
                label = stringResource(R.string.rules_filter_problems),
                // Disabled without verdict data. The filter's whole input is the
                // kernel's verdict tables; with no tunnel there is nothing to
                // judge, so an enabled chip could only ever empty the list — a
                // screen saying "no problems" when the truth is "no idea". The
                // state is also cleared when the tunnel stops (see the ipStats
                // effect above), so a disabled chip is never left selected.
                enabled = ipStats != null,
            )
        }

        if (loading) {
            LinearProgressIndicator(Modifier.fillMaxWidth().padding(horizontal = 20.dp))
        }

        if (current == null && !loading) {
            DetourEmptyState(
                title = stringResource(R.string.rules_empty),
                detail = stringResource(R.string.rules_empty_detail),
            )
            return@Column
        }

        // The rows the search alone produced. Held apart from the verdict
        // filter's result because the two ways of reaching an empty list have
        // different causes — and so different fixes; see the empty state below.
        val searched = remember(current, query, expandedGroups, expandedDomains) {
            current?.rows(query, expandedGroups, expandedDomains).orEmpty()
        }

        // The verdict filter is only meaningful while there is a snapshot to read.
        // Gating it here rather than only on the chip keeps the list honest if the
        // two ever fall out of step.
        val problemsOnlyActive = problemsOnly && ipStats != null
        val visible = remember(searched, problemsOnlyActive, ipStats) {
            val stats = ipStats
            if (!problemsOnlyActive || stats == null) {
                searched
            } else {
                // Filter the flattened rows, keeping a group if any of the
                // domains it still claims holds a bad address. `rows` already
                // narrowed a group's `domains` to the search hits, so a group
                // kept here is kept for a reason the user can see once expanded —
                // and a group that is collapsed still survives on the strength of
                // its (unexpanded) domains, which is what lets the filter point
                // at a problem without opening the whole tree.
                searched.filter { node ->
                    when (node) {
                        is RuleIndex.Node.Group -> node.domains.any { domain ->
                            domain.addresses.any { address ->
                                stats.isProblem(domain.domain, address)
                            }
                        }
                        is RuleIndex.Node.Domain -> node.addresses.any { address ->
                            stats.isProblem(node.domain, address)
                        }
                        is RuleIndex.Node.Address -> stats.isProblem(node.domain, node.address)
                    }
                }
            }
        }

        // An empty list is not one state but two, and they must not share a
        // message. "没有匹配的条目" is about the query; the verdict copy is about
        // the filter hiding rows the user already has. Telling someone to change
        // their source when the answer is "turn the filter off" sends them to the
        // wrong screen entirely — which is why `rules_empty_detail` ("换一个数据源…")
        // is deliberately *not* shown on the search path: that hint is only true
        // when there is no document at all (the `current == null` branch above),
        // not when a query simply missed. Search is tested first because it runs
        // first: if the query matched nothing, the filter never had anything to
        // hide.
        if (visible.isEmpty()) {
            if (searched.isEmpty()) {
                DetourEmptyState(title = stringResource(R.string.rules_empty))
            } else {
                DetourEmptyState(
                    title = stringResource(R.string.rules_filter_problems_empty),
                    detail = stringResource(R.string.rules_filter_problems_empty_detail),
                )
            }
            return@Column
        }

        // The glass bar floats *over* the content now instead of living in the
        // Scaffold's `bottomBar` slot, so it no longer takes any space out of
        // this list. A `LazyColumn` only scrolls as far as its content plus its
        // `contentPadding`, so without this the last group (and everything under
        // it) would sit permanently beneath the bar with no way to scroll it
        // into view — a worse bug than the layout it replaced. The value is read
        // once here, in composition, and the sum is written where the padding is
        // declared, so the arithmetic is a plain `Dp` rather than a read buried
        // in a lambda.
        val barClearance = LocalBottomBarClearance.current
        LazyColumn(
            modifier = Modifier.fillMaxSize(),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(
                start = 20.dp,
                end = 20.dp,
                top = 8.dp,
                bottom = 24.dp.plusBarClearance(barClearance),
            ),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            items(visible, key = { it.key }) { node ->
                // A Box carrying the animation, so the three row types below stay
                // untouched. Expanding a group then fades the new rows in and
                // slides the ones under them, instead of cutting to a new list —
                // the change happens under the finger that caused it, and a hard
                // cut reads as if those rows had always been there.
                Box(Modifier.animateItem()) {
                    when (node) {
                        is RuleIndex.Node.Group -> GroupCard(
                            group = node,
                            expanded = node.name in expandedGroups,
                            onToggleExpand = {
                                expandedGroups =
                                    if (node.name in expandedGroups) expandedGroups - node.name
                                    else expandedGroups + node.name
                            },
                            disabled = disabled,
                            onToggle = applySwitch,
                            ipStats = ipStats,
                        )
                        is RuleIndex.Node.Domain -> DomainRow(
                            node = node,
                            expanded = node.key in expandedDomains,
                            onToggleExpand = {
                                expandedDomains =
                                    if (node.key in expandedDomains) expandedDomains - node.key
                                    else expandedDomains + node.key
                            },
                            disabled = disabled,
                            onToggle = applySwitch,
                            ipStats = ipStats,
                        )
                        is RuleIndex.Node.Address ->
                            AddressRow(node, disabled, applySwitch, ipStats, now)
                    }
                }
            }
        }
    }

    if (showAddSource) {
        AddRuleSourceDialog(
            onDismiss = { showAddSource = false },
            onAdd = { source ->
                prefs.addRuleSource(source)
                showAddSource = false
                // Select what was just added. A source the user has to hunt for and
                // tap before it does anything reads as "the add did nothing".
                selectSource(source.id)
            },
        )
    }

    // Also outside the Column, and for the same reason as the two below: an
    // overlay on the whole screen rather than a row in the list.
    importError?.let { reason ->
        DetourAlertDialog(
            onDismissRequest = { importError = null },
            title = stringResource(R.string.rules_import_local),
            text = { Text(stringResource(R.string.rules_import_failed, reason)) },
            // One button. There is no "try again" action to offer — the picker is
            // already the retry — and a second button that only closed the dialog
            // would be the same action spelled twice.
            confirmButton = {
                TextButton(onClick = { importError = null }) {
                    Text(stringResource(R.string.common_confirm))
                }
            },
        )
    }

    // Outside the Column, like the dialog above: it is an overlay on the whole
    // screen, not a row in the list.
    pendingDisableAll?.let { groups ->
        DetourAlertDialog(
            onDismissRequest = { pendingDisableAll = null },
            title = stringResource(R.string.rules_disable_all_confirm_title),
            text = { Text(stringResource(R.string.rules_disable_all_confirm_text, groups.size)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        disableAllGroups(groups)
                        pendingDisableAll = null
                    },
                ) {
                    Text(stringResource(R.string.rules_disable_all_confirm_ok))
                }
            },
            dismissButton = {
                TextButton(onClick = { pendingDisableAll = null }) {
                    Text(stringResource(android.R.string.cancel))
                }
            },
        )
    }
}

@Composable
private fun GroupCard(
    group: RuleIndex.Node.Group,
    expanded: Boolean,
    onToggleExpand: () -> Unit,
    disabled: Set<String>,
    onToggle: (String, Boolean) -> Unit,
    ipStats: Kernel.IpStats?,
) {
    val key = "g:${group.name}"
    DetourCard(style = DetourCardStyle.Glass) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = 16.dp, end = 8.dp, top = 12.dp, bottom = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(
                Modifier
                    .weight(1f)
                    .clickable(onClick = onToggleExpand),
            ) {
                Text(group.name, style = MaterialTheme.typography.titleMedium)
                // Through the resource, not a template string: the counts are the
                // one line here a translator has to see, and the separator is
                // punctuation a locale may want to move.
                Text(
                    stringResource(R.string.rules_group_counts, group.domainCount, group.addressCount),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                // The group-level rollup: the one line that answers "which group
                // is broken" without opening anything. Sits inside the same
                // clickable Column as the title, so tapping it still expands the
                // group rather than doing nothing. Shown only when something is
                // wrong — silence is the healthy state at all three levels.
                if (ipStats != null) {
                    // Walks domains -> addresses and asks the certificate table
                    // about each pair. Affordable because verdictsFor is a map
                    // lookup, not a scan.
                    //
                    // `domain.domain` (the host), not `domain`: the elements here
                    // are Domain nodes, and verdictsFor is keyed by host string.
                    val bad = group.domains.sumOf { domain ->
                        domain.addresses.count { address ->
                            ipStats.verdictsFor(domain.domain)
                                .any { it.ip == address && it.verdict in BAD_VERDICTS }
                        }
                    }
                    if (bad > 0) {
                        // group.addressCount, not a re-sum of domains: the parser
                        // already computed this total, and a second implementation
                        // of the same number is a second chance for it to drift.
                        Text(
                            stringResource(R.string.rules_verdict_group_bad, bad, group.addressCount),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                }
            }
            DetourSwitch(
                checked = key !in disabled,
                onCheckedChange = { on -> onToggle(key, on) },
            )
            IconButton(onClick = onToggleExpand) {
                Icon(
                    if (expanded) Icons.Filled.KeyboardArrowUp else Icons.Filled.KeyboardArrowDown,
                    contentDescription = stringResource(
                        if (expanded) R.string.rules_collapse else R.string.rules_expand,
                    ),
                )
            }
        }
    }
}

@Composable
private fun DomainRow(
    node: RuleIndex.Node.Domain,
    expanded: Boolean,
    onToggleExpand: () -> Unit,
    disabled: Set<String>,
    onToggle: (String, Boolean) -> Unit,
    ipStats: Kernel.IpStats?,
) {
    val key = "d:${node.domain}"
    Row(
        modifier = Modifier
            .fillMaxWidth()
            // The rail sits in the indent gutter, 8.dp left of where the content
            // starts, so the line reads as the edge of the group this domain
            // belongs to. Drawn before the padding, so `drawBehind` covers the
            // whole row including the gutter the line is drawn in.
            .hierarchyRail(4.dp, MaterialTheme.colorScheme.outlineVariant)
            .padding(start = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // The whole left side opens the address list, the same way a group opens
        // its domains: an arrow that is the only target is a target nobody hits.
        Row(
            modifier = Modifier
                .weight(1f)
                .clickable(onClick = onToggleExpand),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                if (expanded) Icons.Filled.KeyboardArrowUp else Icons.Filled.KeyboardArrowDown,
                contentDescription = stringResource(
                    if (expanded) R.string.rules_collapse else R.string.rules_expand,
                ),
                modifier = Modifier.padding(end = 4.dp),
            )
            Column(Modifier.weight(1f)) {
                Text(
                    node.domain,
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    stringResource(R.string.rules_count_addresses, node.addresses.size),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                // A one-line rollup of the worst news below this domain, so a
                // collapsed domain still says "two of these will not work" without
                // being opened. Shown only when something is wrong: a healthy
                // domain gets no line at all, because an all-good row on every
                // domain would drown the one that is not.
                if (ipStats != null) {
                    val verdicts = ipStats.verdictsFor(node.domain)
                    val bad = node.addresses.count { address ->
                        verdicts.any { it.ip == address && it.verdict in BAD_VERDICTS }
                    }
                    if (bad > 0) {
                        Text(
                            if (bad == node.addresses.size) {
                                stringResource(R.string.rules_verdict_domain_all_bad, node.addresses.size)
                            } else {
                                stringResource(R.string.rules_verdict_domain_bad, bad, node.addresses.size)
                            },
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                }
            }
        }
        DetourSwitch(
            checked = key !in disabled,
            onCheckedChange = { on -> onToggle(key, on) },
        )
    }
}

@Composable
private fun AddressRow(
    node: RuleIndex.Node.Address,
    disabled: Set<String>,
    onToggle: (String, Boolean) -> Unit,
    ipStats: Kernel.IpStats?,
    now: Long,
) {
    val key = "a:${node.address}"
    Row(
        modifier = Modifier
            .fillMaxWidth()
            // One level deeper than a domain, so its rail is one indent further
            // in: 16.dp against the domain's 4.dp. The two rails are what show
            // that an address belongs to the domain above it — the bare rows
            // carry no container to say so.
            .hierarchyRail(16.dp, MaterialTheme.colorScheme.outlineVariant)
            .padding(start = 24.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(
                node.address,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                // Monospace so an IPv4 literal reads as one token at a glance —
                // proportional digits make 10.0.0.1 and 1.0.0.10 scan the same — and
                // so the three levels (group title / domain / address) are visibly
                // different kinds of thing rather than three sizes of the same.
                fontFamily = FontFamily.Monospace,
            )
            // The two last-verdict lines, or — with nothing running — a single
            // line that says so. The *position* is kept rather than dropped: a
            // column that vanishes when the tunnel stops reads as a feature this
            // build does not have, and the user has no way to learn it is only
            // waiting for a connection. Naming the reason turns an absence into a
            // temporary state. The values themselves are still withheld — an
            // empty "—" would be a claim about a tunnel that does not exist.
            if (ipStats == null) {
                Text(
                    stringResource(R.string.rules_verdict_offline),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                val stat = ipStats.addressStat(node.address)
                // The test order lives in `selectorVerdict`, shared with the
                // "只看有问题的" filter, so the two can never disagree about which
                // rows are broken.
                val selector = when (selectorVerdict(stat)) {
                    SelectorVerdict.UNPROBED -> stringResource(R.string.rules_verdict_unprobed)
                    SelectorVerdict.SILENT -> stringResource(R.string.rules_verdict_silent)
                    SelectorVerdict.FAILED -> stringResource(R.string.rules_verdict_failed)
                    SelectorVerdict.OK -> stringResource(R.string.rules_verdict_ok)
                }
                // The round trip is an appendage, not a column: it only means
                // anything once there is a measurement, so it appears only then.
                val rtt = stat?.ewmaRttMs?.let { rtt ->
                    " · " + stringResource(R.string.rules_verdict_rtt, rtt.roundToInt())
                }.orEmpty()
                Text(
                    stringResource(R.string.rules_verdict_selector) + " " + selector + rtt,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )

                val cert = ipStats.verdictsFor(node.domain).firstOrNull { it.ip == node.address }
                val certLabel = if (cert == null) {
                    stringResource(R.string.rules_verdict_unprobed)
                } else {
                    when (cert.verdict) {
                        "in_flight" -> stringResource(R.string.rules_verdict_in_flight)
                        "covers" -> stringResource(R.string.rules_verdict_ok)
                        "wrong_certificate" -> stringResource(R.string.rules_verdict_wrong_cert)
                        "unreachable" -> stringResource(R.string.rules_verdict_unreachable)
                        // The set of verdicts belongs to the kernel, not to this
                        // build. A value we have never heard of is shown as itself
                        // rather than crashing or rendering blank — a new kernel
                        // verdict should be legible before the app is updated.
                        else -> cert.verdict
                    }
                }
                val age = cert?.let { " · " + formatAge(now - it.ageMs, now) }.orEmpty()
                Text(
                    stringResource(R.string.rules_verdict_cert) + " " + certLabel + age,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        DetourSwitch(
            checked = key !in disabled,
            onCheckedChange = { on -> onToggle(key, on) },
        )
    }
}
