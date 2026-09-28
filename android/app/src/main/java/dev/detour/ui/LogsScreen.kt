package dev.detour.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.widget.Toast
import androidx.compose.animation.Crossfade
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
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
import dev.detour.ui.components.LocalBottomBarClearance
import dev.detour.ui.components.plusBarClearance
import dev.detour.ui.icons.DetourIcons
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

    val listState = rememberLazyListState()
    val scope = rememberCoroutineScope()

    // How much room the floating glass bar occupies at the bottom of this
    // screen. It used to be a `Scaffold` `bottomBar`, which reserved that space
    // for every screen for free; now that it floats over the content, this
    // screen has to leave the room itself. Read once here, in composition, and
    // reused by both consumers below — the list's bottom padding and the
    // bottom-anchored button — so the two cannot drift to different heights.
    val barClearance = LocalBottomBarClearance.current

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

    Column(Modifier.fillMaxSize()) {
        // The same header geometry the hand-rolled row used (start 20, end 8,
        // top 12), so the page does not move. The count in the subtitle is the
        // visible one, not the buffer's: after a filter or a search, "how many
        // lines am I looking at" is the useful number, and it is the same number
        // copy-all acts on.
        DetourPageHeader(
            title = stringResource(R.string.logs_title),
            subtitle = stringResource(R.string.logs_count, visible.size),
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
                    Crossfade(targetState = paused, label = "logs-pause-icon") { isPaused ->
                        Icon(
                            if (isPaused) Icons.Filled.PlayArrow else DetourIcons.Pause,
                            contentDescription = stringResource(
                                if (isPaused) R.string.logs_resume else R.string.logs_pause,
                            ),
                        )
                    }
                }
                // Disabled rather than hidden with nothing to copy: the button
                // staying put keeps the four actions in the same places as the
                // filter empties and refills the list, so the next tap lands
                // where the last one did.
                IconButton(onClick = copyAll, enabled = visible.isNotEmpty()) {
                    Icon(
                        DetourIcons.Copy,
                        contentDescription = stringResource(R.string.logs_copy_all),
                    )
                }
                // Clearing is the only action here that cannot be undone, and
                // the header announces the icons by glyph alone. A confirmation
                // is the cheap half of that problem — a tooltip would explain
                // the button but not protect the data.
                IconButton(onClick = { confirmClear = true }) {
                    Icon(
                        Icons.Filled.Delete,
                        contentDescription = stringResource(R.string.logs_clear),
                    )
                }
                IconButton(
                    onClick = {
                        val out = java.io.File(context.cacheDir, "logs-export.txt")
                        // Prefer the archive — the full history that survives a
                        // restart — and fall back to the in-memory list, which is
                        // only what is on screen and is capped at 500 lines.
                        val archive = LogArchive.file(context)
                        val fromArchive = archive.isFile && archive.length() > 0
                        if (!fromArchive && KernelState.logs.value.isEmpty()) {
                            Toast.makeText(context, exportEmpty, Toast.LENGTH_SHORT).show()
                            return@IconButton
                        }
                        runCatching {
                            if (fromArchive) {
                                archive.copyTo(out, overwrite = true)
                            } else {
                                // Same `atMillis<TAB>level<TAB>tag<TAB>message` shape
                                // as the archive writes, so the two sources produce
                                // interchangeable files.
                                out.writeText(
                                    KernelState.logs.value.joinToString(
                                        separator = "\n",
                                        postfix = "\n",
                                    ) {
                                        "${it.atMillis}\t${it.level.name}\t${it.tag}\t${it.message}"
                                    },
                                )
                            }
                            // A FileProvider rather than `EXTRA_TEXT`: the archive
                            // can reach 2 MB (`LogArchive.MAX_BYTES`), and an Intent
                            // extra over ~1 MB dies with
                            // `TransactionTooLargeException`. The authority must
                            // match the manifest's `${applicationId}.fileprovider`.
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
                            // The format argument is only known here, at failure
                            // time, so `getString` rather than a captured
                            // `stringResource`.
                            Toast.makeText(
                                context,
                                context.getString(
                                    R.string.logs_export_failed,
                                    it.message ?: "未知错误",
                                ),
                                Toast.LENGTH_SHORT,
                            ).show()
                        }
                    },
                ) {
                    Icon(
                        Icons.Filled.Share,
                        contentDescription = stringResource(R.string.logs_export_all),
                    )
                }
            },
        )

        // Under the header and above the chips: the search narrows what the chips
        // already narrowed, so it reads as the second, finer control.
        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 20.dp),
            singleLine = true,
            leadingIcon = { Icon(Icons.Filled.Search, contentDescription = null) },
            placeholder = { Text(stringResource(R.string.logs_search_hint)) },
        )

        Spacer(Modifier.height(8.dp))

        Row(
            modifier = Modifier.padding(horizontal = 20.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Filter.entries.forEach { option ->
                DetourFilterChip(
                    selected = filter == option,
                    onClick = { filter = option },
                    label = stringResource(option.labelRes),
                )
            }
        }

        Spacer(Modifier.height(8.dp))

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
                    start = 20.dp,
                    end = 20.dp,
                    top = 4.dp,
                    // The clearance is added, never substituted, so the list
                    // keeps the breathing room it already had and the bar's
                    // height lands on top of it rather than eating into it.
                    bottom = 24.dp.plusBarClearance(barClearance),
                ),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                // Keyed by position in the visible snapshot rather than by a
                // timestamp-plus-hash. The service emits a sample twice a second and
                // a repeated message is ordinary, so two lines can share a
                // millisecond *and* a message — and `LazyColumn` throws on a repeated
                // key. Position is unique by construction and stable for a given
                // snapshot, which is all the list needs: entries only ever arrive at
                // the front or the whole list is replaced.
                itemsIndexed(visible, key = { position, _ -> position }) { _, entry ->
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
                        // Lifted by the bar's clearance on top of its own 24.dp
                        // margin. The bar is anchored to the same bottom-centre,
                        // so at the original offset the two sat on exactly the
                        // same spot and the button was unreachable behind it.
                        .padding(bottom = 24.dp.plusBarClearance(barClearance)),
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
    ERROR(R.string.logs_filter_error),
    DNS(R.string.logs_filter_dns);

    fun matches(entry: KernelState.LogEntry): Boolean = when (this) {
        ALL -> true
        SESSION -> entry.tag == "DetourVpn" || entry.tag == "DetourTunnel"
        ERROR -> entry.level == KernelState.LogEntry.Level.ERROR ||
            entry.level == KernelState.LogEntry.Level.WARN
        DNS -> entry.message.contains("DNS") || entry.tag.contains("Dns", ignoreCase = true)
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

private fun stamp(entry: KernelState.LogEntry, isToday: Boolean): String =
    (if (isToday) timeFormat else dateTimeFormat).format(Date(entry.atMillis))

/**
 * Midnight of the current day, local time, in epoch millis.
 *
 * `Calendar` rather than a fixed offset from `System.currentTimeMillis()`, which
 * is the tempting shortcut and is wrong twice: it ignores the device's time zone,
 * and it would break on a day that is not 24 hours long (a DST transition), which
 * is precisely the day someone is most likely to be reading a log across.
 */
private fun startOfToday(): Long = Calendar.getInstance().apply {
    set(Calendar.HOUR_OF_DAY, 0)
    set(Calendar.MINUTE, 0)
    set(Calendar.SECOND, 0)
    set(Calendar.MILLISECOND, 0)
}.timeInMillis

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun LogRow(
    entry: KernelState.LogEntry,
    isToday: Boolean,
    onLongPress: () -> Unit,
) {
    val tint: Color = when (entry.level) {
        KernelState.LogEntry.Level.ERROR -> MaterialTheme.colorScheme.error
        KernelState.LogEntry.Level.WARN -> MaterialTheme.colorScheme.tertiary
        else -> MaterialTheme.colorScheme.onSurfaceVariant
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
        Row(Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
            Text(
                stamp(entry, isToday),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.padding(horizontal = 6.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    entry.message,
                    style = MaterialTheme.typography.bodySmall,
                    color = tint,
                    maxLines = 3,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    entry.tag,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}
