package dev.detour.core

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.detour.BuildConfig
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * In-process state for the update check, and the **only** entry point that runs
 * one.
 *
 * **Why this object exists instead of letting the About screen and the startup
 * path each do their own check.** There are two triggers — the silent check at
 * process start and the manual button on the About screen — and they do the same
 * thing: read the same address, make the same blocking network call, write the
 * result to the same place. Written twice, the "is a check in flight" test, the
 * timestamp written on failure, and the "is there something newer" decision would
 * eventually diverge, and divergence here looks like "the manual check says there
 * is an update and the startup check says there is not" — a contradiction nobody
 * can explain from the outside. So a check has exactly one entry point, [run], and
 * both triggers call it.
 *
 * The state is Compose `mutableStateOf` rather than a `KernelState`-style
 * `StateFlow`: the only readers are the About screen and `MainActivity`'s dialog
 * test, both of which are Compose, so they can read a property directly and there
 * is no reason to write a collector for them.
 */
object UpdateState {

    /**
     * The throttle window for the automatic check: 24 hours.
     *
     * The startup check means "take a quick look when the app opens", not "send a
     * request on every launch". Without this window, someone who opens the app ten
     * times a day sends ten requests, and GitHub's Releases API rate-limits
     * anonymous callers — spending that budget on repeat requests whose answer
     * cannot have changed is how the one check that matters gets limited. 24 hours
     * matches the rule document's default refresh interval because the two are the
     * same kind of thing: remote content that changes on the scale of days, not of
     * launches.
     */
    const val MIN_AUTO_CHECK_INTERVAL_MS = 24L * 60 * 60 * 1000

    /** The most recent result; `null` means this process has not checked yet. */
    var result: UpdateChecker.Result? by mutableStateOf(null)
        private set

    /** Whether a check is in flight. The manual button greys itself out on this. */
    var checking: Boolean by mutableStateOf(false)
        private set

    /**
     * Whether the "a new version is available" dialog has been shown in this
     * process.
     *
     * **Deliberately in-process and not persisted, and that is not laziness.** The
     * dialog means "a reminder, this time" — not "never remind me again". When the
     * user taps 稍后 they mean "not right now", not "stop telling me". Persist this
     * and the default flips to "dismiss it once and never hear about an update
     * again", and a broken update notice is the kind of failure the user cannot
     * detect: they do not know what they were not told. That is the last state a
     * single stray tap should be able to decide. The cost of keeping it in-process
     * is at most one dialog per cold start, which is exactly the frequency a
     * reminder should have.
     */
    var promptShown: Boolean by mutableStateOf(false)
        private set

    /**
     * Whether the "a new version is available" dialog should be shown.
     *
     * Both conditions are required: an [UpdateChecker.Result.Available] result, and
     * a process that has not shown the dialog yet. `promptShown` is tested first so
     * the common "already shown" path short-circuits before the type test.
     */
    val shouldPrompt: Boolean get() = !promptShown && result is UpdateChecker.Result.Available

    fun markPromptShown() {
        promptShown = true
    }

    /**
     * Whether the startup check should run.
     *
     * Two conditions: the user has not switched auto-check off, and at least
     * [MIN_AUTO_CHECK_INTERVAL_MS] has passed since the last attempt. This reads
     * `lastUpdateCheckAt` (the last *attempt*) rather than a "last success"
     * timestamp because what the throttle has to prevent is frequent requests, and
     * a failed request spends the same rate-limit budget — see the note on write
     * timing in [run].
     */
    fun shouldAutoCheck(prefs: Prefs): Boolean =
        prefs.autoCheckUpdate &&
            System.currentTimeMillis() - prefs.lastUpdateCheckAt >= MIN_AUTO_CHECK_INTERVAL_MS

    /**
     * Runs one check. The silent startup check and the About screen's manual button
     * **both** go through it.
     *
     * **Why the blocking network call must move to `Dispatchers.IO`.** [UpdateChecker.check]
     * is a blocking socket call with a 15 s connect timeout and a 20 s read timeout.
     * Run on the caller's dispatcher, with a main-thread caller — and the About
     * screen's button is one — the page freezes for up to 35 s and the system
     * declares an ANR. A "check for updates" button that hangs the app is worse than
     * not checking at all.
     *
     * **Why `lastUpdateCheckAt` is written on failure too.** The throttle prevents
     * repeated *requests*, not repeated *failures*. Recording only successes means a
     * user whose URL is wrong, or who is offline, re-sends a request that is certain
     * to fail on every cold start and never reaches "24 hours since the last
     * success" — so it looks like auto-check never ran. Writing on failure too makes
     * it "at most one attempt per day", whatever the outcome.
     *
     * **Why `runCatching` rather than letting an exception escape.** Neither caller
     * has anywhere to handle an escaping exception gracefully: one is a
     * fire-and-forget coroutine in `Application.onCreate`, the other a button's
     * coroutine. And once one escapes, `checking` can be left `true` (a permanently
     * disabled button) with no result ever recorded. Folding any exception into
     * [UpdateChecker.Result.Failed] is what makes `check`'s own design — "failure is
     * a result, not an exception" — actually hold at this level.
     */
    suspend fun run(context: Context) {
        // Re-fetched each call rather than passed in by the caller: `run` can be
        // called at any moment, `Prefs.of` is a process singleton, and a second
        // lookup costs nothing while removing the question of whether the caller
        // happens to hold the same instance.
        val prefs = Prefs.of(context)
        val url = prefs.updateUrl

        checking = true
        // `try`/`finally` rather than "set it back to false afterwards": the
        // `withContext` suspends and resumes, and the caller's coroutine can be
        // cancelled at any point (closing the About page cancels the children of its
        // `rememberCoroutineScope`). Cancellation throws `CancellationException`,
        // which would skip the assignment and leave `checking` stuck `true` forever —
        // turning the manual button into a permanently disabled grey brick. The
        // `finally` guarantees the reset on every exit path.
        val outcome = try {
            withContext(Dispatchers.IO) {
                runCatching {
                    UpdateChecker.check(url, BuildConfig.VERSION_CODE, BuildConfig.VERSION_NAME)
                }.getOrElse { error ->
                    UpdateChecker.Result.Failed(error.message ?: "未知错误")
                }
            }
        } finally {
            checking = false
        }

        result = outcome
        // Written for success, failure and "already up to date" alike; see the
        // method note above.
        prefs.updateLastUpdateCheckAt(System.currentTimeMillis())

        // Logged in the shape of the rules prefetch in `DetourApp` (INFO on success,
        // WARN on failure), under the "Update" tag. The silent startup check has no
        // UI at all, so the log is its only observable trace: when someone asks why
        // no update prompt appeared, the first line here says whether the answer was
        // honestly "already up to date" or "the check failed, and here is where".
        when (outcome) {
            is UpdateChecker.Result.Available -> KernelState.log(
                KernelState.LogEntry.Level.INFO,
                "Update",
                "发现新版本 ${outcome.release.versionName}",
            )

            is UpdateChecker.Result.UpToDate -> KernelState.log(
                KernelState.LogEntry.Level.INFO,
                "Update",
                "已是最新版本",
            )

            is UpdateChecker.Result.Failed -> KernelState.log(
                KernelState.LogEntry.Level.WARN,
                "Update",
                "检查更新失败：${outcome.message}",
            )
        }
    }
}
