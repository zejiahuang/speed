package dev.detour.core

import android.content.Context
import android.os.Build
import dev.detour.BuildConfig
import dev.detour.R
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * What a failed connection leaves behind: a file, and a sentence about what to
 * try next.
 *
 * The message the kernel hands back is precise and written for someone who knows
 * what a rule document is; it is also the only thing this app had when a start
 * failed, and it is gone from the screen the moment the user taps connect again.
 * Two things are added here:
 *
 * * **A file.** It carries the state the message came out of — mode, rule source
 *   and cache age, the kernel settings, the last lines of the log — because
 *   "connection failed" on its own cannot be diagnosed after the fact, and by the
 *   time anyone asks, the state has changed.
 * * **A suggestion chosen by cause, not by wording.** [Reason] is passed in by the
 *   caller that failed, so the advice does not depend on matching a message that
 *   may have been translated.
 *
 * The report goes to `files/error-reports/` rather than the cache: the cache is
 * evictable, and a report that disappears is the failure mode this exists to
 * prevent. `res/xml/file_paths.xml` exposes exactly that one directory to the
 * share sheet, so nothing else under `files/` becomes reachable.
 */
object ErrorReport {

    /**
     * Why a start failed. The caller knows; the message alone does not — it is a
     * kernel or system string, and telling "no rules" apart from "no permission"
     * by matching its text would break the moment either was translated.
     */
    enum class Reason {
        /** The rule document could not be fetched, parsed, or found. */
        RULES,

        /** The system refused the VPN authorisation. */
        PERMISSION,

        /** No usable `su`. */
        ROOT_UNAVAILABLE,

        /** `su` ran but the takeover could not be installed. */
        ROOT_INSTALL,

        /** The local reverse proxy did not come up. */
        MITM,

        /** The kernel started and then reported an error. */
        KERNEL,

        /** Anything else. */
        UNKNOWN,
    }

    /** How many reports to keep. One per failed attempt would grow without bound. */
    private const val KEEP = 10

    /** How much of the log to include. The log screen holds [KernelState.MAX_LOG_LINES]. */
    private const val LOG_LINES = 60

    /** The sentence shown under the error on the home screen. */
    fun suggestionRes(reason: Reason): Int = when (reason) {
        Reason.RULES -> R.string.error_hint_rules
        Reason.PERMISSION -> R.string.error_hint_permission
        Reason.ROOT_UNAVAILABLE -> R.string.error_hint_root_unavailable
        Reason.ROOT_INSTALL -> R.string.error_hint_root_install
        Reason.MITM -> R.string.error_hint_mitm
        Reason.KERNEL -> R.string.error_hint_kernel
        Reason.UNKNOWN -> R.string.error_hint_unknown
    }

    /** The directory the reports live in. */
    fun dir(context: Context): File = File(context.filesDir, "error-reports")

    /**
     * Write one report and return it, or `null` if the write failed.
     *
     * Called from the service's failure path, which is already on its way down —
     * so nothing here throws: a device whose storage is full must still be able to
     * report that the tunnel failed.
     */
    fun write(
        context: Context,
        mode: KernelState.Mode,
        message: String,
        reason: Reason,
    ): File? = runCatching {
        val dir = dir(context)
        if (!dir.isDirectory && !dir.mkdirs()) return null
        val stamp = SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US).format(Date())
        val file = File(dir, "speed-error-$stamp.txt")
        file.writeText(text(context, mode, message, reason), Charsets.UTF_8)
        // Newest [KEEP], by the name's own ordering — the stamp is the sort key.
        dir.listFiles()?.sortedByDescending { it.name }?.drop(KEEP)?.forEach { it.delete() }
        file
    }.getOrNull()

    /** The report's text. Separate from [write] so it can be tested without a file. */
    fun text(
        context: Context,
        mode: KernelState.Mode,
        message: String,
        reason: Reason,
    ): String {
        val prefs = Prefs.of(context)
        val source = RuleSource.forMode(mode, prefs.selectedSource)
        val cache = RulesRepository.cacheFile(context)
        return buildString {
            appendLine(context.getString(R.string.report_title) + " · " + BuildConfig.VERSION_NAME +
                " (" + BuildConfig.VERSION_CODE + ")")
            appendLine(context.getString(R.string.report_time) + ": " + stamp())
            appendLine(context.getString(R.string.about_device) + ": " + Build.MANUFACTURER + " " +
                Build.MODEL + " · Android " + Build.VERSION.RELEASE + " (API " + Build.VERSION.SDK_INT + ")")
            appendLine(context.getString(R.string.report_mode) + ": " + mode.name.lowercase())
            appendLine(context.getString(R.string.report_reason) + ": " + reason.name.lowercase())
            appendLine(context.getString(R.string.report_error) + ": " + message)
            appendLine()
            appendLine("--- " + context.getString(R.string.report_rules) + " ---")
            appendLine(context.getString(R.string.home_rule_source_label) + ": " + source.id)
            appendLine(
                context.getString(R.string.report_cache) + ": " +
                    if (cache.isFile) {
                        (cache.length() / 1024) .toString() + " KiB · " +
                            SimpleDateFormat("yyyy-MM-dd HH:mm", Locale.US).format(Date(cache.lastModified()))
                    } else {
                        context.getString(R.string.home_rule_cache_none)
                    },
            )
            if (mode == KernelState.Mode.VPN) {
                appendLine()
                appendLine("--- " + context.getString(R.string.report_settings) + " ---")
                appendLine(prefs.kernelSettingsJson())
            }
            appendLine()
            appendLine("--- " + context.getString(R.string.report_logs) + " (" + LOG_LINES + ") ---")
            val fmt = SimpleDateFormat("HH:mm:ss.SSS", Locale.US)
            KernelState.logs.value.take(LOG_LINES).asReversed().forEach {
                appendLine(
                    fmt.format(Date(it.atMillis)) + " " + it.level.name + " " + it.tag + "  " + it.message,
                )
            }
        }
    }

    private fun stamp(): String =
        SimpleDateFormat("yyyy-MM-dd HH:mm:ss Z", Locale.US).format(Date())
}
