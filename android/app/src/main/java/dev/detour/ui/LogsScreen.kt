package dev.detour.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.widget.Toast
import androidx.compose.animation.Crossfade
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import dev.detour.R
import dev.detour.core.KernelState
import dev.detour.core.LogArchive
import dev.detour.ui.components.DetourAlertDialog
import dev.detour.ui.components.DetourButton
import dev.detour.ui.components.DetourButtonVariant
import dev.detour.ui.components.DetourCard
import dev.detour.ui.components.DetourCardStyle
import dev.detour.ui.components.DetourEmptyState
import dev.detour.ui.components.DetourFilterChip
import dev.detour.ui.components.DetourPageHeader
import dev.detour.ui.components.DetourSearchField
import dev.detour.ui.components.LocalBottomBarClearance
import dev.detour.ui.components.plusBarClearance
import dev.detour.ui.icons.DetourIcons
import dev.detour.ui.theme.LocalDetourMotion
import dev.detour.ui.theme.LocalDetourSpacing
import java.text.SimpleDateFormat
import java.util.Calendar
import java.util.Date
import java.util.Locale
import kotlinx.coroutines.launch

/**
 * The log, newest first.
 *
 * A `LazyColumn` rather than an auto-scrolling terminal. The service produces a
 * line per session and a sample twice a second; anything that scrolls itself
 * makes the screen unreadable the moment someone tries to read it, so the list
 * stays where the user put it. The price of that choice is that following the
 * live tail has no entry point at all — which is what the "回到最新" control
 * below pays back without giving up the no-auto-scroll rule: it appears only
 * once the user has scrolled away, so the list is never moved under someone who
 * did not ask for it.
 *
 * The filter chips are the three questions worth asking of this stream: what did
 * the sessions do, what went wrong, and what did DNS answer. The search box is
 * the fourth: none of those three can answer "where did `github.com` go". The two
 * narrow the same list, so they stack — a search inside the error chip means
 * "errors that mention this", not "errors, or this".
 *
 * Reading the list was also not the same as getting it out. A single line could
 * be copied and the whole buffer could be exported to a file, but the set the
 * user had just narrowed to — "the errors" — could not be copied as text. That
 * is what the copy-all action does, and it copies exactly what is on screen
 * rather than the full buffer, because the filtered view is the thing the user
 * was looking at when they decided to paste it somewhere.
 */
@Composable
fun LogsScreen() {
    val context = LocalContext.current
    val all by KernelState.logs.collectAsState()
    var paused by remember { mutableStateOf(false) }
    var filter by remember { mutableStateOf(Filter.ALL) }
    var query by remember { mutableStateOf("") }
    var confirmClear by remember { mutableStateOf(false) }
    // The header's overflow menu and the filter menu. Two flags rather than one
    // enum: they are anchored to different controls and can never be open together,
    // but sharing a single value would also couple their dismissal behaviour.
    var menuOpen by remember { mutableStateOf(false) }
    var filterMenuOpen by remember { mutableStateOf(false) }

    val listState = rememberLazyListState()
    val scope = rememberCoroutineScope()

    // How much room the floating glass bar occupies at the bottom of this
    // screen. It used to be a `Scaffold` `bottomBar`, which reserved that space
    // for every screen for free; now that it floats over the content, this
    // screen has to leave the room itself. Read once here, in composition, and
    // reused by both consumers below — the list's bottom padding and the
    // bottom-anchored button — so the two cannot drift to different heights.
    val barClearance = LocalBottomBarClearance.current
    val spacing = LocalDetourSpacing.current
    val motion = LocalDetourMotion.current

    // Resolved here because `stringResource` is composable and the click
    // handlers that show them are not. Capturing the resolved values keeps the
    // handlers plain lambdas.
    val copied = stringResource(R.string.logs_copied)
    val exportEmpty = stringResource(R.string.logs_export_empty)
    val exportDone = stringResource(R.string.logs_export_done)
    val exportShareTitle = stringResource(R.string.logs_export_share_title)

    // Midnight today, in epoch millis. Computed once here, at the list layer, and
    // handed down to each row as a comparison — never inside `LogRow`.
    //
    // The obvious alternative, resolving "today" per row, is wrong twice over: a
    // `Calendar` per row is an allocation per visible line, and the list redraws
    // twice a second as samples arrive, so that is per-frame garbage in the one
    // screen that is a dense list of glass cards. It is not `remember`ed either:
    // a remembered value would still say "yesterday" after midnight, and this
    // screen recomposes on every new line anyway, so one `Calendar` per
    // composition is both correct and free.
    val todayStart = startOfToday()

    // Copy one line in the shape the row shows it. `entry.level` is included
    // even though the row tints rather than prints it: a pasted log without the
    // severity is half a log. The stamp carries the date when the line is not
    // from today, for the same reason the row does — a pasted `23:58` from
    // yesterday is unreadable next to today's `09:00`.
    val copyEntry: (KernelState.LogEntry) -> Unit = { entry ->
        val text = "${stamp(entry, entry.atMillis >= todayStart)}  " +
            "${entry.level.name}  ${entry.tag}  ${entry.message}"
        context.getSystemService(ClipboardManager::class.java)
            ?.setPrimaryClip(ClipData.newPlainText("log", text))
        // A Toast rather than a "已复制" label swapped onto the row: the list
        // scrolls under the finger, so a per-row label would be unreadable or
        // attached to the wrong line by the time it was read.
        Toast.makeText(context, copied, Toast.LENGTH_SHORT).show()
    }

    // Freezing the list rather than dropping lines: what arrives while paused is
    // exactly what someone paused to look at.
    var frozen by remember { mutableStateOf<List<KernelState.LogEntry>>(emptyList()) }
    // The buffer the filters read. Lifted out of the `remember` below because the
    // empty state has to tell "no logs yet" apart from "no logs left after
    // filtering", and that question is asked of the source, not of `visible`.
    val source = if (paused) frozen else all
    val visible = remember(source, filter, query) {
        // Chip first, then the search term. Order matters only in that both must
        // apply; the search is a plain case-insensitive substring over the tag
        // and the message, because those are the two fields a person types.
        val byFilter = source.filter { filter.matches(it) }
        val needle = query.trim()
        if (needle.isEmpty()) byFilter else byFilter.filter { it.matchesQuery(needle) }
    }

    // Midnight of each visible line's day, computed once per list rather than per
    // row: a `Calendar` inside `LogRow` would be an allocation per visible line on
    // a screen that recomposes twice a second, which is exactly the cost the
    // `todayStart` note below avoids for the same reason.
    val dayStarts = remember(visible) { visible.map { startOfDay(it.atMillis) } }

    // Copies the current view, not the whole buffer. The text shape matches the
    // single-line copy above, so copying one line and copying all of them
    // produce the same line for the same entry.
    val copyAll: () -> Unit = {
        val text = visible.joinToString("\n") { entry ->
            "${stamp(entry, entry.atMillis >= todayStart)}  " +
                "${entry.level.name}  ${entry.tag}  ${entry.message}"
        }
        context.getSystemService(ClipboardManager::class.java)
            ?.setPrimaryClip(ClipData.newPlainText("logs", text))
        // A Toast, the same feedback the single-line copy already uses. A
        // `Snackbar` is the other candidate and it is the wrong one here: it
        // needs a `SnackbarHost`, this screen is drawn without a `Scaffold` of
        // its own, and hanging one means reaching up into the app shell and
        // changing `MainActivity` for a one-line confirmation. The count is
        // formatted from `getString` rather than a captured `stringResource`
        // because it is only known here, at click time.
        Toast.makeText(
            context,
            context.getString(R.string.logs_copied_all, visible.size),
            Toast.LENGTH_SHORT,
        ).show()
    }

    // Lifted out of the header's share button when the actions moved into a menu:
    // the body is unchanged, it just has a name now.
    val exportLogs: () -> Unit = export@{
        val out = java.io.File(context.cacheDir, "logs-export.txt")
        // Prefer the archive — the full history that survives a restart — and fall
        // back to the in-memory list, which is only what is on screen and is
        // capped at 500 lines.
        val archive = LogArchive.file(context)
        val fromArchive = archive.isFile && archive.length() > 0
        if (!fromArchive && KernelState.logs.value.isEmpty()) {
            Toast.makeText(context, exportEmpty, Toast.LENGTH_SHORT).show()
            return@export
        }
        runCatching {
            if (fromArchive) {
                archive.copyTo(out, overwrite = true)
            } else {
                // Same `atMillis<TAB>level<TAB>tag<TAB>message` shape as the archive
                // writes, so the two sources produce interchangeable files.
                out.writeText(
                    KernelState.logs.value.joinToString(
                        separator = "\n",
                        postfix = "\n",
                    ) {
                        "${it.atMillis}\t${it.level.name}\t${it.tag}\t${it.message}"
                    },
                )
            }
            // A FileProvider rather than `EXTRA_TEXT`: the archive can reach 2 MB
            // (`LogArchive.MAX_BYTES`), and an Intent extra over ~1 MB dies with
            // `TransactionTooLargeException`. The authority must match the
            // manifest's `${applicationId}.fileprovider`.
            val uri = FileProvider.getUriForFile(
                context,
                context.packageName + ".fileprovider",
                out,
            )
            val send = Intent(Intent.ACTION_SEND)
                .setType("text/plain")
                .putExtra(Intent.EXTRA_STREAM, uri)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            context.startActivity(Intent.createChooser(send, exportShareTitle))
        }.onSuccess {
            Toast.makeText(context, exportDone, Toast.LENGTH_SHORT).show()
        }.onFailure {
            Toast.makeText(
                context,
                context.getString(
                    R.string.logs_export_failed,
                    it.message ?: context.getString(R.string.common_unknown),
                ),
                Toast.LENGTH_SHORT,
            ).show()
        }
    }

    Column(Modifier.fillMaxSize()) {
        // The same header geometry the hand-rolled row used (start 20, end 8,
        // top 12), so the page does not move. No subtitle (removed on request,
        // 2026-10-17): the line count was a second grey line under the title for a
        // number that is already visible by counting, and the paused state it used
        // to carry is now said by the banner under the header, which is louder and
        // appears exactly when it is true.
        DetourPageHeader(
            title = stringResource(R.string.logs_title),
            actions = {
                IconButton(
                    onClick = {
                        if (!paused) frozen = all
                        paused = !paused
                    },
                ) {
                    // Crossfade the glyph: the two states are unrelated shapes
                    // (play vs. pause bars), and an instant swap reads as a redraw
                    // rather than a toggle.
                    Crossfade(
                        targetState = paused,
                        animationSpec = motion.effects,
                        label = "logs-pause-icon",
                    ) { isPaused ->
                        Icon(
                            if (isPaused) Icons.Filled.PlayArrow else DetourIcons.Pause,
                            contentDescription = stringResource(
                                if (isPaused) R.string.logs_resume else R.string.logs_pause,
                            ),
                        )
                    }
                }
                // Pause stays in the header; the other three move into a menu.
                // Pause is the one control used *while reading*, so burying it a
                // tap deeper would cost on the common path. The other three are
                // occasional, and two of them are destructive or wide in scope —
                // exactly the kind of thing a menu can label in words.
                Box {
                    IconButton(onClick = { menuOpen = true }) {
                        Icon(
                            Icons.Filled.MoreVert,
                            contentDescription = stringResource(R.string.logs_menu_more),
                        )
                    }
                    DropdownMenu(
                        expanded = menuOpen,
                        onDismissRequest = { menuOpen = false },
                    ) {
                        // The scope is in the label, because the two copy/export
                        // actions disagree about it: this one is the filtered view,
                        // the next is the whole archive. Adjacent icons never said
                        // which was which.
                        DropdownMenuItem(
                            text = {
                                Text(stringResource(R.string.logs_copy_view, visible.size))
                            },
                            enabled = visible.isNotEmpty(),
                            onClick = {
                                menuOpen = false
                                copyAll()
                            },
                        )
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.logs_export_archive)) },
                            onClick = {
                                menuOpen = false
                                exportLogs()
                            },
                        )
                        HorizontalDivider()
                        DropdownMenuItem(
                            text = {
                                Text(
                                    stringResource(R.string.logs_clear),
                                    color = MaterialTheme.colorScheme.error,
                                )
                            },
                            onClick = {
                                menuOpen = false
                                confirmClear = true
                            },
                        )
                    }
                }
            },
        )

        // The paused state, said out loud. A banner rather than a tint or a badge:
        // the consequence of pausing is that the list stops changing, and the one
        // thing a stopped list cannot do is tell you why it stopped.
        if (paused) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = spacing.screenH, vertical = spacing.xs),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(
                    DetourIcons.Pause,
                    contentDescription = null,
                    modifier = Modifier.size(spacing.lg),
                    tint = MaterialTheme.colorScheme.tertiary,
                )
                Spacer(Modifier.width(spacing.sm))
                Text(
                    stringResource(R.string.logs_paused_banner),
                    modifier = Modifier.weight(1f),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                TextButton(onClick = { paused = false }) {
                    Text(stringResource(R.string.logs_resume))
                }
            }
        }

        // Search and the filter share one row. They narrow the same list, so they
        // are one control group; the chips used to take a row of their own for
        // four values, and the search box below them read as a second, unrelated
        // control rather than the finer pass over what the chips left.
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = spacing.screenH),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(spacing.sm),
        ) {
            DetourSearchField(
                value = query,
                onValueChange = { query = it },
                placeholder = stringResource(R.string.logs_search_hint),
                modifier = Modifier.weight(1f),
            )
            Box {
                DetourFilterChip(
                    selected = filter != Filter.ALL,
                    onClick = { filterMenuOpen = true },
                    label = stringResource(filter.labelRes),
                )
                DropdownMenu(
                    expanded = filterMenuOpen,
                    onDismissRequest = { filterMenuOpen = false },
                ) {
                    Filter.entries.forEach { option ->
                        DropdownMenuItem(
                            text = { Text(stringResource(option.labelRes)) },
                            onClick = {
                                filter = option
                                filterMenuOpen = false
                            },
                            trailingIcon = if (filter == option) {
                                { Icon(Icons.Filled.Check, contentDescription = null) }
                            } else {
                                null
                            },
                        )
                    }
                }
            }
        }

        Spacer(Modifier.height(spacing.sm))

        if (visible.isEmpty()) {
            // Three different reasons the list can be empty, and each needs its
            // own answer — the first two would both be lies if they shared one.
            //
            // The order is load-bearing. A paused screen whose snapshot froze empty
            // is *also* `source`-empty, so the pause case has to be tested before
            // the "filtered everything out" case; tested the other way round it
            // falls through to `logs_empty`, which is exactly the false "还没有日志"
            // this branch exists to remove. All three read `sourceEmpty` (and its
            // negation) rather than `visible`, which is empty by definition here.
            val sourceEmpty = source.isEmpty()
            val pausedButBufferHasLogs = paused && sourceEmpty && all.isNotEmpty()
            // One judgement for both lines. Title and detail each deciding for
            // themselves is how a screen ends up saying "没有匹配" over "连接一次后…".
            val (titleRes, detailRes) = when {
                // Paused before any line arrived: the snapshot is empty while the
                // buffer is not. Not "no logs yet" — there are logs, the view is
                // simply frozen.
                pausedButBufferHasLogs ->
                    R.string.logs_paused_empty to R.string.logs_paused_empty_detail
                // Logs exist but the user's own chip or search excluded them all.
                !sourceEmpty -> R.string.logs_no_match to R.string.logs_no_match_detail
                // Genuinely nothing has been logged yet.
                else -> R.string.logs_empty to R.string.logs_empty_detail
            }
            // No `modifier`: `DetourEmptyState` fills and centres itself, so
            // passing `fillMaxSize` here would be the same instruction twice.
            DetourEmptyState(
                title = stringResource(titleRes),
                detail = stringResource(detailRes),
                icon = DetourIcons.Subject,
            )
            return@Column
        }

        Box(Modifier.fillMaxSize()) {
            LazyColumn(
                state = listState,
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(
                    start = spacing.screenH,
                    end = spacing.screenH,
                    top = spacing.xs,
                    // The clearance is added, never substituted, so the list
                    // keeps the breathing room it already had and the bar's
                    // height lands on top of it rather than eating into it.
                    bottom = spacing.xl.plusBarClearance(barClearance),
                ),
                // The log list is a density flow, not a card list: 4 dp between
                // lines, deliberately tighter than the 12 dp the card pages use.
                // It maps to `xs`; widening it to `md` would make the page taller,
                // which is the opposite of what this screen is for.
                verticalArrangement = Arrangement.spacedBy(spacing.xs),
            ) {
                // Keyed by position in the visible snapshot rather than by a
                // timestamp-plus-hash. The service emits a sample twice a second and
                // a repeated message is ordinary, so two lines can share a
                // millisecond *and* a message — and `LazyColumn` throws on a repeated
                // key. Position is unique by construction and stable for a given
                // snapshot, which is all the list needs: entries only ever arrive at
                // the front or the whole list is replaced.
                itemsIndexed(visible, key = { position, _ -> position }) { index, entry ->
                    // A day header before the first line of each day. The list is
                    // newest-first, so the headers read top-down as today, then
                    // yesterday — the direction someone scrolling for "where did
                    // today start" is already moving.
                    if (dayStarts[index] != dayStarts.getOrNull(index - 1)) {
                        DayHeader(dayStarts[index], todayStart)
                    }
                    LogRow(
                        entry,
                        isToday = entry.atMillis >= todayStart,
                        onLongPress = { copyEntry(entry) },
                    )
                }
            }

            // The entry point to the live tail that the no-auto-scroll rule
            // (see the file header) otherwise leaves out. It shows only once
            // there is something above the viewport to come back from, so at the
            // top — where it would do nothing — it is simply absent.
            //
            // `canScrollBackward` rather than `firstVisibleItemIndex > 0`: they
            // answer the same question ("has the list moved off its top edge"),
            // but the index is an `Int` that changes on every item and the
            // offset, if added, changes on every scrolled pixel — each read in
            // composition and so each a recomposition of this whole screen. The
            // boolean flips once per crossing, which is the only moment the
            // button's visibility actually changes.
            if (listState.canScrollBackward) {
                // Newest is index 0 (the list is newest-first), so "latest" is
                // the top of the list.
                DetourButton(
                    onClick = { scope.launch { listState.animateScrollToItem(0) } },
                    modifier = Modifier
                        .align(Alignment.BottomCenter)
                        // Lifted by the bar's clearance on top of its own `xl`
                        // margin. The bar is anchored to the same bottom-centre,
                        // so at the original offset the two sat on exactly the
                        // same spot and the button was unreachable behind it.
                        .padding(bottom = spacing.xl.plusBarClearance(barClearance)),
                    variant = DetourButtonVariant.Tonal,
                ) {
                    Text(stringResource(R.string.logs_back_to_latest))
                }
            }
        }
    }

    // Outside the Column, like the dialog on the rules screen: it is an overlay
    // on the whole screen, not a row in the list. The title reuses the settings
    // screen's "清空全部日志" because it is literally the same action and the two
    // screens should not word it two ways; the body and the dismiss label are this
    // screen's own, so the dialog no longer borrows the rules page's "取消".
    if (confirmClear) {
        DetourAlertDialog(
            onDismissRequest = { confirmClear = false },
            title = stringResource(R.string.settings_clear_logs),
            text = { Text(stringResource(R.string.logs_clear_confirm_detail)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        KernelState.clearLogs()
                        frozen = emptyList()
                        confirmClear = false
                    },
                ) {
                    Text(stringResource(R.string.logs_clear))
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmClear = false }) {
                    Text(stringResource(R.string.common_cancel))
                }
            },
        )
    }
}

private enum class Filter(val labelRes: Int) {
    ALL(R.string.logs_filter_all),
    SESSION(R.string.logs_filter_session),
    // Named WARN, not ERROR, because that is what it selects: the chip used to say
    // 「错误」 while matching `ERROR || WARN`, so picking it showed warnings under a
    // label that promised errors. The label moved to the predicate, not the other
    // way round — a warning is a thing a person debugging wants to see.
    WARN(R.string.logs_filter_warn),
    DNS(R.string.logs_filter_dns);

    fun matches(entry: KernelState.LogEntry): Boolean = when (this) {
        ALL -> true
        SESSION -> entry.tag == "DetourVpn" || entry.tag == "DetourTunnel"
        WARN -> entry.level == KernelState.LogEntry.Level.ERROR ||
            entry.level == KernelState.LogEntry.Level.WARN
        // Tag only. The old predicate also matched `message.contains("DNS")`, which
        // pulled in every line that mentioned DNS in passing — a connect log that
        // says "DNS 已就绪" is not a DNS event, and it made this chip mean "lines
        // containing the word" rather than "lines from the resolver".
        DNS -> entry.tag.contains("dns", ignoreCase = true)
    }
}

/**
 * The two fields a person searches by, matched case-insensitively as a substring.
 *
 * Not the level, and not the timestamp: the level already has a chip, and nobody
 * types "09-26" into a log search box. Tag and message are what is on the line.
 */
private fun KernelState.LogEntry.matchesQuery(needle: String): Boolean =
    tag.contains(needle, ignoreCase = true) || message.contains(needle, ignoreCase = true)

/**
 * `HH:mm:ss.SSS` for a line from today, `MM-dd HH:mm:ss.SSS` for anything older.
 *
 * Two formatters rather than one with a conditional prefix, because the prefix
 * would itself need a date formatter — and two `SimpleDateFormat`s built once at
 * class-load are cheaper than composing a string per line.
 *
 * The date appears only when it is not today, which is the case that actually
 * matters: within one day the ordering is already the list's, and printing
 * `09-27` on four hundred consecutive lines is noise. A line from yesterday is
 * where a bare `23:58` starts to lie about which `09:00` it is near.
 */
private val timeFormat = SimpleDateFormat("HH:mm:ss.SSS", Locale.US)
private val dateTimeFormat = SimpleDateFormat("MM-dd HH:mm:ss.SSS", Locale.US)
private val dayFormat = SimpleDateFormat("MM-dd", Locale.US)

/**
 * Width of the severity strip on a log row.
 *
 * A drawing dimension, not a spacing step: it is not a gap between two things, so
 * it does not belong on the spacing scale — the same reasoning that keeps
 * `DetourEmptyState`'s 48 dp circle and the console's 180 dp height off it.
 */
private val LEVEL_BAR_WIDTH = 3.dp

private fun stamp(entry: KernelState.LogEntry, isToday: Boolean): String =
    (if (isToday) timeFormat else dateTimeFormat).format(Date(entry.atMillis))

/**
 * Midnight of [millis]'s day, local time, in epoch millis.
 *
 * `Calendar` rather than a fixed offset from the timestamp, which is the tempting
 * shortcut and is wrong twice: it ignores the device's time zone, and it would
 * break on a day that is not 24 hours long (a DST transition), which is precisely
 * the day someone is most likely to be reading a log across.
 */
private fun startOfDay(millis: Long): Long = Calendar.getInstance().apply {
    timeInMillis = millis
    set(Calendar.HOUR_OF_DAY, 0)
    set(Calendar.MINUTE, 0)
    set(Calendar.SECOND, 0)
    set(Calendar.MILLISECOND, 0)
}.timeInMillis

private fun startOfToday(): Long = startOfDay(System.currentTimeMillis())

/**
 * The day separator between two runs of log lines.
 *
 * `今天` / `昨天` rather than a date, because those are the two the reader is
 * actually locating; anything older gets `MM-dd`, which is the same shape the
 * per-line stamp uses once a line is not from today.
 *
 * "Yesterday" is derived as `startOfDay(todayStart - 1)` rather than
 * `todayStart - 86_400_000`: on a DST day the two differ, and subtracting a day of
 * milliseconds lands on 23:00 the previous day, which then floors to the same
 * previous day only by luck.
 */
@Composable
private fun DayHeader(dayStart: Long, todayStart: Long) {
    val spacing = LocalDetourSpacing.current
    val label = when (dayStart) {
        todayStart -> stringResource(R.string.logs_day_today)
        startOfDay(todayStart - 1) -> stringResource(R.string.logs_day_yesterday)
        else -> dayFormat.format(Date(dayStart))
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = spacing.sm, bottom = spacing.xs),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.width(spacing.sm))
        HorizontalDivider(
            modifier = Modifier.weight(1f),
            color = MaterialTheme.colorScheme.outlineVariant,
        )
    }
}

/**
 * The tag as a person reads it.
 *
 * The stored tag is the emitting class (`DetourVpn`, `DnsRedirect`), which is the
 * right identifier to put in a copied log and the wrong thing to put in a column of
 * a list: it is noise on every line, and it is the same width as the message.
 * Unknown tags are shown as themselves — the set belongs to the app, not to this
 * build, and a new one should be legible before it is translated.
 */
@Composable
private fun tagLabel(tag: String): String = when (tag) {
    "DetourVpn" -> stringResource(R.string.log_tag_connection)
    "DetourTunnel" -> stringResource(R.string.log_tag_tunnel)
    "DnsRedirect" -> stringResource(R.string.log_tag_dns)
    "Rules" -> stringResource(R.string.log_tag_rules)
    "DetourApp" -> stringResource(R.string.log_tag_app)
    "DetourKernel" -> stringResource(R.string.log_tag_kernel)
    "DetourRoot" -> stringResource(R.string.log_tag_root)
    else -> tag
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun LogRow(
    entry: KernelState.LogEntry,
    isToday: Boolean,
    onLongPress: () -> Unit,
) {
    val spacing = LocalDetourSpacing.current
    val tint: Color = when (entry.level) {
        KernelState.LogEntry.Level.ERROR -> MaterialTheme.colorScheme.error
        KernelState.LogEntry.Level.WARN -> MaterialTheme.colorScheme.tertiary
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    // A second, non-colour channel for the severity. The tint alone carries the
    // level, which is fine for someone who can tell the two hues apart and useless
    // for someone who cannot — and this is the one screen where a person scans for
    // "what went wrong" rather than reads top to bottom. The bar is additive: the
    // tint stays exactly as it was, and INFO gets no bar at all, so the two levels
    // that matter are the two that stand out.
    val bar: Color? = when (entry.level) {
        KernelState.LogEntry.Level.ERROR -> MaterialTheme.colorScheme.error
        KernelState.LogEntry.Level.WARN -> MaterialTheme.colorScheme.tertiary
        else -> null
    }
    DetourCard(
        // Long-press rather than tap: the row is a wall of text and a tap has no
        // obvious meaning here, while "hold to copy this line" is the gesture
        // the list was missing. `combinedClickable` with an empty `onClick`
        // keeps the press ripple without inventing a tap action, and the
        // modifier is passed positionally because `DetourCard`'s `onClick` is a
        // separate, named parameter we deliberately leave unset. The label is
        // the only place the gesture is announced, since a hint drawn on the row
        // would be a hint on every row.
        // Glass, per the owner's "与其它页统一". This is the hardest place to keep
        // legible — a dense list of small cards, each carrying the glass
        // highlight — which is exactly why every text colour in this row already
        // comes from the theme (`onSurfaceVariant`, and the level `tint`) rather
        // than being chosen for a light background: the tint behind these lines
        // is `surfaceContainer` at 72%, so a hard-coded dark grey would fall
        // apart the moment a wallpaper showed through.
        Modifier.combinedClickable(
            onClick = {},
            onLongClickLabel = stringResource(R.string.logs_copy),
            onLongClick = onLongPress,
        ),
        style = DetourCardStyle.Glass,
    ) {
        // One line to start, rather than the two the old stamp-over-message-over-
        // tag stack always needed. The time, the tag and the message share the
        // row, and only the message flexes (`weight(1f)`): the time and the tag
        // are the two fields with a fixed shape, so they size to their content
        // and the message takes whatever is left. A short line — most of them —
        // is then a single row of height instead of two.
        //
        // The message wraps *inside its own column* (`maxLines = 2`), so a long
        // line grows downward and never pushes the tag off the row, and the tag
        // is never truncated — a half tag is worse than none. The level is still
        // carried by the message's colour alone (the `tint` above), exactly as
        // the stacked layout carried it; nothing about the severity moved. The
        // two separators use `md` (12 dp), which is the gap the old row's single
        // zero-size `Spacer` with 6 dp horizontal padding actually produced.
        Row(
            modifier = Modifier
                // Drawn before the padding, so the strip sits on the card's own left
                // edge rather than at the start of the text.
                .drawBehind {
                    bar?.let { drawRect(it, size = Size(LEVEL_BAR_WIDTH.toPx(), size.height)) }
                }
                .padding(horizontal = spacing.md, vertical = spacing.sm),
            verticalAlignment = Alignment.Top,
        ) {
            Text(
                stamp(entry, isToday),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.width(spacing.md))
            Text(
                tagLabel(entry.tag),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.width(spacing.md))
            Text(
                entry.message,
                modifier = Modifier.weight(1f),
                style = MaterialTheme.typography.bodySmall,
                color = tint,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}
