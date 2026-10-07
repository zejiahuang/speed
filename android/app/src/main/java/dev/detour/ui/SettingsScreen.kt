package dev.detour.ui

import android.widget.Toast
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.StringRes
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
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
import androidx.compose.material3.Surface
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
import androidx.compose.ui.text.input.PasswordVisualTransformation
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
import dev.detour.ui.components.DetourCard
import dev.detour.ui.components.DetourCardStyle
import dev.detour.ui.components.ConsoleEntry
import dev.detour.ui.components.DetourChoiceRow
import dev.detour.ui.components.DetourConsoleRow
import dev.detour.ui.components.DetourDivider
import dev.detour.ui.components.DetourKeyValueRow
import dev.detour.ui.components.DetourNavRow
import dev.detour.ui.components.DetourPageHeader
import dev.detour.ui.components.LocalBottomBarClearance
import dev.detour.ui.components.DetourSectionCard
import dev.detour.ui.components.DetourSegmentedRow
import dev.detour.ui.components.DetourSliderRow
import dev.detour.ui.components.DetourStepperRow
import dev.detour.ui.components.DetourToggleRow
import dev.detour.ui.theme.LocalDetourMotion
import dev.detour.ui.theme.supportsDynamicColor
import java.io.IOException
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * The five groups, in the order they are shown — and, since the settings screen
 * was split, the five second-level pages.
 *
 * An enum rather than five hand-written cards because the search has to walk the
 * groups in a fixed order and ask each one for its visible rows; with the groups
 * as data the renderer is a loop instead of five near-identical blocks, and
 * adding a group cannot forget to update the renderer.
 *
 * The groups were not re-cut for the split. They were already the answer to
 * "what is this setting about", which is the question a page split has to
 * answer, and re-cutting them would have moved rows for no reason. [titleRes] is
 * therefore both the card heading and the page title, and [summaryRes] is the
 * entry row's second line on the settings screen — a summary rather than an
 * inventory of the rows inside, because an inventory would have to be kept in
 * step with them and would be wrong the first time one moved.
 *
 * [DATA] is declared last on purpose. Every group before it changes a setting
 * this app owns; the data group's rows either hand a file to, or take one from,
 * another app, or leave for another screen. Putting it last keeps the settings
 * proper together and reads the group as what it is — the exit.
 */
enum class SettingsPage(
    @StringRes val titleRes: Int,
    @StringRes val summaryRes: Int,
) {
    CONNECTION(R.string.settings_section_connection, R.string.settings_page_connection),
    LOGS(R.string.settings_section_logs, R.string.settings_page_logs),
    APPEARANCE(R.string.settings_section_appearance, R.string.settings_page_appearance),
    ADVANCED(R.string.settings_section_advanced, R.string.settings_page_advanced),
    DATA(R.string.settings_section_data, R.string.settings_page_data),
}

/**
 * One setting, as data.
 *
 * [searchText] is everything about this row a search may match: the label, and
 * its hint where it has one. Resolving those strings here — rather than having
 * the filter reach back into resources — is what lets "no match" be knowable
 * without keeping a second copy of every label in step with the first.
 *
 * [visible] is for a row that exists only while another control is on: the glass
 * sliders exist while 玻璃效果 is on, the wallpaper scrim while a wallpaper is
 * set, the console while 开发者视图 is on. Those rows used to be left out of the
 * list entirely, which made them *appear* — a switch flipped and five rows were
 * simply there on the next frame, with no way for the eye to follow what the
 * switch had done. A row that is always in the list and merely collapsed can be
 * animated open and shut, and the switch reads as the cause of the movement.
 *
 * It defaults to true so that the ~40 rows that are unconditional say nothing
 * about it. It is not a general-purpose "hide this row": a row that is never
 * shown is a row that should not be in the list, and one that is always hidden
 * would be a collapsed row taking part in the search model for no reason.
 *
 * **`visible` is declared before `content`, and that ordering is required rather
 * than stylistic.** Kotlin binds a trailing lambda to the *last* parameter, not
 * to the last function-typed one, so a defaulted `visible` sitting last would
 * take every call's trailing lambda and leave `content` unset — all forty-odd
 * rows fail to compile with "actual type is '() -> Unit', but 'Boolean' was
 * expected". Keeping the lambda last is what lets the gated rows read
 * `SettingsRow(PAGE, keys, visible = on) { ... }` exactly like the plain ones.
 */
private class SettingsRow(
    val page: SettingsPage,
    val searchText: List<String>,
    val visible: Boolean = true,
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
 * Settings: the entries into the five groups, or one group when [page] names it.
 *
 * Grouped by *what the setting is about* rather than by how often it is used. A
 * flat list sorted by frequency would put the proxy port next to the dark mode
 * switch, and neither belongs near the other.
 *
 * **The five cards became five second-level pages (2026-10-06).** The list had
 * reached 46 rows across five cards — several screens of scroll with no way to
 * take in what was on it — and the owner asked for it to be tidied up with
 * second-level pages. The groups were not re-cut: they were already the answer
 * to "what is this setting about", which is the question a split has to answer,
 * and re-cutting them would have moved rows for no reason. What the split costs
 * is one tap on the way to every setting, and that is paid deliberately; the
 * alternative was a page nobody could survey.
 *
 * **Nothing is hidden, and that is a constraint rather than a claim.** Hiding
 * settings makes them unfindable — this file has already been burned by that
 * once, in the fold removed below. Two things hold the line. The search still
 * walks every row in every group, and answers with the matching rows themselves,
 * rendered in place: a setting inside a page is therefore one search away and
 * needs no navigating at all. And a query matching a group's own title returns
 * that group whole, so typing 外观 lists what is in 外观 instead of one row
 * pointing at it. A group is also always one tap away, and says what it holds on
 * its entry row.
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
 * next to what it reveals. The general rule is recorded there, and the page split
 * keeps it: both rows are inside the same page, so the distance is unchanged.
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
 * [page] is null for the list of entries and names a group for that group's own
 * page. [onNavigate] takes the group to open, or null to close: one callback for
 * both directions rather than two, because the list and the page are the same
 * component in two states and a split pair would let a call site handle one
 * direction and silently drop the other. [onOpenAbout] is a callback rather than
 * a navigation call made here, because this screen is not the one that owns the
 * destination stack. None of the three has a default: a default would let a call
 * site forget to pass it and silently lose its only effect, which is the
 * "control that does nothing" defect this file has already been burned by.
 */
@Composable
fun SettingsScreen(
    page: SettingsPage?,
    onNavigate: (SettingsPage?) -> Unit,
    onOpenAbout: () -> Unit,
) {
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

        // The proxy port only means anything in proxy mode, which is the mode
        // [BuildFlags.TUN_ONLY] would hide. The row and the divider that
        // separated it from the source selector go together, or the card
        // would open with a stray divider at the top.
        if (!BuildFlags.TUN_ONLY) {
            val proxyLabel = stringResource(R.string.settings_proxy_port)
            val proxyHint = stringResource(R.string.settings_proxy_port_hint, prefs.proxyPort)
            add(
                SettingsRow(SettingsPage.CONNECTION, listOf(proxyLabel, proxyHint)) {
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
                page = SettingsPage.CONNECTION,
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
            SettingsRow(SettingsPage.CONNECTION, listOf(refreshLabel)) {
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
            SettingsRow(SettingsPage.CONNECTION, listOf(offlineLabel, offlineHint)) {
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
            SettingsRow(SettingsPage.CONNECTION, listOf(autoConnectLabel, autoConnectHint)) {
                DetourToggleRow(
                    label = autoConnectLabel,
                    hint = autoConnectHint,
                    checked = prefs.autoConnect,
                    onChange = { prefs.updateAutoConnect(it) },
                )
            },
        )

        // 断开前二次确认 sits here rather than in 外观, where it was until the page
        // split. The row is about what the disconnect button does, not about how
        // the app looks — its own hint says so, 避免误触把正在跑的下载掐掉 — and it
        // stayed misfiled only because a 46-row list put the right card a scroll
        // away and nobody was reading the headings. With the pages split, "which
        // page would I look on" is the only way to find a row, so a row filed
        // under the wrong page is a row that cannot be found.
        val confirmLabel = stringResource(R.string.settings_confirm_disconnect)
        val confirmHint = stringResource(R.string.settings_confirm_disconnect_hint)
        add(
            SettingsRow(SettingsPage.CONNECTION, listOf(confirmLabel, confirmHint)) {
                DetourToggleRow(
                    label = confirmLabel,
                    hint = confirmHint,
                    checked = prefs.confirmDisconnect,
                    onChange = { prefs.updateConfirmDisconnect(it) },
                )
            },
        )

        // --- root mode --------------------------------------------------------

        // The root-mode cost, stated here rather than only in the mode picker.
        // It is the one consequence this app cannot show once the mode is running:
        // the proxy presents its own certificate, so the client stops verifying
        // the real one. A row of its own — always present, matched by search — so
        // it is findable whether or not the mode is selected, which a hint on a
        // mode-only control would not be.
        val rootRiskTitle = stringResource(R.string.settings_root_risk_title)
        val rootRiskBody = stringResource(R.string.settings_root_risk_body)
        add(
            SettingsRow(SettingsPage.CONNECTION, listOf(rootRiskTitle, rootRiskBody)) {
                Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
                    Text(rootRiskTitle, style = MaterialTheme.typography.bodyLarge)
                    Text(
                        rootRiskBody,
                        modifier = Modifier.padding(top = 4.dp),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            },
        )

        // --- the upstream exit ------------------------------------------------

        // One row, not five, and that is not a layout preference. The endpoint is
        // what the enable switch is gated on, so splitting them would let a search
        // surface the switch with its precondition filtered away — a control whose
        // reason to exist is off screen. Kept together the row also answers the one
        // question this setting is about: where does my traffic leave from.
        //
        // The endpoint field is read straight from and written straight to `prefs`
        // rather than through a hoisted `remember`. That is the opposite of the
        // console's row above, and the reason is the same one: a `remember` inside
        // a row lambda is positionally unstable, and a plain read of Compose state
        // has no position to be unstable about.
        val exitLabel = stringResource(R.string.settings_upstream_proxy)
        val exitHint = stringResource(R.string.settings_upstream_proxy_hint)
        val exitAddressLabel = stringResource(R.string.settings_upstream_proxy_address)
        val exitAddressBad = stringResource(R.string.settings_upstream_proxy_address_invalid)
        val exitKindLabel = stringResource(R.string.settings_upstream_proxy_kind)
        val exitUserLabel = stringResource(R.string.settings_upstream_proxy_username)
        val exitPassLabel = stringResource(R.string.settings_upstream_proxy_password)
        val exitEnableLabel = stringResource(R.string.settings_upstream_proxy_enable)
        val exitEnableHint = stringResource(R.string.settings_upstream_proxy_enable_hint)
        // The error state is "there is text and it is not an endpoint". An empty
        // field is not an error — it is the state every install starts in — and
        // marking it red would greet every user with a complaint about a setting
        // they have not touched.
        val exitAddressWrong = prefs.upstreamProxyAddress.isNotEmpty() && !prefs.upstreamProxyConfigured
        // Spelled out on the local so the lambda is inferred as composable; inside
        // an `if` the slot's expected type alone is not always enough for that. Same
        // reason as the search field's `clearAction` above.
        val exitAddressSupport: (@Composable () -> Unit)? = if (exitAddressWrong) {
            { Text(exitAddressBad) }
        } else {
            null
        }
        add(
            SettingsRow(
                page = SettingsPage.CONNECTION,
                searchText = listOf(
                    exitLabel, exitHint, exitAddressLabel, exitKindLabel,
                    exitUserLabel, exitPassLabel, exitEnableLabel, exitEnableHint,
                ),
            ) {
                Text(
                    exitLabel,
                    modifier = Modifier.padding(start = 16.dp, top = 12.dp),
                    style = MaterialTheme.typography.bodyLarge,
                )
                Text(
                    exitHint,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedTextField(
                    value = prefs.upstreamProxyAddress,
                    onValueChange = { prefs.updateUpstreamProxyAddress(it) },
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp),
                    singleLine = true,
                    label = { Text(exitAddressLabel) },
                    placeholder = { Text("203.0.113.7:1080") },
                    isError = exitAddressWrong,
                    supportingText = exitAddressSupport,
                )
                // The protocol control is the shared segmented row, which brings its
                // own 16 dp inset — so nothing here wraps the row in a second one,
                // or the control would sit 32 dp in while every field above it sits
                // at 16.
                DetourSegmentedRow(
                    label = exitKindLabel,
                    options = listOf(
                        Prefs.PROXY_KIND_SOCKS5 to R.string.settings_upstream_proxy_kind_socks5,
                        Prefs.PROXY_KIND_HTTP_CONNECT to R.string.settings_upstream_proxy_kind_http,
                    ),
                    selected = prefs.upstreamProxyKind,
                    onSelect = { prefs.updateUpstreamProxyKind(it) },
                )
                OutlinedTextField(
                    value = prefs.upstreamProxyUsername,
                    onValueChange = { prefs.updateUpstreamProxyUsername(it) },
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp),
                    singleLine = true,
                    label = { Text(exitUserLabel) },
                )
                OutlinedTextField(
                    value = prefs.upstreamProxyPassword,
                    onValueChange = { prefs.updateUpstreamProxyPassword(it) },
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(start = 16.dp, end = 16.dp, top = 8.dp),
                    singleLine = true,
                    label = { Text(exitPassLabel) },
                    visualTransformation = PasswordVisualTransformation(),
                )
                // The switch, and only once the endpoint is one the kernel can dial.
                // This is the whole gate: with the address blank, or a name, or
                // missing a port, there is no control to turn on rather than a
                // control that turns on and does nothing. The flag behind it is kept
                // across the address being cleared, so retyping the same endpoint
                // comes back with the exit still on — and `Prefs.upstreamProxyActive`
                // makes the blank case off regardless of what the flag says, so the
                // hidden state can never be "on with nothing proxied".
                if (prefs.upstreamProxyConfigured) {
                    DetourToggleRow(
                        label = exitEnableLabel,
                        hint = exitEnableHint,
                        checked = prefs.upstreamProxyEnabled,
                        onChange = { prefs.updateUpstreamProxyEnabled(it) },
                    )
                }
            },
        )

        // The upstream resolver. A chip row, not a switch, and the difference is
        // the whole point of the setting: the measurement behind it
        // (`memory/2026-10-02.md` §14) held the resolver fixed and varied only
        // the transport — plain UDP/53, plain TCP/53, DoH — and got the same
        // forged addresses all three times. The forgery is made by the resolver,
        // not by the path to it, so a switch labelled "DoH" would be selling the
        // wrong variable. What is chosen here is *which resolver answers*, and
        // DoH is only how it is spoken to.
        //
        // It sits next to the exit because both answer "where does this leave
        // from", and they are independent: the exit is for names whose SNI is
        // blocked, this is for names whose answer is forged. A user may want
        // either, both, or neither.
        val resolverLabel = stringResource(R.string.settings_dns_upstream)
        val resolverHint = stringResource(R.string.settings_dns_upstream_hint)
        val resolverNoneLabel = stringResource(R.string.settings_dns_upstream_none)
        val resolverUrlLabel = stringResource(R.string.settings_dns_upstream_url)
        val resolverUrlHint = stringResource(R.string.settings_dns_upstream_url_hint)
        val resolverUrlBad = stringResource(R.string.settings_dns_upstream_url_invalid)
        val resolverAddressLabel = stringResource(R.string.settings_dns_upstream_address)
        val resolverAddressHint = stringResource(R.string.settings_dns_upstream_address_hint)
        // Which chip is lit is *derived* from the stored pair rather than
        // remembered beside it, so the stored URL stays the single source of
        // truth: a chip that claimed a selection the kernel would not act on is
        // exactly the "control that cannot take effect" this screen exists to
        // avoid. A pair matching no preset is a custom endpoint, and then no chip
        // is lit — which is the honest reading, not a missing selection.
        val resolverPreset = Prefs.DNS_UPSTREAM_PRESETS.firstOrNull {
            it.url == prefs.dnsUpstreamUrl && it.address == prefs.dnsUpstreamAddress
        }
        val resolverSelected = when {
            resolverPreset != null -> resolverPreset.id
            prefs.dnsUpstreamUrl.isEmpty() -> ""
            else -> "custom"
        }
        // "There is text and it is not an endpoint". An empty field is not an
        // error — it is the state every install starts in — and marking it red
        // would greet every user with a complaint about a setting they have not
        // touched.
        val resolverWrong = prefs.dnsUpstreamUrl.isNotEmpty() && !prefs.dnsUpstreamConfigured
        val resolverSupport: (@Composable () -> Unit)? = if (resolverWrong) {
            { Text(resolverUrlBad) }
        } else {
            null
        }
        add(
            SettingsRow(
                page = SettingsPage.CONNECTION,
                searchText = buildList {
                    add(resolverLabel)
                    add(resolverHint)
                    add(resolverNoneLabel)
                    Prefs.DNS_UPSTREAM_PRESETS.forEach { add(stringResource(it.labelRes)) }
                    add(resolverUrlLabel)
                    add(resolverUrlHint)
                    add(resolverAddressLabel)
                    add(resolverAddressHint)
                },
            ) {
                DetourChoiceRow(
                    label = resolverLabel,
                    hint = resolverHint,
                    options = buildList {
                        add("" to R.string.settings_dns_upstream_none)
                        Prefs.DNS_UPSTREAM_PRESETS.forEach { add(it.id to it.labelRes) }
                    },
                    selected = resolverSelected,
                    onSelect = { id ->
                        // Every branch is the same single write, so a preset and
                        // a hand-typed endpoint cannot take different paths into
                        // the setting. `""` is "the client's own resolver", which
                        // is both the first chip and the cleared state.
                        val preset = Prefs.DNS_UPSTREAM_PRESETS.firstOrNull { it.id == id }
                        when {
                            id.isEmpty() -> prefs.updateDnsUpstream("", "")
                            preset != null -> prefs.updateDnsUpstream(preset.url, preset.address)
                            // The custom chip is derived, never rendered, so this
                            // arm is unreachable — kept so a future chip cannot
                            // silently do nothing.
                            else -> Unit
                        }
                    },
                )
                OutlinedTextField(
                    value = prefs.dnsUpstreamUrl,
                    onValueChange = { prefs.updateDnsUpstream(it, prefs.dnsUpstreamAddress) },
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp),
                    singleLine = true,
                    label = { Text(resolverUrlLabel) },
                    placeholder = { Text("https://1.1.1.1/dns-query") },
                    supportingText = resolverSupport,
                    isError = resolverWrong,
                )
                // Drawn unconditionally rather than only for a named endpoint.
                // Whether a name needs this is a property of the URL, and hiding
                // the field would mean the state changed shape under the user's
                // fingers as they typed the authority — the one moment they need
                // to see where it goes. The hint says when it matters.
                OutlinedTextField(
                    value = prefs.dnsUpstreamAddress,
                    onValueChange = { prefs.updateDnsUpstream(prefs.dnsUpstreamUrl, it) },
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(start = 16.dp, end = 16.dp, top = 8.dp),
                    singleLine = true,
                    label = { Text(resolverAddressLabel) },
                    supportingText = { Text(resolverAddressHint) },
                )
            },
        )

        // --- logs -------------------------------------------------------------

        val statsLabel = stringResource(R.string.settings_stats_interval)
        add(
            SettingsRow(SettingsPage.LOGS, listOf(statsLabel)) {
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
            SettingsRow(SettingsPage.LOGS, listOf(logArchiveLabel, logArchiveHint)) {
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
            SettingsRow(SettingsPage.APPEARANCE, listOf(dynColorLabel, dynColorHint)) {
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
            SettingsRow(SettingsPage.APPEARANCE, listOf(darkModeLabel)) {
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
            SettingsRow(SettingsPage.APPEARANCE, listOf(themeColorLabel)) {
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
            SettingsRow(SettingsPage.APPEARANCE, listOf(cornerLabel)) {
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
            SettingsRow(SettingsPage.APPEARANCE, listOf(glassLabel, glassHint)) {
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
        //
        // **The gate is the row's own `visible`, not an `if` around the `add`s.**
        // Both keep the sliders off screen; only `visible` lets them be animated.
        // A row left out of the list has nothing to grow from, so the switch used
        // to put five rows on screen between one frame and the next with nothing
        // connecting the movement to the control that caused it. The promise
        // above survives the change rather than being traded away for it:
        // `AnimatedVisibility` drops its content from composition once it has
        // collapsed, so a hidden slider is not merely invisible, it is not there.
        val glassVisible = prefs.glassEnabled
        val glassBlurLabel = stringResource(R.string.settings_glass_blur)
        add(
            SettingsRow(
                SettingsPage.APPEARANCE,
                listOf(glassBlurLabel),
                visible = glassVisible,
            ) {
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
            SettingsRow(
                SettingsPage.APPEARANCE,
                listOf(glassTintLabel),
                visible = glassVisible,
            ) {
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
            SettingsRow(
                SettingsPage.APPEARANCE,
                listOf(glassLensLabel),
                visible = glassVisible,
            ) {
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
            SettingsRow(
                SettingsPage.APPEARANCE,
                listOf(glassHighlightLabel),
                visible = glassVisible,
            ) {
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
            SettingsRow(
                SettingsPage.APPEARANCE,
                listOf(glassBorderLabel),
                visible = glassVisible,
            ) {
                DetourSliderRow(
                    label = glassBorderLabel,
                    value = prefs.glassBorder.toFloat(),
                    range = 0f..100f,
                    display = "${prefs.glassBorder}%",
                    onChange = { prefs.updateGlassBorder(it.toInt()) },
                )
            },
        )

        val wallpaperLabel = stringResource(R.string.settings_wallpaper)
        val wallpaperHint = stringResource(R.string.settings_wallpaper_hint)
        // Read once, so the two rows below agree on the same frame and the labels
        // cannot flicker between "选择图片" and "更换" mid-recomposition.
        val hasWallpaper = prefs.wallpaper.isNotBlank()
        add(
            SettingsRow(SettingsPage.APPEARANCE, listOf(wallpaperLabel, wallpaperHint)) {
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
        //
        // `visible` rather than an `if`, so that setting a wallpaper makes the row
        // grow in under the 选择图片 row that caused it — see the note on the glass
        // sliders above. Collapsed means out of composition, so "not shown" still
        // means not shown.
        val scrimLabel = stringResource(R.string.settings_wallpaper_scrim)
        add(
            SettingsRow(
                SettingsPage.APPEARANCE,
                listOf(scrimLabel),
                visible = hasWallpaper,
            ) {
                DetourSliderRow(
                    label = scrimLabel,
                    value = prefs.wallpaperScrim.toFloat(),
                    range = 0f..100f,
                    display = "${prefs.wallpaperScrim}%",
                    onChange = { prefs.updateWallpaperScrim(it.toInt()) },
                )
            },
        )

        val fontLabel = stringResource(R.string.settings_font_scale)
        add(
            SettingsRow(SettingsPage.APPEARANCE, listOf(fontLabel)) {
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
            SettingsRow(SettingsPage.APPEARANCE, listOf(homeRateLabel, homeRateHint)) {
                DetourToggleRow(
                    label = homeRateLabel,
                    hint = homeRateHint,
                    checked = prefs.homeShowRate,
                    onChange = { prefs.updateHomeShowRate(it) },
                )
            },
        )

        // --- advanced ---------------------------------------------------------

        val maxCandLabel = stringResource(R.string.settings_max_candidates)
        add(
            SettingsRow(SettingsPage.ADVANCED, listOf(maxCandLabel)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(staggerLabel)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(raceWidthLabel, raceWidthHint)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(raceLaunchLabel)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(maxDialingLabel, maxDialingHint)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(cooldownLabel)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(dialNamesLabel, dialNamesHint)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(mtuLabel, mtuHint)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(timeoutLabel)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(tcpIdleLabel)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(udpIdleLabel)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(maxTcpLabel)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(maxUdpLabel)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(answerDnsLabel, answerDnsHint)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(certLabel, certHint)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(observeDnsLabel, observeDnsHint)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(devViewLabel, devViewHint)) {
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
            SettingsRow(SettingsPage.ADVANCED, listOf(kernelVersionLabel)) {
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
        // which is intended — with one consequence that falls out of `visible`
        // rather than being chosen: while 开发者视图 is off the row is collapsed,
        // and a collapsed row is not in the search model, so 命令行 finds nothing
        // until the switch is on. That is the honest answer. The console is not
        // merely unlisted while it is off, it is not composed at all, so a search
        // result pointing at it would be pointing at a row that does not exist.
        val consoleLabel = stringResource(R.string.settings_console)
        val consoleHint = stringResource(R.string.settings_console_hint)
        // The gate is the row's own `visible` rather than an `if`, so that
        // flipping 开发者视图 grows the console in underneath the switch that
        // turned it on instead of dropping a row onto the page between two
        // frames. See the note on the glass sliders for the same change.
        add(
            SettingsRow(
                SettingsPage.ADVANCED,
                listOf(consoleLabel, consoleHint),
                visible = prefs.developerView,
            ) {
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

        // --- data -------------------------------------------------------------

        // Export is an action, not a value, so it is an action row and not a
        // toggle or a choice: there is no state to show, only a thing to do.
        val exportLabel = stringResource(R.string.settings_export)
        val exportHint = stringResource(R.string.settings_export_hint)
        add(
            SettingsRow(SettingsPage.DATA, listOf(exportLabel, exportHint)) {
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
            SettingsRow(SettingsPage.DATA, listOf(importLabel, importHint)) {
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

        // 恢复默认设置 lives here rather than under the five entries on the
        // settings list, where it was until now. It is a data operation — it
        // throws away exactly what 导出数据 writes and 导入数据 restores — so it
        // belongs with them, and the page split is what made that answerable:
        // "which page would I look on" is now the only way to find a row.
        //
        // It is part of the search model like any other row. It used to be kept
        // out of it, on the argument that a query must not be able to hide the
        // one row that is always reachable. That argument does not survive the
        // move: a page is no more reachable under an active query than a row is,
        // so keeping it unsearchable would only have meant that 恢复 found
        // nothing anywhere. Searching 恢复 now finds it, which is the thing the
        // old arrangement was protecting.
        val restoreLabel = stringResource(R.string.settings_restore_defaults)
        val restoreHint = stringResource(R.string.settings_restore_defaults_hint)
        add(
            SettingsRow(SettingsPage.DATA, listOf(restoreLabel, restoreHint)) {
                DetourActionRow(
                    label = restoreLabel,
                    hint = restoreHint,
                    actionLabel = stringResource(R.string.settings_restore_action),
                    onAction = { showRestoreConfirm = true },
                )
            },
        )

        val aboutLabel = stringResource(R.string.settings_about)
        val aboutHint = stringResource(R.string.settings_about_hint)
        add(
            SettingsRow(SettingsPage.DATA, listOf(aboutLabel, aboutHint)) {
                // A nav row, not an action row: 关于 is another full page, so it
                // gets the same chevron and the same whole-row tap target as the
                // five entries that lead to this page. Its action used to read
                // 查看, which is the label this change removed everywhere it
                // meant "go somewhere".
                DetourNavRow(
                    label = aboutLabel,
                    hint = aboutHint,
                    onClick = onOpenAbout,
                )
            },
        )
    }

    val needle = query.trim()
    // Global, and that is the constraint the page split had to satisfy: hiding
    // settings makes them unfindable, so a query is answered with the matching
    // rows themselves, rendered where the search was typed, and it reaches the
    // rows inside a page without navigating to one. A group's own title matches
    // too, so 外观 returns what is in 外观 rather than one entry row pointing at it.
    val pageTitles = SettingsPage.entries.associateWith { stringResource(it.titleRes) }
    val matches = if (needle.isEmpty()) {
        emptyList()
    } else {
        rows.filter { row ->
            // A collapsed row is not on screen, so it is not a result. The
            // console while 开发者视图 is off is not hidden-but-findable, it is
            // absent from the composition, and a hit that pointed at it would
            // point at nothing.
            row.visible && (
                row.searchText.any { it.contains(needle, ignoreCase = true) } ||
                    pageTitles.getValue(row.page).contains(needle, ignoreCase = true)
                )
        }
    }

    // The reconnect the pending banner offers, as one lambda rather than two
    // copies: both the list and the pages show the banner, and a second copy is a
    // second place to forget that the mode has to come from the running engine.
    val onRestart: () -> Unit = {
        ContextCompat.startForegroundService(
            context,
            DetourVpnService.restartIntent(context, status.mode),
        )
    }

    if (page == null) {
        SettingsList(
            query = query,
            onQueryChange = { query = it },
            needle = needle,
            matches = matches,
            pending = pending,
            onRestart = onRestart,
            onOpenPage = onNavigate,
        )
    } else {
        SettingsGroupPage(
            page = page,
            rows = rows.filter { it.page == page },
            pending = pending,
            onRestart = onRestart,
            onClose = { onNavigate(null) },
        )
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
                    Text(stringResource(R.string.settings_restore_action))
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
 * The settings list: the search, the five entries, and 恢复默认.
 *
 * [matches] is the search result, and it is empty when nothing is being searched
 * for. The body switches on [needle] rather than on `matches.isEmpty()`, because
 * "nothing matched" and "nothing was typed" are two different screens — one shows
 * the entries, the other says the query found nothing — and a single test cannot
 * tell them apart.
 *
 * The rows are not drawn here at all when nothing is being searched for: the
 * entries are five fixed rows naming the five pages, and the settings themselves
 * live behind them. What keeps that from hiding anything is the search above
 * them; see the note on [SettingsScreen].
 */
@Composable
private fun SettingsList(
    query: String,
    onQueryChange: (String) -> Unit,
    needle: String,
    matches: List<SettingsRow>,
    pending: Boolean,
    onRestart: () -> Unit,
    onOpenPage: (SettingsPage) -> Unit,
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        // The bar floats *over* the content rather than sitting below it in a
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
                    IconButton(onClick = { onQueryChange("") }) {
                        Icon(Icons.Filled.Clear, contentDescription = null)
                    }
                }
            } else {
                null
            }
            OutlinedTextField(
                value = query,
                onValueChange = onQueryChange,
                modifier = Modifier.fillMaxWidth(),
                singleLine = true,
                placeholder = { Text(stringResource(R.string.settings_search_hint)) },
                leadingIcon = { Icon(Icons.Filled.Search, contentDescription = null) },
                trailingIcon = clearAction,
            )
        }

        // Why the banner exists: every kernel setting is read exactly once, when
        // the engine is built, and this screen had no way to say so. A user who
        // moved a row and saw no effect had nothing on screen to tell them a
        // reconnect was the missing step — and "it silently did nothing" is
        // indistinguishable from "it is broken".
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
                    PendingCard(onRestart)
                }
            }
        }

        if (needle.isEmpty()) {
            // The five entries, as one untitled card. No heading: the page's own
            // title is 设置 and every row already carries a group's name, so a
            // heading here would be the third copy of words already on screen
            // twice.
            item(key = "settings-entries") {
                UntitledSectionCard {
                    SettingsPage.entries.forEachIndexed { index, target ->
                        if (index > 0) DetourDivider()
                        DetourNavRow(
                            label = stringResource(target.titleRes),
                            hint = stringResource(target.summaryRes),
                            onClick = { onOpenPage(target) },
                        )
                    }
                }
            }
        } else {
            // One card per group that has matches, in declaration order. A group
            // whose rows all filtered out is skipped rather than shown empty — and
            // the card keeps its group's title, because after the split that title
            // is also the name of the page the row lives on, which is what tells
            // the reader where to go to find the row's neighbours.
            SettingsPage.entries.forEach { target ->
                val hits = matches.filter { it.page == target }
                if (hits.isEmpty()) return@forEach
                item(key = target.name) {
                    DetourSectionCard(stringResource(target.titleRes)) {
                        SettingsRows(hits)
                    }
                }
            }

            if (matches.isEmpty()) {
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
        }
    }
}

/**
 * One group, as a page of its own.
 *
 * **It is opaque, and that is a consequence of where it sits rather than a style
 * choice.** The app body draws this page as a sibling of the floating bar, over
 * it, and therefore outside the node marked with `layerBackdrop` — so there is no
 * backdrop within reach here, and a glass card would take `drawGlass`'s fallback
 * branch and read as "the glass is broken" rather than "this page has no glass".
 * The page paints `colorScheme.surface` instead; the cards inside it come from
 * the shared vocabulary and degrade to a flat tint on an opaque page, which reads
 * as an ordinary card. `AboutScreen` is drawn the same way for the same reason.
 *
 * The safe-area insets are taken here rather than from the app body's `Scaffold`,
 * for that same reason: this page covers that `Scaffold` rather than sitting
 * inside it, and there is no floating bar left to clear, so the bottom inset has
 * to be its own or the last row sits under the gesture bar.
 *
 * `BackHandler` rather than a close button only: the page covers the bar and
 * reads as an overlay, so the system back gesture has to close it; without this
 * it would fall through to the activity and quit the app from a page that looks
 * like a dialog.
 *
 * **Why the card under the header has no heading of its own.** [DetourPageHeader]
 * prints the group's name, and a card heading with the same words one line below
 * it is the duplication the 玻璃效果 heading was deleted for. The rows are the
 * page's whole content, so they are the card's whole content.
 */
@Composable
private fun SettingsGroupPage(
    page: SettingsPage,
    rows: List<SettingsRow>,
    pending: Boolean,
    onRestart: () -> Unit,
    onClose: () -> Unit,
) {
    // Enabled unconditionally: while this page is shown it is the topmost thing
    // on screen, so there is nothing else for back to mean.
    BackHandler(enabled = true) { onClose() }

    Surface(
        modifier = Modifier.fillMaxSize(),
        color = MaterialTheme.colorScheme.surface,
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .verticalScroll(rememberScrollState())
                .padding(bottom = 32.dp),
            // The settings list's own rhythm — 12 dp between cards — so the two
            // read as the same surface. The gutter is applied per card rather than
            // here, because `DetourPageHeader` carries its own 20 dp start padding
            // and a second one would double it.
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            DetourPageHeader(
                title = stringResource(page.titleRes),
                actions = {
                    DetourButton(onClick = onClose, variant = DetourButtonVariant.Text) {
                        Text(stringResource(R.string.settings_back))
                    }
                },
            )

            if (pending) {
                PendingCard(
                    onRestart = onRestart,
                    modifier = Modifier.padding(horizontal = 20.dp),
                )
            }

            UntitledSectionCard(Modifier.padding(horizontal = 20.dp)) {
                SettingsRows(rows)
            }
        }
    }
}

/**
 * The "the running engine is on old settings" banner.
 *
 * Shared by the list and the pages rather than written once per page: a change
 * made on a page has to be able to say so on that page, or the person who made it
 * has to navigate back to find out why nothing happened.
 */
@Composable
private fun PendingCard(onRestart: () -> Unit, modifier: Modifier = Modifier) {
    DetourSectionCard(
        title = stringResource(R.string.settings_kernel_pending_title),
        modifier = modifier,
    ) {
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
                DetourButton(onClick = onRestart, variant = DetourButtonVariant.Text) {
                    Text(stringResource(R.string.settings_kernel_pending_action))
                }
            }
        }
    }
}

/**
 * A group card without its heading.
 *
 * The same shape as [DetourSectionCard] minus the title, for the two places that
 * have no use for one: the list's entry card, whose page is already titled 设置
 * and whose rows each name their own group, and a group page, whose header
 * already names the group. Repeating the shape rather than making
 * [DetourSectionCard]'s title nullable is deliberate: a nullable title would let
 * a caller drop a heading by accident, and every existing call site would have to
 * be read to find out which ones meant it.
 */
@Composable
private fun UntitledSectionCard(
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    DetourCard(
        modifier = modifier,
        style = DetourCardStyle.Glass,
    ) {
        Column(Modifier.padding(vertical = 4.dp), content = content)
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
    val motion = LocalDetourMotion.current
    rows.forEachIndexed { index, row ->
        // The divider travels with the row *below* it rather than sitting
        // between two rows. A divider left behind while its row collapses is a
        // hairline hanging in space for the length of the animation — and at the
        // end of it, a card that opens on a divider.
        //
        // The `Column` is a shape, not a layout: it stacks the divider and the
        // row exactly as the enclosing card already stacked them, so a row that
        // is always visible is byte-for-byte the layout it was before this
        // wrapper existed.
        AnimatedVisibility(
            visible = row.visible,
            enter = expandVertically(motion.sizeSpatial),
            exit = shrinkVertically(motion.sizeSpatial),
        ) {
            Column {
                if (index > 0) DetourDivider()
                row.content()
            }
        }
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
