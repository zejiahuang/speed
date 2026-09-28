package dev.detour.core

import android.content.Context
import java.io.File

/**
 * The on-disk log archive.
 *
 * The in-memory log list is bounded at 500 lines and is gone when the process
 * dies — which is exactly the moment someone wants to read it, because the crash
 * that killed the process is the thing they are looking for. When "按天归档" is on,
 * every line is also appended to a file that survives a restart; when it is off,
 * nothing is written at all.
 *
 * **The setting is checked at write time, not at install time.** Reading it once
 * when the sink is installed would mean toggling the switch did nothing until the
 * next cold start, which is the same "the control moves and nothing else does"
 * failure this whole round exists to remove.
 *
 * The archive is append-only, one line per entry, `atMillis<TAB>level<TAB>tag<TAB>
 * message`. Tab-separated rather than JSON because the message is free text that
 * may contain anything, and escaping it correctly on every line is a bug waiting
 * to happen; the fields before it never contain a tab.
 */
object LogArchive {

    private const val NAME = "logs-archive.log"

    /**
     * Rewrite threshold, in bytes.
     *
     * A cap rather than a date index: the file is append-only so that a log line
     * never costs more than one write, and a restart never has to rewrite it.
     * When it is crossed the oldest half is dropped — once, then not again for a
     * long while.
     */
    private const val MAX_BYTES = 2L * 1024 * 1024

    /** How many of the newest lines survive a rewrite. */
    private const val KEEP_LINES = 20_000

    fun file(context: Context): File = File(context.filesDir, NAME)

    /**
     * Route every log line into the archive.
     *
     * Installed once, from the Application, before the first line is written so
     * that the kernel-load result is archived too. Installing twice would double
     * every line, so the Application is the only caller.
     */
    fun install(context: Context) {
        val app = context.applicationContext
        KernelState.archive = { entry ->
            // Read on every line, on purpose: the switch has to take effect now.
            if (Prefs.of(app).logArchive) append(app, entry)
        }
    }

    /**
     * Append one line.
     *
     * `@Synchronized` because `log()` is called from more than one thread — the
     * service's step coroutine, the main thread at startup, and whichever thread a
     * rule load runs on — and two unsynchronised `appendText` calls can interleave
     * mid-line. The lock is per-object and the work under it is one small write.
     */
    @Synchronized
    private fun append(context: Context, entry: KernelState.LogEntry) {
        val target = file(context)
        // A failure to archive is never a reason to lose a log line or crash the
        // tunnel: storage can be full, and the in-memory list still has the entry.
        runCatching {
            if (target.length() > MAX_BYTES) {
                val kept = target.readLines().takeLast(KEEP_LINES)
                target.writeText(kept.joinToString(separator = "\n", postfix = "\n"))
            }
            target.appendText(
                "${entry.atMillis}\t${entry.level.name}\t${entry.tag}\t${entry.message}\n",
            )
        }
    }

    /** Drop the archive. The in-memory list is [KernelState.clearLogs]'s job. */
    fun clear(context: Context) {
        runCatching { file(context).delete() }
    }

    /** Size in bytes, for the control surface and the dump. */
    fun sizeBytes(context: Context): Long = file(context).length()
}
