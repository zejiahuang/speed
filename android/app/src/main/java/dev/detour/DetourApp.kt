package dev.detour

import android.app.Application
import android.net.VpnService
import androidx.core.content.ContextCompat
import dev.detour.core.BuildFlags
import dev.detour.core.DetourVpnService
import dev.detour.core.Disclaimer
import dev.detour.core.Kernel
import dev.detour.core.KernelState
import dev.detour.core.LogArchive
import dev.detour.core.Prefs
import dev.detour.core.RulesRepository
import dev.detour.core.UpdateState
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

/**
 * Loads the native library before anything can ask for it.
 *
 * `System.loadLibrary` is idempotent, but doing it here means the failure is
 * discovered once, at startup, rather than on the first connect — where it would
 * surface as a tunnel that refuses to start with no obvious cause.
 *
 * A failure is recorded, not thrown. A missing ABI is a packaging mistake; the UI
 * should still come up and say so in the settings screen rather than crash on
 * launch and leave the user with nothing to look at.
 */
class DetourApp : Application() {

    /**
     * Where the launch-time rules prefetch runs.
     *
     * `Dispatchers.IO` and a scope of the Application's own, for the same reason
     * `DetourVpnService` and `ControlReceiver` each keep one: `RulesRepository.load()`
     * is network I/O with a 20 s connect and a 90 s read timeout, and `onCreate`
     * runs on the main thread. Calling it inline would freeze the UI — or trip the
     * ANR watchdog outright — before the first frame is ever drawn, which is a
     * worse cold start than the one this prefetch exists to remove.
     *
     * A `SupervisorJob` so that a failing fetch does not cancel anything else that
     * later shares this scope; nothing here is lifecycle-bound because the process
     * itself is the lifetime that matters.
     */
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    override fun onCreate() {
        super.onCreate()

        // Before anything can read the status: the mode the user last chose.
        //
        // The [BuildFlags.TUN_ONLY] branch is the one that overrides that choice
        // to VPN, and it is not taken while the flag is false — which is the
        // point: the picker is on screen, so the stored value is honoured and
        // there is a control that can change it. The branch stays because it is
        // what makes hiding the mode again a one-line change.
        //
        // An unreadable value falls back to PROXY, which is what `Prefs.mode` and
        // `KernelState.Status` default to as well — one fallback for the three, so
        // a corrupt value cannot name a mode the other two disagree with.
        val stored = Prefs.of(this).mode
        val restored = if (BuildFlags.TUN_ONLY) {
            KernelState.Mode.VPN
        } else {
            runCatching { KernelState.Mode.valueOf(stored.uppercase()) }
                .getOrDefault(KernelState.Mode.PROXY)
        }
        KernelState.restoreMode(restored)

        // Before the first log line, so the kernel-load result below is archived
        // too. The sink itself checks the "按天归档" setting on every write, so
        // installing it unconditionally is what lets the switch work immediately.
        LogArchive.install(this)

        // After the sink, deliberately: auto-connect can log (a skipped launch
        // with no consent is a WARN), and a log line emitted before the sink is
        // installed would be visible in the app but missing from the archive the
        // owner reads when diagnosing a tunnel that did not come up.
        maybeAutoConnect()

        val loaded = Kernel.ensureLoaded()
        KernelState.log(
            if (loaded) KernelState.LogEntry.Level.INFO else KernelState.LogEntry.Level.ERROR,
            "DetourApp",
            if (loaded) "内核库已加载" else "内核库不可用：${Kernel.loadError}",
        )

        // Fetch the rules now rather than at connect. The cold-start delay the
        // owner sees is `RulesRepository.load()` running for the first time on the
        // connect path; doing it here moves that cost off the critical path, so
        // the first connect is fast and the document is already on disk.
        //
        // `load()` with no `forceRefresh`, deliberately. The repository already
        // returns the cache while it is still fresh and fetches only once it has
        // aged past `refresh_hours`, so launching the app repeatedly costs nothing.
        // Forcing a refresh here would turn every launch into a 1.4 MB download.
        //
        // Offline stays correct because the repository owns that decision: it
        // serves the cache, and throws only when the device is offline **and**
        // there is no cache. A second offline check here could only disagree with
        // it — and two components disagreeing about the same fact is a bug, not a
        // safety net.
        //
        // A failure is recorded, not thrown. A missing network at launch is
        // normal, not fatal; the `runCatching` turns the offline-with-no-cache
        // throw into a WARN and the app comes up regardless.
        scope.launch {
            runCatching { RulesRepository.load(this@DetourApp) }
                .onSuccess { bytes ->
                    KernelState.log(
                        KernelState.LogEntry.Level.INFO,
                        "DetourApp",
                        "启动时已预取规则：${bytes.size / 1024} KiB",
                    )
                }
                .onFailure { error ->
                    KernelState.log(
                        KernelState.LogEntry.Level.WARN,
                        "DetourApp",
                        "启动时预取规则失败：${error.message}",
                    )
                }
        }

        // One silent update check at startup.
        //
        // **Why this lives in `Application.onCreate` and not in an Activity.** The
        // same reason as the rules prefetch above: this belongs to "the process
        // started", not to "some screen was opened". Putting it on the About page
        // would make "is there a new version" depend on whether the user happened to
        // open that page, and `MainActivity` is still too late — what has to be
        // covered is the act of opening the app. This scope's lifetime is the
        // process, so the check is not cancelled by any screen being destroyed and
        // always gets to finish after a cold start.
        //
        // **Why it is silent.** A failure is logged and nothing is shown: an update
        // check that reports an error at startup is more annoying than no check at
        // all. The user did not ask for one, so a dead network, a wrong URL or a
        // GitHub rate limit must not put anything on screen. On this whole path,
        // "there genuinely is an update" is the only outcome that later opens a
        // window (see the `UpdateState.pendingPrompt` test in `MainActivity`).
        //
        // The `shouldAutoCheck` test is inside the coroutine rather than done
        // synchronously here to decide whether to launch at all: it reads `Prefs`,
        // and `Prefs.of` performs a SharedPreferences read plus migration, so keeping
        // it on the IO context keeps even that read off the main thread.
        scope.launch {
            val prefs = Prefs.of(this@DetourApp)
            if (UpdateState.shouldAutoCheck(prefs)) UpdateState.run(this@DetourApp)
        }
    }

    /**
     * Connect on launch, when the user has asked for it and consent already exists.
     *
     * The consent test is the whole point of the method. `VpnService.prepare`
     * returns `null` when consent has already been granted and an `Intent` when the
     * system would have to ask the user — and an `Application` has no Activity to
     * ask from, so auto-connect can only ever work on a device that has connected
     * manually at least once. That is not a limitation to work around: a launch that
     * popped a consent dialog out of nowhere would be a worse surprise than not
     * connecting, so a missing consent is logged and skipped rather than forced.
     *
     * Proxy mode needs no consent at all, so the gate applies only to VPN.
     *
     * **The disclaimer gate comes before all of that, and is not mode-specific.**
     * The disclaimer is about the software rather than about a permission, so it
     * covers both modes — starting a tunnel for someone who has never been shown
     * the notice would make the notice decorative. This runs from
     * `Application.onCreate`, i.e. strictly before `MainActivity` can put the dialog
     * on screen, which is exactly why the check has to be here: the window appears a
     * frame or two later, and a tunnel that came up in the meantime would already be
     * carrying traffic.
     */
    private fun maybeAutoConnect() {
        val prefs = Prefs.of(this)
        if (!prefs.autoConnect) return

        // After the `autoConnect` test rather than before it, so the WARN below is
        // only ever emitted when a connection was actually wanted and withheld.
        // Logging it on every launch of every install that never enabled
        // auto-connect would be noise, and noise is what makes a real WARN hard to
        // find. `isAccepted` reads the bundled text; an unreadable resource counts
        // as accepted (see `Disclaimer`), so a packaging defect does not silently
        // disable auto-connect as a second symptom.
        if (!Disclaimer.isAccepted(Disclaimer.text(this), prefs)) {
            KernelState.log(
                KernelState.LogEntry.Level.WARN,
                "DetourApp",
                "启动自动连接跳过：还没有接受免责声明",
            )
            return
        }

        val mode = KernelState.status.value.mode
        val needsConsent = runCatching { VpnService.prepare(this) }.getOrNull() != null
        if (mode == KernelState.Mode.VPN && needsConsent) {
            KernelState.log(
                KernelState.LogEntry.Level.WARN,
                "DetourApp",
                "启动自动连接跳过：还没有 VPN 授权",
            )
            return
        }

        // Reached from `Application.onCreate`, which only runs because the user
        // launched the app, so starting a foreground service here is allowed. It
        // must **not** be moved to a boot receiver without also adding
        // `RECEIVE_BOOT_COMPLETED` and a receiver — auto-connect on boot is a
        // separate feature, and this method's permission to start a service
        // depends on the user having opened the app.
        ContextCompat.startForegroundService(this, DetourVpnService.startIntent(this, mode))
    }
}
