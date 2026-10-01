package dev.detour.ui

import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Clear
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import dev.detour.BuildConfig
import dev.detour.R
import dev.detour.control.ControlConsole
import dev.detour.core.BuildFlags
import dev.detour.core.DetourVpnService
import dev.detour.core.Kernel
import dev.detour.core.KernelState
import dev.detour.core.Prefs
import dev.detour.core.RuleSource
import dev.detour.core.RulesRepository
import dev.detour.core.SettingsBackup
import dev.detour.core.WallpaperStore
import dev.detour.ui.components.DetourActionRow
import dev.detour.ui.components.DetourAlertDialog
import dev.detour.ui.components.DetourButton
import dev.detour.ui.components.DetourButtonVariant
import dev.detour.ui.components.ConsoleEntry
import dev.detour.ui.components.DetourChoiceRow
import dev.detour.ui.components.DetourConsoleRow
import dev.detour.ui.components.DetourDivider
import dev.detour.ui.components.DetourKeyValueRow
import dev.detour.ui.components.LocalBottomBarClearance
import dev.detour.ui.components.DetourSectionCard
import dev.detour.ui.components.DetourSegmentedRow
import dev.detour.ui.components.DetourSliderRow
import dev.detour.ui.components.DetourStepperRow
import dev.detour.ui.components.DetourToggleRow
import dev.detour.ui.theme.supportsDynamicColor
import java.io.IOException
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * The five groups, in the order they are shown.
 *
 * An enum rather than five hand-written cards because the search has to walk the
 * groups in a fixed order and ask each one for its visible rows; with the groups
 * as data the renderer is a loop instead of five near-identical blocks, and
 * adding a group cannot forget to update the renderer.
 *
 * [DATA] is declared last on purpose. Every group before it changes a setting
 * this app owns; the data group's rows either hand a file to, or take one from,
 * another app, or leave for another screen. Putting it last keeps the settings
 * proper together and reads the group as what it is — the exit.
 */
private enum class SettingsSection(@StringRes val titleRes: Int) {
    CONNECTION(R.string.settings_section_connection),
    LOGS(R.string.settings_section_logs),
    APPEARANCE(R.string.settings_section_appearance),
    ADVANCED(R.string.settings_section_advanced),
    DATA(R.string.settings_section_data),
}

/**
 * One setting, as data.
 *
 * [searchText] is everything about this row a search may match: the label, and
 * its hint where it has one. Resolving those strings here — rather than having
 * the filter reach back into resources — is what lets "no match" be knowable
 * without keeping a second copy of every label in step with the first.
 */
private class SettingsRow(
    val section: SettingsSection,
    val searchText: List<String>,
    val content: @Composable () -> Unit,
)

/**
 * How many executed commands the console keeps on screen.
 *
 * A console is read from the bottom, so the oldest entries are the ones to drop.
 * The cap exists because the transcript is unbounded work the user never asked
 * for: `dump` alone is a few kilobytes of pretty-printed JSON, and an afternoon
 * of tapping would otherwise grow a list this screen rebuilds on every
 * keystroke elsewhere.
 */
private const val CONSOLE_MAX_ENTRIES = 20

/**
 * Settings, in five groups that match the five kinds of decision.
 *
 * Grouped by *what the setting is about* rather than by how often it is used. A
 * flat list sorted by frequency would put the proxy port next to the dark mode
 * switch, and neither belongs near the other.
 *
 * The advanced group is last and always expanded, with no fold on its heading.
 * It used to be a collapsible card that opened collapsed, and that made the
 * 开发者视图 switch look broken: the group holds the developer-only rows — the
 * 内核命令行 console among them — so flipping the switch changed nothing on screen
 * until the heading was tapped again, and "I turned it on and nothing happened"
 * is indistinguishable from "the switch does not work". The owner hit exactly
 * this on a real device (2026-09-27) and had the fold removed. Dropping it also
 * simplifies the search: with no fold there is no "force it open while a query is
 * active" special case, because nothing can hide a match. The group is still
 * last, and still clearly marked as the place where a change can make the tunnel
 * worse — it is not hidden, because hiding settings makes them unfindable.
 *
 * Removing the fold was necessary but not sufficient, and the owner said so twice.
 * With the fold gone the console row existed, but the 开发者视图 switch that reveals
 * it was still two and a half screens away, in 日志与诊断 — so tapping it still
 * changed nothing on screen. The switch was moved to sit directly above 内核版本,
 * next to what it reveals. The general rule is recorded there.
 *
 * The body is built as data ([SettingsRow]) and filtered once rather than a
 * hand-written card per section: a search has to be able to answer "nothing
 * matched" and to know which rows to keep, and doing that over layout code would
 * mean every label had a second copy to keep in step.
 *
 * Every row now comes from `ui/components`. The six row composables that used to
 * live at the bottom of this file were the app's duplication problem in
 * miniature, and they are gone.
 *
 * [onOpenAbout] is a callback rather than a navigation call made here, because
 * this screen is not the one that owns the destination stack. The parameter has
 * no default: a default would let a call site forget to pass it and silently lose
 * the 关于 row's only effect, which is the "control that does nothing" defect this
 * file has already been burned by.
 */
@Composable
fun SettingsScreen(onOpenAbout: () -> Unit) {
    val context = LocalContext.current
    val prefs = Prefs.of(context)
    val status by KernelState.status.collectAsState()
    // `pending` is a question about the *running* engine, so it is only asked while
    // one is running — and only in VPN mode. Proxy mode takes no kernel settings at
    // all (`Kernel.startProxy` has no config parameter), so a reconnect there would
    // apply nothing and the banner would be a lie.
    val pending = status.phase == KernelState.Phase.ON &&
        status.mode == KernelState.Mode.VPN &&
        prefs.kernelSettingsPending()
    var showAddSource by remember { mutableStateOf(false) }
    var showRestoreConfirm by remember { mutableStateOf(false) }
    var showImportConfirm by remember { mutableStateOf(false) }
    var query by remember { mutableStateOf("") }

    // The console's state, hoisted here rather than kept inside the row's `add`
    // lambda. The row list is rebuilt on every recomposition and reordered by the
    // search, so a `remember` inside that lambda would be positionally unstable —
    // the typed line and the transcript would be lost the first time the query
    // moved the row. `rememberSaveable` for the input so a rotation keeps it.
    var consoleInput by rememberSaveable { mutableStateOf("") }
    var consoleEntries by remember { mutableStateOf(emptyList<ConsoleEntry>()) }
    var consoleRunning by remember { mutableStateOf(false) }
    val consoleScope = rememberCoroutineScope()

    // The photo picker, hoisted above `buildList` for the same reason the console's
    // state is: the row list is rebuilt and reordered by the search, so a
    // `remember` inside a row lambda would be positionally unstable and the
    // launcher would be re-registered as rows moved.
    //
    // `PickVisualMedia` is the *system* photo picker. It needs no runtime
    // permission, which is what makes it the right choice here: this app declares
    // no media or storage permission at all and should not start now.
    var wallpaperError by remember { mutableStateOf<String?>(null) }
    val wallpaperScope = rememberCoroutineScope()
    val wallpaperFailed = stringResource(R.string.settings_wallpaper_failed)
    val wallpaperPicker = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        // A null uri is a cancelled pick, not a failure: leave the current
        // wallpaper alone rather than treating "user changed their mind" as an error.
        if (uri != null) {
            wallpaperScope.launch {
                wallpaperError = if (WallpaperStore.save(context, uri) == null) wallpaperFailed else null
            }
        }
    }

    // The two backup pickers. Like the photo picker above, both are the Storage
    // Access Framework: the chooser runs in another process and hands back a URI
    // this app may read or write for exactly this one operation, which is what
    // lets the app declare no storage permission at all. The obvious-looking
    // alternative — a path under `getExternalFilesDir` — would put the file where
    // the user cannot choose and cannot find it without a second app.
    //
    // The scope is hoisted for the same reason the wallpaper picker's is: the row
    // list is rebuilt and reordered by the search, so a `remember` inside a row
    // lambda would be positionally unstable.
    val backupScope = rememberCoroutineScope()
    val exportPicker = rememberLauncherForActivityResult(
        ActivityResultContracts.CreateDocument("application/json"),
    ) { uri ->
        // A null uri is a cancelled pick, not a failure: the user changed their
        // mind and there is nothing to report. Toasting here would scold them for
        // it, which is the same mistake the wallpaper picker above avoids.
        if (uri != null) {
            backupScope.launch {
                // Off the main thread. The file is small, but the chooser can hand
                // back a URI on a slow provider — a cloud document — where a
                // main-thread write would ANR before it returned.
                val outcome = withContext(Dispatchers.IO) {
                    runCatching {
                        context.contentResolver.openOutputStream(uri)?.use { stream ->
                            stream.write(SettingsBackup.export(context).toByteArray())
                        } ?: throw IOException("无法打开输出流")
                    }
                }
                // `stringResource` is not callable from a click handler, so the
                // toast text comes from the context directly.
                val message = outcome.fold(
                    onSuccess = { context.getString(R.string.settings_export_done) },
                    onFailure = {
                        context.getString(R.string.settings_export_failed, it.message ?: "未知错误")
                    },
                )
                Toast.makeText(context, message, Toast.LENGTH_SHORT).show()
            }
        }
    }
    val importPicker = rememberLauncherForActivityResult(
        // `*/*` alongside the JSON type: some file pickers hide a `.json` file when
        // only `application/json` is requested, and the content is validated after
        // reading anyway, so the narrower filter would only lose files the user is
        // entitled to pick. No persistable grant is requested — the stream is read
        // immediately in this callback, while the temporary grant is still live.
        ActivityResultContracts.OpenDocument(),
    ) { uri ->
                // Cancelled pick: nothing to report, same as the export above.
                if (uri != null) {
                    backupScope.launch {
                        // Off the main thread for the same reason as the export: a slow
                        // provider would otherwise block the main thread long enough to ANR.
                        val outcome = withContext(Dispatchers.IO) {
                            runCatching {
                                context.contentResolver.openInputStream(uri)?.use { stream ->
                                    stream.bufferedReader().readText()
                                } ?: throw IOException("无法打开输入流")
                            }
                        }
                        val message = outcome.fold(
                            onSuccess = { json ->
                                val result = SettingsBackup.import(context, json)
                                when (result.failure) {
                                    SettingsBackup.ImportResult.Failure.NOT_JSON ->
                                        context.getString(R.string.settings_import_bad_json)
                                    SettingsBackup.ImportResult.Failure.NOT_OURS ->
                                        context.getString(R.string.settings_import_not_ours)
                                    SettingsBackup.ImportResult.Failure.TOO_NEW ->
                                        context.getString(R.string.settings_import_too_new)
                                    null -> {
                                        // An import can change the rule source, and the
                                        // kernel keeps serving the old table until it is
                                        // told to reload — the same two steps the restore
                                        // path and every other source change on this screen
                                        // performs.
                                        RulesRepository.invalidate(context)
                                        DetourVpnService.reloadRulesIfRunning()
                                        if (result.ignored == 0) {
                                            context.getString(R.string.settings_import_done, result.applied)
                                        } else {
                                            context.getString(
                                                R.string.settings_import_done_ignored,
                                                result.applied,
                                                result.ignored,
                                            )
                                        }
                                    }
                                }
                            },
                            onFailure = {
                                context.getString(R.string.settings_import_read_failed, it.message ?: "未知错误")
                            },
                        )
                        Toast.makeText(context, message, Toast.LENGTH_SHORT).show()
                    }
                }
            }

    // The rule-file picker, for importing a hosts document from anywhere the user
    // can reach. The same Storage Access Framework contract as the settings import
    // above and for the same reason: no storage permission, and the stream is read
    // in this callback while the temporary grant is still live.
    //
    // `*/*` because a hosts file has no registered MIME type — different providers
    // report `text/plain`, `application/octet-stream`, or nothing at all — and the
    // content is validated after reading regardless, so a narrower filter could
    // only hide files the user is entitled to pick.
    //
    // Everything after "the user picked a file" is `RulesRepository.importLocal`,
    // which is also what the rules screen calls. Validating, copying, adding,
    // selecting, dropping the stale cache and reloading a running tunnel are six
    // steps that must not be able to differ between the two screens.
    val rulesImportPicker = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocument(),
    ) { uri ->
        // Cancelled pick: nothing to report, as with the three pickers above.
        if (uri != null) {
            backupScope.launch {
                val message = withContext(Dispatchers.IO) {
                    when (val outcome = RulesRepository.importLocal(context, uri)) {
                        is RulesRepository.LocalImport.Added ->
                            context.getString(R.string.rules_import_done, outcome.source.label)
                        is RulesRepository.LocalImport.AlreadyPresent ->
                            context.getString(R.string.rules_import_already, outcome.source.label)
                        is RulesRepository.LocalImport.Failed ->
                            context.getString(R.string.rules_import_failed, outcome.reason)
                    }
                }
                Toast.makeText(context, message, Toast.LENGTH_SHORT).show()
            }
        }
    }

    // Every setting as a row, in the order it is shown. The content lambdas read
    // `prefs` live, so a change recomposes the row exactly as it did when the
    // layout was written out by hand.
    val rows: List<SettingsRow> = buildList {
        // --- connection -------------------------------------------------------

        // The proxy port only means anything in proxy mode, which is
        // hidden under [BuildFlags.TUN_ONLY]. The row and the divider that
        // separated it from the source selector go together, or the card
        // would open with a stray divider at the top.
        if (!BuildFlags.TUN_ONLY) {
            val proxyLabel = stringResource(R.string.settings_proxy_port)
            val proxyHint = stringResource(R.string.settings_proxy_port_hint, prefs.proxyPort)
            add(
                SettingsRow(SettingsSection.CONNECTION, listOf(proxyLabel, proxyHint)) {
                    DetourStepperRow(
                        label = proxyLabel,
                        value = prefs.proxyPort.toString(),
                        hint = proxyHint,
                        onDecrease = { prefs.updateProxyPort(prefs.proxyPort - 1) },
                        onIncrease = { prefs.updateProxyPort(prefs.proxyPort + 1) },
                    )
                },
            )
        }

        // A list, not a segmented row. The set of sources is data now (see
        // RuleSource) and can hold a retired entry and user-added ones —
        // neither of which a fixed three-way control can express. The
        // selection action is the same one the rules screen uses: changing
        // the source changes the document the kernel runs, so the cache and
        // any running tunnel have to be told, or the control moves and
        // nothing else does.
        //
        // Heading, list and "add" are one row here rather than three. They are
        // one control: splitting them would let a search surface a source with
        // no heading, or a heading with no source, and the dividers between them
        // would appear where the original had none.
        val sourceHeading = stringResource(R.string.rules_source)
        val addSourceLabel = stringResource(R.string.rules_add_source)
        val importLocalLabel = stringResource(R.string.rules_import_local)
        val localSourceLabel = stringResource(R.string.rules_source_local)
        add(
            SettingsRow(
                section = SettingsSection.CONNECTION,
                searchText = buildList {
                    add(sourceHeading)
                    add(addSourceLabel)
                    add(importLocalLabel)
                    prefs.ruleSources.forEach { source ->
                        add(source.labelRes?.let { stringResource(it) } ?: source.label)
                        if (source.usable) add(source.url)
                        if (source.isLocal) add(localSourceLabel)
                    }
                },
            ) {
                Text(
                    sourceHeading,
                    modifier = Modifier.padding(start = 16.dp, top = 12.dp),
                    style = MaterialTheme.typography.bodyLarge,
                )
                prefs.ruleSources.forEach { source ->
                    RuleSourceRow(
                        source = source,
                        selected = source.id == prefs.ruleSourceId,
                        onSelect = {
                            prefs.updateRuleSource(source.id)
                            RulesRepository.invalidate(context)
                            DetourVpnService.reloadRulesIfRunning()
                        },
                        onDelete = {
                            // Same two steps as selecting: the source list changed,
                            // so the next load has to reflect it and a running
                            // tunnel has to be handed the result. Removing the
                            // *selected* source makes this load a different
                            // document; removing any other is a no-op for the
                            // tunnel, and the refetch it costs is the price of not
                            // making the two paths differ in a way a caller cannot
                            // see.
                            prefs.removeRuleSource(source.id)
                            RulesRepository.invalidate(context)
                            DetourVpnService.reloadRulesIfRunning()
                        },
                    )
                }
                DetourDivider()
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { showAddSource = true }
                        .padding(horizontal = 16.dp, vertical = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(Icons.Filled.Add, contentDescription = null)
                    Spacer(Modifier.width(12.dp))
                    Text(
                        addSourceLabel,
                        style = MaterialTheme.typography.bodyLarge,
                    )
                }
                DetourDivider()
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { rulesImportPicker.launch(arrayOf("*/*")) }
                        .padding(horizontal = 16.dp, vertical = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    // `Icons.Filled.Add` a second time, and that is a decision
                    // rather than an oversight. `material-icons-core` is the only
                    // icon artifact this app ships — the extended one is
                    // deliberately not pulled in, see the build file — and it
                    // carries no document or folder glyph. Of the ones it does
                    // carry, `List` is already the rules tab's own icon and
                    // `ExitToApp` points the opposite way; the plus is honest,
                    // because this action *is* "add a source", only by file
                    // instead of by address. The label is what separates the rows.
                    Icon(Icons.Filled.Add, contentDescription = null)
                    Spacer(Modifier.width(12.dp))
                    Text(
                        importLocalLabel,
                        style = MaterialTheme.typography.bodyLarge,
                    )
                }
            },
        )

        val refreshLabel = stringResource(R.string.settings_refresh_interval)
        add(
            SettingsRow(SettingsSection.CONNECTION, listOf(refreshLabel)) {
                DetourStepperRow(
                    label = refreshLabel,
                    value = "${prefs.refreshHours} h",
                    onDecrease = { prefs.updateRefreshHours((prefs.refreshHours - 1).coerceAtLeast(1)) },
                    onIncrease = { prefs.updateRefreshHours((prefs.refreshHours + 1).coerceAtMost(72)) },
                )
            },
        )

        val offlineLabel = stringResource(R.string.settings_offline)
        val offlineHint = stringResource(R.string.settings_offline_hint)
        add(
            SettingsRow(SettingsSection.CONNECTION, listOf(offlineLabel, offlineHint)) {
                DetourToggleRow(
                    label = offlineLabel,
                    hint = offlineHint,
                    checked = prefs.offline,
                    onChange = { prefs.updateOffline(it) },
                )
            },
        )

        val autoConnectLabel = stringResource(R.string.settings_auto_connect)
        val autoConnectHint = stringResource(R.string.settings_auto_connect_hint)
        add(
            SettingsRow(SettingsSection.CONNECTION, listOf(autoConnectLabel, autoConnectHint)) {
                DetourToggleRow(
                    label = autoConnectLabel,
                    hint = autoConnectHint,
                    checked = prefs.autoConnect,
                    onChange = { prefs.updateAutoConnect(it) },
                )
            },
        )

        // --- logs -------------------------------------------------------------

        val statsLabel = stringResource(R.string.settings_stats_interval)
        add(
            SettingsRow(SettingsSection.LOGS, listOf(statsLabel)) {
                DetourStepperRow(
                    label = statsLabel,
                    value = "${prefs.statsIntervalSeconds} s",
                    onDecrease = { prefs.updateStatsInterval((prefs.statsIntervalSeconds - 1).coerceAtLeast(1)) },
                    onIncrease = { prefs.updateStatsInterval((prefs.statsIntervalSeconds + 1).coerceAtMost(60)) },
                )
            },
        )

        val logArchiveLabel = stringResource(R.string.settings_log_archive)
        val logArchiveHint = stringResource(R.string.settings_log_archive_hint)
        add(
            SettingsRow(SettingsSection.LOGS, listOf(logArchiveLabel, logArchiveHint)) {
                DetourToggleRow(
                    label = logArchiveLabel,
                    hint = logArchiveHint,
                    checked = prefs.logArchive,
                    onChange = { prefs.updateLogArchive(it) },
                )
            },
        )

        // 开发者视图 used to live here, in 日志与诊断. It is now the row directly
        // above 内核版本 in the advanced card, because its only effect on this
        // screen is the console below that row. See the note there.

        // --- appearance -------------------------------------------------------

        val dynColorLabel = stringResource(R.string.settings_dynamic_color)
        // Say so rather than offering a switch that does nothing:
        // below Android 12 there is no palette to derive from the
        // wallpaper.
        val dynColorHint = if (supportsDynamicColor()) {
            stringResource(R.string.settings_dynamic_color_hint)
        } else {
            stringResource(R.string.settings_dynamic_color_unsupported)
        }
        add(
            SettingsRow(SettingsSection.APPEARANCE, listOf(dynColorLabel, dynColorHint)) {
                DetourToggleRow(
                    label = dynColorLabel,
                    hint = dynColorHint,
                    checked = prefs.dynamicColor && supportsDynamicColor(),
                    enabled = supportsDynamicColor(),
                    onChange = { prefs.updateDynamicColor(it) },
                )
            },
        )

        val darkModeLabel = stringResource(R.string.settings_dark_mode)
        add(
            SettingsRow(SettingsSection.APPEARANCE, listOf(darkModeLabel)) {
                DetourSegmentedRow(
                    label = darkModeLabel,
                    options = listOf(
                        "follow" to R.string.settings_dark_follow,
                        "always" to R.string.settings_dark_always,
                        "never" to R.string.settings_dark_never,
                    ),
                    selected = prefs.darkMode,
                    onSelect = { prefs.updateDarkMode(it) },
                )
            },
        )

        // The palette is a separate decision from dark mode: "follow the
        // wallpaper" and "always dark" are not alternatives to each
        // other, and offering them as one control would make both harder
        // to find.
        val themeColorLabel = stringResource(R.string.settings_theme_color)
        add(
            SettingsRow(SettingsSection.APPEARANCE, listOf(themeColorLabel)) {
                DetourChoiceRow(
                    label = themeColorLabel,
                    options = listOf(
                        "dynamic" to R.string.settings_theme_color_dynamic,
                        "brand" to R.string.settings_theme_color_brand,
                        "green" to R.string.settings_theme_color_green,
                        "orange" to R.string.settings_theme_color_orange,
                        "violet" to R.string.settings_theme_color_violet,
                    ),
                    selected = prefs.themeColor,
                    onSelect = { prefs.updateThemeColor(it) },
                )
            },
        )

        val cornerLabel = stringResource(R.string.settings_corner_style)
        add(
            SettingsRow(SettingsSection.APPEARANCE, listOf(cornerLabel)) {
                DetourSegmentedRow(
                    label = cornerLabel,
                    options = listOf(
                        "small" to R.string.settings_corner_small,
                        "medium" to R.string.settings_corner_medium,
                        "large" to R.string.settings_corner_large,
                    ),
                    selected = prefs.cornerStyle,
                    onSelect = { prefs.updateCornerStyle(it) },
                )
            },
        )

        // One switch, because the material is one idea. It was two switches
        // (液态玻璃 / 磨砂材质) until the owner merged them (2026-10-01): two master
        // switches for a single visual effect read as two features, and the one
        // combination only two switches could express — refraction without blur —
        // is not something this section ever explained. The merge is not free:
        // the switch turns on both halves, so the blur, which is the expensive
        // half, can no longer be left off. `resolveDetourGlass` records that.
        //
        // The old section heading 玻璃效果 is gone rather than kept above this
        // row: the row's own label is now 玻璃效果, and a heading with the same
        // words one line above the switch is a duplicate that also makes search
        // return two rows for one control.
        val glassLabel = stringResource(R.string.settings_glass)
        val glassHint = stringResource(R.string.settings_glass_hint)
        add(
            SettingsRow(SettingsSection.APPEARANCE, listOf(glassLabel, glassHint)) {
                DetourToggleRow(
                    label = glassLabel,
                    hint = glassHint,
                    checked = prefs.glassEnabled,
                    onChange = { prefs.updateGlassEnabled(it) },
                )
            },
        )

        // The numeric knobs are hidden until the switch is on, and that is the
        // same principle the wallpaper scrim below states in full: a control that
        // cannot have an effect must not be shown. With the switch off,
        // `resolveDetourGlass` ignores every one of these numbers, so showing them
        // would offer five handles connected to nothing — the "the control does
        // nothing" defect this project has already paid for twice. All five are
        // gated by the one switch now, rather than each by "its own" switch,
        // because the one switch turns on both halves and so all five numbers
        // reach the picture.
        //
        // A hint row used to sit here saying the numbers apply only while "the
        // corresponding switch" was on. It is deleted rather than reworded: the
        // sliders only exist while the switch is on, so that sentence could only
        // ever be read next to a switch that is already on, which makes it a
        // tautology pointing at nothing.
        //
        // Their defaults are the previous preset's numbers, so the moment a
        // slider appears the picture is already the one the switch turned on —
        // see `Prefs` for why that matters.
        if (prefs.glassEnabled) {
            val glassBlurLabel = stringResource(R.string.settings_glass_blur)
            add(
                SettingsRow(SettingsSection.APPEARANCE, listOf(glassBlurLabel)) {
                    DetourSliderRow(
                        label = glassBlurLabel,
                        value = prefs.glassBlur.toFloat(),
                        range = 0f..100f,
                        display = "${prefs.glassBlur}dp",
                        onChange = { prefs.updateGlassBlur(it.toInt()) },
                    )
                },
            )
            val glassTintLabel = stringResource(R.string.settings_glass_tint)
            add(
                SettingsRow(SettingsSection.APPEARANCE, listOf(glassTintLabel)) {
                    DetourSliderRow(
                        label = glassTintLabel,
                        value = prefs.glassTint.toFloat(),
                        range = 0f..100f,
                        display = "${prefs.glassTint}%",
                        onChange = { prefs.updateGlassTint(it.toInt()) },
                    )
                },
            )
            val glassLensLabel = stringResource(R.string.settings_glass_lens)
            add(
                SettingsRow(SettingsSection.APPEARANCE, listOf(glassLensLabel)) {
                    DetourSliderRow(
                        label = glassLensLabel,
                        value = prefs.glassLens.toFloat(),
                        range = 0f..150f,
                        display = "${prefs.glassLens}dp",
                        onChange = { prefs.updateGlassLens(it.toInt()) },
                    )
                },
            )
            val glassHighlightLabel = stringResource(R.string.settings_glass_highlight)
            add(
                SettingsRow(SettingsSection.APPEARANCE, listOf(glassHighlightLabel)) {
                    DetourSliderRow(
                        label = glassHighlightLabel,
                        value = prefs.glassHighlight.toFloat(),
                        range = 0f..100f,
                        display = "${prefs.glassHighlight}%",
                        onChange = { prefs.updateGlassHighlight(it.toInt()) },
                    )
                },
            )
            val glassBorderLabel = stringResource(R.string.settings_glass_border)
            add(
                SettingsRow(SettingsSection.APPEARANCE, listOf(glassBorderLabel)) {
                    DetourSliderRow(
                        label = glassBorderLabel,
                        value = prefs.glassBorder.toFloat(),
                        range = 0f..100f,
                        display = "${prefs.glassBorder}%",
                        onChange = { prefs.updateGlassBorder(it.toInt()) },
                    )
                },
            )
        }

        val wallpaperLabel = stringResource(R.string.settings_wallpaper)
        val wallpaperHint = stringResource(R.string.settings_wallpaper_hint)
        // Read once, so the two rows below agree on the same frame and the labels
        // cannot flicker between "选择图片" and "更换" mid-recomposition.
        val hasWallpaper = prefs.wallpaper.isNotBlank()
        add(
            SettingsRow(SettingsSection.APPEARANCE, listOf(wallpaperLabel, wallpaperHint)) {
                DetourActionRow(
                    label = wallpaperLabel,
                    // The error replaces the hint *on screen* but not in `keys` above:
                    // search must keep finding the row by its stable description, or a
                    // failed pick would make the row unfindable until it was cleared.
                    hint = wallpaperError ?: wallpaperHint,
                    actionLabel = stringResource(
                        if (hasWallpaper) R.string.settings_wallpaper_change
                        else R.string.settings_wallpaper_pick,
                    ),
                    onAction = {
                        wallpaperError = null
                        wallpaperPicker.launch(
                            PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                        )
                    },
                    secondaryLabel = if (hasWallpaper) {
                        stringResource(R.string.settings_wallpaper_clear)
                    } else {
                        null
                    },
                    onSecondary = if (hasWallpaper) {
                        { WallpaperStore.clear(context) }
                    } else {
                        null
                    },
                )
            },
        )

        // The scrim slider is hidden until there is a wallpaper, and that is the point
        // rather than a nicety: a scrim with no wallpaper behind it changes nothing on
        // screen, which is precisely the "the control does nothing" defect this project
        // has already been burned by twice. A control that cannot have an effect must
        // not be shown.
        if (hasWallpaper) {
            val scrimLabel = stringResource(R.string.settings_wallpaper_scrim)
            add(
                SettingsRow(SettingsSection.APPEARANCE, listOf(scrimLabel)) {
                    DetourSliderRow(
                        label = scrimLabel,
                        value = prefs.wallpaperScrim.toFloat(),
                        range = 0f..100f,
                        display = "${prefs.wallpaperScrim}%",
                        onChange = { prefs.updateWallpaperScrim(it.toInt()) },
                    )
                },
            )
        }

        val fontLabel = stringResource(R.string.settings_font_scale)
        add(
            SettingsRow(SettingsSection.APPEARANCE, listOf(fontLabel)) {
                DetourSliderRow(
                    label = fontLabel,
                    value = prefs.fontScale,
                    range = 0.85f..1.3f,
                    display = "${(prefs.fontScale * 100).toInt()}%",
                    onChange = { prefs.updateFontScale(it) },
                )
            },
        )

        val homeRateLabel = stringResource(R.string.settings_home_show_rate)
        val homeRateHint = stringResource(R.string.settings_home_show_rate_hint)
        add(
            SettingsRow(SettingsSection.APPEARANCE, listOf(homeRateLabel, homeRateHint)) {
                DetourToggleRow(
                    label = homeRateLabel,
                    hint = homeRateHint,
                    checked = prefs.homeShowRate,
                    onChange = { prefs.updateHomeShowRate(it) },
                )
            },
        )

        val confirmLabel = stringResource(R.string.settings_confirm_disconnect)
        val confirmHint = stringResource(R.string.settings_confirm_disconnect_hint)
        add(
            SettingsRow(SettingsSection.APPEARANCE, listOf(confirmLabel, confirmHint)) {
                DetourToggleRow(
                    label = confirmLabel,
                    hint = confirmHint,
                    checked = prefs.confirmDisconnect,
                    onChange = { prefs.updateConfirmDisconnect(it) },
                )
            },
        )

        // --- advanced ---------------------------------------------------------

        val maxCandLabel = stringResource(R.string.settings_max_candidates)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(maxCandLabel)) {
                DetourStepperRow(
                    label = maxCandLabel,
                    value = prefs.maxCandidates.toString(),
                    onDecrease = { prefs.updateMaxCandidates((prefs.maxCandidates - 1).coerceAtLeast(1)) },
                    onIncrease = { prefs.updateMaxCandidates((prefs.maxCandidates + 1).coerceAtMost(64)) },
                )
            },
        )

        val staggerLabel = stringResource(R.string.settings_connect_stagger)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(staggerLabel)) {
                DetourStepperRow(
                    label = staggerLabel,
                    value = "${prefs.connectStaggerMillis} ms",
                    onDecrease = { prefs.updateConnectStagger((prefs.connectStaggerMillis - 50).coerceAtLeast(0)) },
                    onIncrease = { prefs.updateConnectStagger((prefs.connectStaggerMillis + 50).coerceAtMost(2000)) },
                )
            },
        )

        val raceWidthLabel = stringResource(R.string.settings_race_width)
        val raceWidthHint = stringResource(R.string.settings_race_width_hint)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(raceWidthLabel, raceWidthHint)) {
                DetourStepperRow(
                    label = raceWidthLabel,
                    value = prefs.raceWidth.toString(),
                    hint = raceWidthHint,
                    onDecrease = { prefs.updateRaceWidth(prefs.raceWidth - 1) },
                    onIncrease = { prefs.updateRaceWidth(prefs.raceWidth + 1) },
                )
            },
        )

        val raceLaunchLabel = stringResource(R.string.settings_race_launch)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(raceLaunchLabel)) {
                DetourStepperRow(
                    label = raceLaunchLabel,
                    value = "${prefs.raceLaunchMillis} ms",
                    onDecrease = { prefs.updateRaceLaunch((prefs.raceLaunchMillis - 50).coerceAtLeast(0)) },
                    onIncrease = { prefs.updateRaceLaunch((prefs.raceLaunchMillis + 50).coerceAtMost(1000)) },
                )
            },
        )

        val maxDialingLabel = stringResource(R.string.settings_max_dialing)
        val maxDialingHint = stringResource(R.string.settings_max_dialing_hint)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(maxDialingLabel, maxDialingHint)) {
                DetourStepperRow(
                    label = maxDialingLabel,
                    value = prefs.maxDialing.toString(),
                    hint = maxDialingHint,
                    onDecrease = { prefs.updateMaxDialing(prefs.maxDialing - 16) },
                    onIncrease = { prefs.updateMaxDialing(prefs.maxDialing + 16) },
                )
            },
        )

        val cooldownLabel = stringResource(R.string.settings_failure_cooldown)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(cooldownLabel)) {
                DetourSliderRow(
                    label = cooldownLabel,
                    value = prefs.failureCooldownSeconds.toFloat(),
                    range = 5f..600f,
                    display = "${prefs.failureCooldownSeconds} s",
                    onChange = { prefs.updateFailureCooldown(it.toInt()) },
                )
            },
        )

        val dialNamesLabel = stringResource(R.string.settings_dial_names)
        val dialNamesHint = stringResource(R.string.settings_dial_names_hint)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(dialNamesLabel, dialNamesHint)) {
                DetourToggleRow(
                    label = dialNamesLabel,
                    hint = dialNamesHint,
                    checked = prefs.dialNames,
                    onChange = { prefs.updateDialNames(it) },
                )
            },
        )

        // Every row in this card reaches the kernel. All of them are keys
        // in `Prefs.kernelSettingsJson()`, which is the document the
        // engine is built from — there is no line inside this card
        // between "kernel settings" and "the shell's business". An
        // earlier comment here claimed there was, and it was wrong:
        // max_candidates, connect_stagger, race_width, race_launch,
        // max_dialing, failure_cooldown and dial_names are all in that
        // document too.
        //
        // What is special about the MTU below is that it has a second
        // consumer outside the kernel: it also sizes the TUN interface
        // (`DetourVpnService.establish`). Nothing else here does.
        val mtuLabel = stringResource(R.string.settings_mtu)
        val mtuHint = stringResource(R.string.settings_mtu_hint)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(mtuLabel, mtuHint)) {
                DetourStepperRow(
                    label = mtuLabel,
                    value = prefs.mtu.toString(),
                    hint = mtuHint,
                    onDecrease = { prefs.updateMtu(prefs.mtu - 100) },
                    onIncrease = { prefs.updateMtu(prefs.mtu + 100) },
                )
            },
        )

        val timeoutLabel = stringResource(R.string.settings_connect_timeout)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(timeoutLabel)) {
                DetourSliderRow(
                    label = timeoutLabel,
                    value = prefs.connectTimeoutSeconds.toFloat(),
                    range = 1f..60f,
                    display = "${prefs.connectTimeoutSeconds} s",
                    onChange = { prefs.updateConnectTimeout(it.toInt()) },
                )
            },
        )

        val tcpIdleLabel = stringResource(R.string.settings_tcp_idle)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(tcpIdleLabel)) {
                DetourSliderRow(
                    label = tcpIdleLabel,
                    value = prefs.tcpIdleSeconds.toFloat(),
                    range = 30f..1800f,
                    display = "${prefs.tcpIdleSeconds} s",
                    onChange = { prefs.updateTcpIdle(it.toInt()) },
                )
            },
        )

        val udpIdleLabel = stringResource(R.string.settings_udp_idle)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(udpIdleLabel)) {
                DetourSliderRow(
                    label = udpIdleLabel,
                    value = prefs.udpIdleSeconds.toFloat(),
                    range = 10f..300f,
                    display = "${prefs.udpIdleSeconds} s",
                    onChange = { prefs.updateUdpIdle(it.toInt()) },
                )
            },
        )

        val maxTcpLabel = stringResource(R.string.settings_max_tcp_flows)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(maxTcpLabel)) {
                DetourStepperRow(
                    label = maxTcpLabel,
                    value = prefs.maxTcpFlows.toString(),
                    onDecrease = { prefs.updateMaxTcpFlows(prefs.maxTcpFlows - 64) },
                    onIncrease = { prefs.updateMaxTcpFlows(prefs.maxTcpFlows + 64) },
                )
            },
        )

        val maxUdpLabel = stringResource(R.string.settings_max_udp_flows)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(maxUdpLabel)) {
                DetourStepperRow(
                    label = maxUdpLabel,
                    value = prefs.maxUdpFlows.toString(),
                    onDecrease = { prefs.updateMaxUdpFlows(prefs.maxUdpFlows - 32) },
                    onIncrease = { prefs.updateMaxUdpFlows(prefs.maxUdpFlows + 32) },
                )
            },
        )

        val answerDnsLabel = stringResource(R.string.settings_answer_dns)
        val answerDnsHint = stringResource(R.string.settings_answer_dns_hint)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(answerDnsLabel, answerDnsHint)) {
                DetourToggleRow(
                    label = answerDnsLabel,
                    hint = answerDnsHint,
                    checked = prefs.answerDnsFromRules,
                    onChange = { prefs.updateAnswerDns(it) },
                )
            },
        )

        // Reachable at last. This was on with no way to see it or turn it
        // off, which made a working tunnel look broken whenever a probe
        // misjudged an address.
        val certLabel = stringResource(R.string.settings_certificate_check)
        val certHint = stringResource(R.string.settings_certificate_check_hint)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(certLabel, certHint)) {
                DetourToggleRow(
                    label = certLabel,
                    hint = certHint,
                    checked = prefs.certificateCheck,
                    onChange = { prefs.updateCertificateCheck(it) },
                )
            },
        )

        val observeDnsLabel = stringResource(R.string.settings_observe_dns)
        val observeDnsHint = stringResource(R.string.settings_observe_dns_hint)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(observeDnsLabel, observeDnsHint)) {
                DetourToggleRow(
                    label = observeDnsLabel,
                    hint = observeDnsHint,
                    checked = prefs.observeDns,
                    onChange = { prefs.updateObserveDns(it) },
                )
            },
        )

        // 开发者视图 sits here — directly above 内核版本, and therefore directly
        // above the console it reveals — and not in 日志与诊断, where it started.
        //
        // The switch was never broken. `prefs.developerView` is Compose state, the
        // console row below is added and removed on the same frame, and the adb
        // path proved it. The defect was **distance**: the switch sat about two and
        // a half screens above its only effect on this screen, so tapping it changed
        // nothing inside the viewport. Measured on the device before and after the
        // tap, the visible tree was byte-identical (2026-09-27) — which is exactly
        // what "the switch does nothing" looks like from the outside, and it is the
        // same failure the fold used to cause, one layer down. A `LazyColumn` only
        // composes what is near the viewport, so a row two screens away does not
        // merely look absent; it does not exist yet.
        //
        // The rule this settles: **a switch belongs next to what it reveals.** That
        // is why the fix is a move and not an auto-scroll — a control that scrolls
        // the page out from under the user's finger is a second surprise on top of
        // the one being fixed, and it would still leave "off" with no visible
        // answer, because the row it scrolled to would simply vanish.
        //
        // The switch also gates the developer rows in the home screen's session
        // card. That half is cross-screen and cannot be moved next to the switch,
        // so the hint names it — otherwise the switch reads as settings-only and
        // the home screen appears to change for no reason.
        val devViewLabel = stringResource(R.string.settings_developer_view)
        val devViewHint = stringResource(R.string.settings_developer_view_hint)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(devViewLabel, devViewHint)) {
                DetourToggleRow(
                    label = devViewLabel,
                    hint = devViewHint,
                    checked = prefs.developerView,
                    onChange = { prefs.updateDeveloperView(it) },
                )
            },
        )

        val kernelVersionLabel = stringResource(R.string.settings_kernel_version)
        add(
            SettingsRow(SettingsSection.ADVANCED, listOf(kernelVersionLabel)) {
                DetourKeyValueRow(
                    label = kernelVersionLabel,
                    // Read from the build rather than typed in. A hard-coded
                    // string here goes stale on the first release and then
                    // reports the wrong version to the person trying to diagnose
                    // a problem. When the library failed to load, that failure
                    // *is* the answer.
                    value = Kernel.loadError ?: "watt-ffi ${BuildConfig.VERSION_NAME}",
                )
            },
        )

        // The in-app console, directly under 内核版本. Gated on `开发者视图`, not on
        // `BuildConfig.DEBUG` like the adb receiver: an exported broadcast receiver
        // is a remote-control surface any app on the device can reach, so it is
        // debug-only; this can only be typed into by whoever is holding the phone,
        // so it is a user's choice whether to show it. `prefs.developerView` is
        // Compose state, so flipping the switch adds or removes this row live.
        //
        // "Live" is necessary but was not sufficient: the switch has to be *next to*
        // this row for the change to be seen, which is why it is the row directly
        // above 内核版本. See the note on it.
        //
        // It joins the search model like any other row — searching 命令行 finds it,
        // which is intended.
        val consoleLabel = stringResource(R.string.settings_console)
        val consoleHint = stringResource(R.string.settings_console_hint)
        if (prefs.developerView) {
            add(
                SettingsRow(SettingsSection.ADVANCED, listOf(consoleLabel, consoleHint)) {
                    DetourConsoleRow(
                        label = consoleLabel,
                        hint = consoleHint,
                        input = consoleInput,
                        onInputChange = { consoleInput = it },
                        onSubmit = {
                            // A blank line or a `#` comment parses to null and is
                            // ignored — the console is not the place to scold a
                            // user for pressing enter on an empty field.
                            ControlConsole.parse(consoleInput)?.let { parsed ->
                                val line = consoleInput.trim()
                                // Clear the field and mark the run before the work
                                // starts, so the UI answers the tap immediately.
                                consoleInput = ""
                                consoleRunning = true
                                consoleScope.launch {
                                    // Off the main thread: `run` may fetch a
                                    // megabyte of rules or read files. A failure is
                                    // turned into the same `error` shape a command
                                    // returns, so the transcript renders it the
                                    // same way rather than crashing.
                                    val result = withContext(Dispatchers.IO) {
                                        runCatching {
                                            ControlConsole.run(
                                                context.applicationContext,
                                                parsed.command,
                                                parsed.value,
                                                parsed.key,
                                            )
                                        }.getOrElse {
                                            JSONObject().put("error", it.message ?: it.javaClass.simpleName)
                                        }
                                    }
                                    consoleEntries = (
                                        consoleEntries + ConsoleEntry(
                                            line,
                                            // Pretty-printed: this is a console meant
                                            // to be read, and `dump`'s compact
                                            // one-line form is unusable.
                                            result.toString(2),
                                            result.has("error"),
                                        )
                                        ).takeLast(CONSOLE_MAX_ENTRIES)
                                    consoleRunning = false
                                }
                            }
                        },
                        onClear = { consoleEntries = emptyList() },
                        entries = consoleEntries,
                        running = consoleRunning,
                    )
                },
            )
        }

        // --- data -------------------------------------------------------------

        // Export is an action, not a value, so it is an action row and not a
        // toggle or a choice: there is no state to show, only a thing to do.
        val exportLabel = stringResource(R.string.settings_export)
        val exportHint = stringResource(R.string.settings_export_hint)
        add(
            SettingsRow(SettingsSection.DATA, listOf(exportLabel, exportHint)) {
                DetourActionRow(
                    label = exportLabel,
                    hint = exportHint,
                    actionLabel = stringResource(R.string.settings_export_action),
                    onAction = {
                        // Built at the moment of the tap, not at composition: the
                        // suggested name should carry the time of the export, not
                        // the time the row happened to be composed. No colons — a
                        // colon is legal in a display name but is a path separator
                        // on some providers, which would silently rename or reject
                        // the file.
                        exportPicker.launch(
                            "detour-settings-" +
                                LocalDateTime.now()
                                    .format(DateTimeFormatter.ofPattern("yyyyMMdd-HHmmss")) +
                                ".json",
                        )
                    },
                )
            },
        )

        val importLabel = stringResource(R.string.settings_import)
        val importHint = stringResource(R.string.settings_import_hint)
        add(
            SettingsRow(SettingsSection.DATA, listOf(importLabel, importHint)) {
                DetourActionRow(
                    label = importLabel,
                    hint = importHint,
                    actionLabel = stringResource(R.string.settings_import_action),
                    // A confirm first, because import overwrites the settings the
                    // user is looking at — the same reason 恢复默认 has one. The
                    // dialog's confirm, not this tap, launches the picker, so the
                    // chooser only appears after the overwrite is agreed to.
                    onAction = { showImportConfirm = true },
                )
            },
        )

        val aboutLabel = stringResource(R.string.settings_about)
        val aboutHint = stringResource(R.string.settings_about_hint)
        add(
            SettingsRow(SettingsSection.DATA, listOf(aboutLabel, aboutHint)) {
                DetourActionRow(
                    label = aboutLabel,
                    hint = aboutHint,
                    actionLabel = stringResource(R.string.settings_about_action),
                    onAction = onOpenAbout,
                )
            },
        )
    }

    val needle = query.trim()
    val visible = if (needle.isEmpty()) {
        rows
    } else {
        rows.filter { row -> row.searchText.any { it.contains(needle, ignoreCase = true) } }
    }

    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        // The bar now floats *over* the content rather than sitting below it in a
        // `Scaffold` slot, so `Scaffold` no longer hands this screen a bottom
        // inset that clears it. The clearance has to be added here, on the scroll
        // container that owns the padding, or the last row ends up underneath the
        // bar. `LocalBottomBarClearance` is the measured bar height plus the
        // navigation-bar inset; it is zero outside the app body, so a preview is
        // unaffected.
        contentPadding = PaddingValues(
            start = 20.dp,
            end = 20.dp,
            top = 12.dp,
            bottom = 32.dp + LocalBottomBarClearance.current,
        ),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Text(
                stringResource(R.string.settings_title),
                style = MaterialTheme.typography.headlineSmall,
            )
        }

        // The field's horizontal inset is the list's own `contentPadding`, not a
        // second `padding` here — doubling it would make the field narrower than
        // every card below it.
        item {
            // The type is spelled out on the local so the lambda is inferred as
            // composable; inside an `if` the slot's expected type alone is not
            // always enough for that.
            val clearAction: (@Composable () -> Unit)? = if (query.isNotEmpty()) {
                {
                    IconButton(onClick = { query = "" }) {
                        Icon(Icons.Filled.Clear, contentDescription = null)
                    }
                }
            } else {
                null
            }
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                modifier = Modifier.fillMaxWidth(),
                singleLine = true,
                placeholder = { Text(stringResource(R.string.settings_search_hint)) },
                leadingIcon = { Icon(Icons.Filled.Search, contentDescription = null) },
                trailingIcon = clearAction,
            )
        }

        // Why this exists: every kernel setting is read exactly once, when the engine
        // is built, and this screen had no way to say so. A user who moved a row and
        // saw no effect had nothing on screen to tell them a reconnect was the
        // missing step — and "it silently did nothing" is indistinguishable from "it
        // is broken".
        //
        // Conditional, with a stable key and `animateItem()`: the item fades and
        // slides both in and out like every other list change in the app, and when
        // nothing is pending there is no item at all — so the list's `spacedBy`
        // contributes nothing and the gap is exactly what it was before the banner
        // existed. An always-present item would have kept the exit animation but
        // paid for it with a permanent extra 12 dp. It is deliberately outside the
        // search model — it is a statement about the running engine, not a setting.
        if (pending) {
            item(key = "settings-pending") {
                Box(Modifier.animateItem()) {
                    DetourSectionCard(stringResource(R.string.settings_kernel_pending_title)) {
                        Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
                            Text(
                                stringResource(R.string.settings_kernel_pending_body),
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Row(
                                modifier = Modifier.fillMaxWidth(),
                                horizontalArrangement = Arrangement.End,
                            ) {
                                DetourButton(
                                    onClick = {
                                        ContextCompat.startForegroundService(
                                            context,
                                            DetourVpnService.restartIntent(context, status.mode),
                                        )
                                    },
                                    variant = DetourButtonVariant.Text,
                                ) {
                                    Text(stringResource(R.string.settings_kernel_pending_action))
                                }
                            }
                        }
                    }
                }
            }
        }

        // One card per section, in declaration order, holding only the rows the
        // query left visible. A section whose rows all filtered out is skipped
        // entirely rather than shown empty.
        SettingsSection.values().forEach { section ->
            val sectionRows = visible.filter { it.section == section }
            if (sectionRows.isEmpty()) return@forEach
            item(key = section.name) {
                val title = stringResource(section.titleRes)
                DetourSectionCard(title) {
                    SettingsRows(sectionRows)
                }
            }
        }

        if (needle.isNotEmpty() && visible.isEmpty()) {
            item {
                Text(
                    stringResource(R.string.settings_search_empty),
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(vertical = 24.dp),
                    textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        // Not part of the search model: "restore defaults" is an action, not a
        // setting, and matching it against a query would let a search hide it —
        // which is the one row that has to stay reachable when everything else is
        // filtered away.
        item {
            DetourButton(
                onClick = { showRestoreConfirm = true },
                variant = DetourButtonVariant.Outlined,
            ) {
                Text(stringResource(R.string.settings_restore_defaults))
            }
        }
    }

    if (showAddSource) {
        AddRuleSourceDialog(
            onDismiss = { showAddSource = false },
            onAdd = { source ->
                prefs.addRuleSource(source)
                prefs.updateRuleSource(source.id)
                RulesRepository.invalidate(context)
                DetourVpnService.reloadRulesIfRunning()
                showAddSource = false
            },
        )
    }

    if (showRestoreConfirm) {
        DetourAlertDialog(
            onDismissRequest = { showRestoreConfirm = false },
            title = stringResource(R.string.settings_restore_confirm_title),
            text = { Text(stringResource(R.string.settings_restore_confirm_text)) },
            confirmButton = {
                DetourButton(
                    onClick = {
                        prefs.restoreDefaults()
                        // The rule source may have changed, and the same two extra
                        // steps are what every other source change on this screen
                        // does: drop the cache and hand a running tunnel the result.
                        RulesRepository.invalidate(context)
                        DetourVpnService.reloadRulesIfRunning()
                        // `stringResource` is not callable from a click handler, so
                        // the toast text comes from the context directly.
                        Toast.makeText(
                            context,
                            context.getString(R.string.settings_restore_done),
                            Toast.LENGTH_SHORT,
                        ).show()
                        showRestoreConfirm = false
                    },
                ) {
                    Text(stringResource(R.string.settings_restore_confirm_ok))
                }
            },
            dismissButton = {
                DetourButton(
                    onClick = { showRestoreConfirm = false },
                    variant = DetourButtonVariant.Text,
                ) {
                    Text(stringResource(android.R.string.cancel))
                }
            },
        )
    }

    if (showImportConfirm) {
        DetourAlertDialog(
            onDismissRequest = { showImportConfirm = false },
            title = stringResource(R.string.settings_import_confirm_title),
            text = { Text(stringResource(R.string.settings_import_confirm_text)) },
            confirmButton = {
                DetourButton(
                    onClick = {
                        showImportConfirm = false
                        // The chooser opens from the confirm, not from the row:
                        // the tap that agrees to the overwrite is the one that
                        // should ask for the file, so cancelling the dialog never
                        // opens a picker the user did not ask for.
                        importPicker.launch(arrayOf("application/json", "*/*"))
                    },
                ) {
                    Text(stringResource(R.string.settings_import_confirm_ok))
                }
            },
            dismissButton = {
                DetourButton(
                    onClick = { showImportConfirm = false },
                    variant = DetourButtonVariant.Text,
                ) {
                    Text(stringResource(android.R.string.cancel))
                }
            },
        )
    }
}

/**
 * A section's rows, with the hairline between them.
 *
 * The divider is placed *between* rows and never at either end, which is what
 * lets one renderer draw a card that opens on a row and closes on a row no
 * matter how many the search left visible — and is why the rows are data instead
 * of each card hand-writing its own dividers.
 */
@Composable
private fun ColumnScope.SettingsRows(rows: List<SettingsRow>) {
    rows.forEachIndexed { index, row ->
        if (index > 0) DetourDivider()
        row.content()
    }
}

/**
 * One row of the source list: a radio, a name, its address, and a delete.
 *
 * Built-in sources have no delete — they are not in storage to delete, and
 * [Prefs.removeRuleSource] refuses them anyway, so offering the button would be a
 * control that cannot do what it says.
 *
 * The subtitle is three-way now, and each case is a different kind of answer. A
 * source imported from a file says so and nothing more: its `url` is empty, so
 * the old `if (usable) url else …` would have labelled it "暂不可用" — a working
 * source described as broken, which is the worst of the three. The copy's own
 * name is a SHA-256 and would tell the reader nothing, and the picked file's name
 * is already the row's title; `dump` is where the copy's name belongs.
 *
 * The last case is the "unavailable" string. That branch no longer describes a
 * *retired* source — no built-in sets `unavailable` since `s302` was deleted (see
 * [RuleSource]) — it is what shows for a source with neither an address nor a
 * local copy, the one state `usable` is false for.
 */
@Composable
private fun RuleSourceRow(
    source: RuleSource,
    selected: Boolean,
    onSelect: () -> Unit,
    onDelete: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(enabled = source.usable, onClick = onSelect)
            .padding(start = 16.dp, end = 8.dp, top = 4.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        RadioButton(selected = selected, onClick = onSelect, enabled = source.usable)
        Column(Modifier.weight(1f)) {
            Text(
                source.labelRes?.let { stringResource(it) } ?: source.label,
                style = MaterialTheme.typography.bodyLarge,
            )
            Text(
                when {
                    source.isLocal -> stringResource(R.string.rules_source_local)
                    source.usable -> source.url
                    else -> stringResource(R.string.rules_source_unavailable)
                },
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        if (!source.builtin) {
            IconButton(onClick = onDelete) {
                Icon(
                    Icons.Filled.Delete,
                    contentDescription = stringResource(R.string.rules_source_delete),
                )
            }
        }
    }
}
