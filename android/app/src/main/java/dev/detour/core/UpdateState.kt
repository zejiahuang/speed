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

    /**
     * The most recent result; `null` means this process has not checked yet.
     *
     * Private, and [resultFor] is the way to read it. The result and the channel
     * it answers are one piece of information, so exposing the result alone would
     * be handing out half of it — see [resultFor].
     */
    private var result: UpdateChecker.Result? by mutableStateOf(null)

    /**
     * The channel [result] was produced for; `null` when there is no result.
     *
     * Compose state rather than a plain field: two consecutive checks can produce
     * the same result value — `UpToDate` twice is the common case — and if only
     * this field changed, a reader subscribed to the result alone would not
     * recompose and would keep rendering the previous channel's answer.
     */
    private var resultChannel: UpdateChecker.Channel? by mutableStateOf(null)

    /**
     * The last result, but only when it answers [channel].
     *
     * **Why the result is stored with the channel it came from.** A result answers
     * one question — "is there a newer stable release" is not answered by "is there
     * a newer prerelease" — so a result produced for one channel must never be
     * rendered under the other's control.
     *
     * The first version of this cleared the result whenever the channel changed,
     * which is correct only if *every* writer remembers to clear. The About page's
     * segmented row did; the control channel's `set update_channel` did not, and
     * measured on the device that left `有新版本 0.17.0-rc2` — a prerelease — sitting
     * underneath a 稳定版 control. Pairing the result with its channel moves the rule
     * to the read site, where no writer can forget it. It also covers a check that
     * is still in flight when the channel changes: the answer still lands, but it
     * lands as an answer to the question it actually asked, so switching to the
     * other channel simply stops showing it.
     *
     * Switching away and back therefore re-shows the result rather than leaving the
     * page blank. That is the honest behaviour — the result really does answer the
     * channel that is on screen again.
     */
    fun resultFor(channel: UpdateChecker.Channel): UpdateChecker.Result? =
        if (resultChannel == channel) result else null

    /**
     * [resultFor] narrowed to the one case that has something to show the user.
     *
     * Exists so the update dialog does not have to cast: it is entered only when
     * there is an available release, and an explicit cast there is a claim the
     * compiler cannot check. Returning `null` for the other two outcomes lets the
     * caller test the value it actually needs.
     */
    fun availableFor(channel: UpdateChecker.Channel): UpdateChecker.Result.Available? =
        resultFor(channel) as? UpdateChecker.Result.Available

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
     * The release the update dialog should offer on [channel], or `null` when no
     * dialog belongs on screen.
     *
     * Both conditions are required: a result that answers [channel] and is
     * [UpdateChecker.Result.Available], and a process that has not shown the dialog
     * yet. `promptShown` is tested first so the common "already shown" path
     * short-circuits before the type test.
     *
     * Returning the release rather than a boolean is what removes the dialog's
     * cast: "the flag is true" and "there is a release to show" are the same fact
     * here, and a separate boolean would let a caller test one and then read the
     * other.
     *
     * The channel is a parameter rather than being read from `Prefs` here for the
     * same reason [resultFor] is a function: this object holds no `Context` and has
     * no business resolving a preference, and the caller already has the value it
     * is rendering the page with. Without it, a check that was in flight while the
     * user switched channels could raise a dialog offering the *other* channel's
     * release.
     */
    fun pendingPrompt(channel: UpdateChecker.Channel): UpdateChecker.Result.Available? =
        if (promptShown) null else availableFor(channel)

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
        val channel = prefs.updateChannelValue

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
                    UpdateChecker.check(
                        url,
                        BuildConfig.VERSION_CODE,
                        BuildConfig.VERSION_NAME,
                        channel,
                    )
                }.getOrElse { error ->
                    UpdateChecker.Result.Failed(error.message ?: "未知错误")
                }
            }
        } finally {
            checking = false
        }

        // Recorded as a pair — the answer and the question it answers. A check that
        // is in flight when the channel changes still lands, but it lands as the
        // *other* channel's answer, so `resultFor` stops showing it instead of the
        // beta control displaying a stable version. See `resultFor`.
        result = outcome
        resultChannel = channel
        // Written for success, failure and "already up to date" alike; see the
        // method note above.
        prefs.updateLastUpdateCheckAt(System.currentTimeMillis())

        // Logged in the shape of the rules prefetch in `DetourApp` (INFO on success,
        // WARN on failure), under the "Update" tag. The silent startup check has no
        // UI at all, so the log is its only observable trace: when someone asks why
        // no update prompt appeared, the first line here says whether the answer was
        // honestly "already up to date" or "the check failed, and here is where".
        // The channel goes into the message rather than staying implicit. "已是最新
        // 版本" on the beta channel means "no newer prerelease", which is a different
        // claim from the same sentence on the stable channel; and this line is the
        // only record of which endpoint a given check asked, since the About screen
        // shows the channel only until the user changes it.
        val channelTag = if (channel == UpdateChecker.Channel.BETA) "测试版" else "稳定版"
        when (outcome) {
            is UpdateChecker.Result.Available -> KernelState.log(
                KernelState.LogEntry.Level.INFO,
                "Update",
                "发现新版本 ${outcome.release.versionName}（$channelTag 通道）",
            )

            is UpdateChecker.Result.UpToDate -> KernelState.log(
                KernelState.LogEntry.Level.INFO,
                "Update",
                "已是最新版本（$channelTag 通道）",
            )

            is UpdateChecker.Result.Failed -> KernelState.log(
                KernelState.LogEntry.Level.WARN,
                "Update",
                "检查更新失败：${outcome.message}（$channelTag 通道）",
            )
        }
    }
}
