package dev.detour

import android.app.Application
import android.net.VpnService
import androidx.core.content.ContextCompat
import dev.detour.core.BuildFlags
import dev.detour.core.DetourVpnService
import dev.detour.core.Kernel
import dev.detour.core.KernelState
import dev.detour.core.LogArchive
import dev.detour.core.Prefs
import dev.detour.core.RulesRepository
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
        // Under [BuildFlags.TUN_ONLY] that choice is forced to VPN regardless of
        // what is stored. The mode picker is hidden while the flag is on, so a
        // device that previously stored `proxy` would otherwise run the proxy
        // forever with no control that could change it. The stored value is left
        // untouched, so flipping the flag restores whatever the user had chosen.
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
     */
    private fun maybeAutoConnect() {
        if (!Prefs.of(this).autoConnect) return

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
