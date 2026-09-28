package dev.detour.core

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import androidx.core.app.NotificationCompat
import dev.detour.MainActivity
import dev.detour.R
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull

/**
 * The tunnel, and the kernel that runs inside it.
 *
 * This *is* the `VpnService` — not a wrapper around one. Two reasons:
 *
 * * `protect(fd)` has to be called on the instance that established the tunnel,
 *   and the kernel calls it from a Rust thread. Keeping both on one object means
 *   the callback is a method reference rather than a static lookup.
 * * Android only allows one active VPN per app, so a second service to own the
 *   engine would have nothing of its own to own.
 *
 * The step loop runs on a coroutine, not in a service callback:
 * `watt_engine_step` blocks for up to a hundred milliseconds waiting for a
 * packet, so calling it on the main thread would freeze the UI every time the
 * network went quiet — which is most of the time.
 */
class DetourVpnService : VpnService() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var loop: Job? = null
    private var engine: Kernel.Engine? = null
    private var proxy: Kernel.Proxy? = null
    private var tunnel: ParcelFileDescriptor? = null

    override fun onCreate() {
        super.onCreate()
        instance = this
        // Not in an `init` block: `init` runs before `attachBaseContext`, so
        // `getSystemService` returns null and the service fails to construct.
        createChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                teardown()
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_START -> start(
                intent.getStringExtra(EXTRA_MODE)
                    ?.let { runCatching { KernelState.Mode.valueOf(it) }.getOrNull() }
                    ?: KernelState.Mode.VPN,
            )
            ACTION_RESTART -> restart(
                intent.getStringExtra(EXTRA_MODE)
                    ?.let { runCatching { KernelState.Mode.valueOf(it) }.getOrNull() }
                    ?: KernelState.status.value.mode,
            )
        }
        // Not sticky on purpose. A restarted tunnel would come up without the
        // rule document the user had loaded, and relaying with the wrong rules
        // silently is worse than not relaying.
        return START_NOT_STICKY
    }

    private fun start(mode: KernelState.Mode) {
        if (engine != null || proxy != null) {
            KernelState.log(KernelState.LogEntry.Level.WARN, TAG, "已在运行，忽略重复请求")
            return
        }
        KernelState.setStatus {
            it.copy(phase = KernelState.Phase.STARTING, mode = mode, message = null)
        }
        startForeground(NOTIFICATION_ID, notification(getString(R.string.home_state_connecting)))

        scope.launch {
            try {
                val document = RulesRepository.load(this@DetourVpnService)
                KernelState.log(
                    KernelState.LogEntry.Level.INFO, TAG,
                    "规则已加载 ${document.size / 1024} KiB",
                )

                when (mode) {
                    KernelState.Mode.PROXY -> startProxy(document)
                    KernelState.Mode.VPN -> startVpn(document)
                }
            } catch (err: Throwable) {
                fail(err.message ?: err.javaClass.simpleName)
            }
        }
    }

    /**
     * Tear the tunnel down and bring it straight back up, in one command.
     *
     * Deliberately not "send STOP, then send START". Those are two `startService`
     * calls whose ordering is the system's to decide, and `start()` returns early
     * and silently when an engine is already up — so a START that overtook its own
     * STOP would leave the user on the old settings with the button apparently
     * having worked. Here `teardown()` is synchronous (it joins the step loop), so
     * by the time `start` runs there is nothing left to collide with.
     *
     * The mode is carried through rather than re-chosen: this is "rebuild with the
     * same intent", and VPN consent is already granted, so asking for it again
     * would be wrong.
     */
    private fun restart(mode: KernelState.Mode) {
        teardown()
        start(mode)
    }

    /**
     * The mode that needs neither TUN nor root.
     *
     * No tunnel, no `VpnService` consent, no protector — the kernel simply listens
     * on a local port and relays the listed domains it is asked for. What it
     * cannot do is capture traffic the app did not send it, so the user has to
     * point something at [proxy]'s port.
     */
    private suspend fun startProxy(document: ByteArray) {
        val started = Kernel.startProxy(Prefs.of(this).proxyPort, document)
        proxy = started
        KernelState.setStatus {
            it.copy(
                phase = KernelState.Phase.ON,
                sinceMillis = System.currentTimeMillis(),
                proxyPort = started.port,
            )
        }
        KernelState.log(
            KernelState.LogEntry.Level.INFO, TAG,
            "代理已监听 127.0.0.1:${started.port}",
        )
        updateNotification(getString(R.string.home_state_on))
        // Nothing to step: the proxy owns its own accept loop and per-connection
        // threads. Only the counters need sampling, and this mode does not
        // expose any yet.
    }

    private suspend fun startVpn(document: ByteArray) {
        // The MTU is read **once, into a local**, and both consumers are handed
        // that one value: [establish] for the interface, and the settings
        // document for the kernel. Holding a single [Prefs] instance is not
        // enough — `prefs.mtu` is a live property, so reading it twice is two
        // reads, and an `updateMtu` landing between them would put the interface
        // and the kernel back on different numbers, which is the bug this is
        // here to prevent. [establish] records what that disagreement costs.
        val prefs = Prefs.of(this)
        val mtu = prefs.mtu

        val descriptor = establish(mtu)
            ?: run { fail(getString(R.string.home_no_permission)); return }

        // Without this the settings screen is a list of switches that change
        // nothing: the engine was built on `StackConfig::default()` and no
        // amount of writing to `SharedPreferences` reached it.
        //
        // Held in a local rather than passed inline because the same string is
        // recorded as "what the engine is using": two calls to
        // [Prefs.kernelSettingsJson] are two snapshots, and a row edited between
        // them would be recorded as applied when the kernel never saw it.
        val settings = prefs.kernelSettingsJson(mtu)

        // The protector is the whole reason this C ABI exists: it is how the
        // kernel's own upstream sockets stay out of the tunnel it just created.
        // Measured without it on this emulator: sixteen client connections
        // produced `tcp_open=2048` and not one byte came back.
        engine = Kernel.create(
            tunFd = descriptor.fd,
            name = INTERFACE,
            rules = document,
            protector = ::protectSocket,
            settings = settings,
        )
        tunnel = descriptor
        // Recorded only once the engine exists. `applied` means "the running
        // engine is using this", and a create that threw must not be able to
        // claim otherwise.
        prefs.recordAppliedKernelSettings(settings)

        // The read-only snapshot the rules screen asks for. Registered here,
        // after the engine exists, and cleared in `teardown`/`fail` — the
        // lambda reads the field on every call rather than capturing a value,
        // so it can never outlive the engine it points at. A screen that held
        // the engine directly would be holding a freed native handle the
        // moment the tunnel stops.
        KernelState.ipStats = { engine?.ipStats() }

        KernelState.setStatus {
            it.copy(phase = KernelState.Phase.ON, sinceMillis = System.currentTimeMillis())
        }
        KernelState.log(KernelState.LogEntry.Level.INFO, TAG, "内核已启动（VPN）")
        updateNotification(getString(R.string.home_state_on))

        runLoop()
    }

    /**
     * Build and claim the tunnel.
     *
     * **Everything is routed in, and the kernel decides.** The obvious alternative
     * — installing one route per rule address — does not work: the merged rule set
     * carries around 180,000 addresses, and each `addRoute` is a binder call to
     * the system server. Measured on this emulator, the service never finished
     * starting.
     *
     * So the tunnel claims `0.0.0.0/0` and the decision moves to where it belongs.
     * The kernel already knows how to make it: `plan()` returns rule addresses for
     * a listed domain and `direct` for anything else, and both end up as an
     * ordinary socket. The cost is that unlisted traffic also crosses userspace —
     * which is inherent to any userspace VPN, and is the price of not having to
     * enumerate the ruleset into the routing table.
     *
     * The app itself is excluded, or the tunnel would capture the very
     * connections the kernel makes to reach the rule addresses.
     *
     * [mtu] is the **user's** number, and it has to be the same one the kernel
     * is handed. Two different things are sized by it and they have to agree:
     * the interface MTU is what the device's own IP stack uses to decide how big
     * a packet it may hand to `tun0`, while `StackConfig.mtu` sizes the buffer
     * the kernel reads those packets into (`engine.rs` allocates
     * `mtu.max(576) + 128`, which is why the floor matters here too).
     *
     * This was a constant 1500 while the settings screen wrote `prefs.mtu`, so
     * the two could disagree — and the read buffer was the smaller one. Measured
     * on the emulator: after `set mtu 1300`, `ip link show tun0` still reported
     * `mtu 1500`, while the kernel had been told 1300 and sized its buffer at
     * 1428. `read_fd` is a single `read()` with no truncation check (`tun.rs`),
     * so any frame above the buffer length is silently cut to it and the IP
     * header's `total_length` still claims the original size. Below `mtu` 1372
     * the buffer cannot hold a maximum-size frame at all, and the settings row
     * that reaches it steps by 100 from a default of 1500 — two taps down is
     * enough. That is why the MTU is threaded through from one read of [Prefs]
     * rather than defaulted here.
     */
    private fun establish(mtu: Int): ParcelFileDescriptor? {
        val builder = Builder()
            .setSession(getString(R.string.app_name))
            .setMtu(mtu)
            .addAddress(TUNNEL_ADDRESS, TUNNEL_PREFIX)
            // The kernel answers DNS from the rule set, so the tunnel has to be
            // the resolver the system hands out.
            .addDnsServer(TUNNEL_DNS)
            .addRoute("0.0.0.0", 0)
            .addRoute("::", 0)
            .addDisallowedApplication(packageName)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            builder.setMetered(false)
        }
        KernelState.log(
            KernelState.LogEntry.Level.INFO, TAG,
            "接管全部流量，由内核按规则分流",
        )
        return builder.establish()
    }

    /**
     * The step loop, with the counters sampled on two independent cadences.
     *
     * **These are two different things and are deliberately not merged.** The rate
     * is a derivative: sampled once a second it is a number nobody can watch move,
     * so it keeps its own fine tick regardless of what the settings screen says.
     * The counters published to the UI are a *report*, and `statsIntervalSeconds`
     * (1–60 s) is how often the user wants one produced — that is the knob the
     * "统计输出间隔" row actually owns.
     *
     * Driving both from the report interval would make the speed display stutter
     * once a second; driving both from the rate tick would make the setting do
     * nothing. One `stats()` call per tick serves both, so the kernel is still
     * asked for its counters only when at least one of the two is due.
     */
    private suspend fun runLoop() {
        loop?.cancel()
        loop = scope.launch {
            var lastRateSample = 0L
            var lastReport = 0L
            var previous = Kernel.Stats()
            var previousAt = System.currentTimeMillis()

            while (isActive) {
                val current = engine ?: break
                if (current.step() < 0) {
                    fail(Kernel.lastError() ?: "内核返回了错误")
                    break
                }

                // Re-read every tick so moving the "统计输出间隔" stepper changes a
                // running tunnel rather than only the next one. A settings read is a
                // volatile load and a snapshot read; the `step` above is the
                // expensive call by orders of magnitude.
                val reportIntervalMs =
                    Prefs.of(this@DetourVpnService).statsIntervalSeconds.coerceIn(1, 60) * 1000L

                val now = System.currentTimeMillis()
                val rateDue = now - lastRateSample >= RATE_SAMPLE_INTERVAL_MS
                val reportDue = now - lastReport >= reportIntervalMs
                if (!rateDue && !reportDue) continue

                val stats = current.stats() ?: continue

                if (rateDue) {
                    lastRateSample = now
                    val elapsed = (now - previousAt).coerceAtLeast(1)
                    Rate.update(
                        down = (stats.bytesToClient - previous.bytesToClient) * 1000 / elapsed,
                        up = (stats.bytesToUpstream - previous.bytesToUpstream) * 1000 / elapsed,
                    )
                    previous = stats
                    previousAt = now
                }
                if (reportDue) {
                    lastReport = now
                    KernelState.setStats(stats)
                }
            }
        }
    }

    /**
     * Swap in a freshly filtered document without disturbing running flows.
     *
     * Called when a rule switch moves while the tunnel is up. `replace_rules`
     * exists for exactly this: a flow already in flight is carrying bytes for an
     * address the old document chose, and cutting it off to apply a settings
     * change would turn "I switched a group off" into "my download died".
     *
     * Both modes are handled, because both can be up when a switch moves. The
     * earlier version only knew about [engine], so in proxy mode the reload
     * returned early and the switch stayed decoration until a reconnect — the
     * exact symptom the reload exists to prevent.
     */
    fun reloadRules() {
        val tunnel = engine
        val relay = proxy
        if (tunnel == null && relay == null) return
        scope.launch {
            try {
                val document = RulesRepository.load(this@DetourVpnService, forceRefresh = false)
                val applied = when {
                    tunnel != null -> tunnel.replaceRules(document)
                    relay != null -> relay.replaceRules(document)
                    else -> false
                }
                if (applied) {
                    KernelState.log(
                        KernelState.LogEntry.Level.INFO, TAG,
                        "规则已热替换（${document.size / 1024} KiB），在跑的流未受影响",
                    )
                } else {
                    KernelState.log(
                        KernelState.LogEntry.Level.ERROR, TAG,
                        "规则热替换被内核拒绝：${Kernel.lastError()}",
                    )
                }
            } catch (err: Throwable) {
                KernelState.log(
                    KernelState.LogEntry.Level.ERROR, TAG, "重载规则失败：${err.message}",
                )
            }
        }
    }

    /**
     * Called by the kernel for every upstream socket, before it connects.
     *
     * `super.protect`, not `protect`: this method shadows the `VpnService` one,
     * and calling it by name would recurse until the stack ran out.
     *
     * Returning false makes the kernel abandon that connection rather than hand
     * the socket back to the tunnel.
     */
    private fun protectSocket(fd: Int): Boolean = protect(fd)

    /**
     * A scoped handle on the run loop, so stopping actually waits for it.
     *
     * `loop?.cancel()` is not enough and never was. Cancellation is cooperative:
     * it sets a flag and returns, while the coroutine may be several frames deep
     * inside `step()` — which blocks for up to a hundred milliseconds waiting for
     * a packet, and on the way through it calls the protector, which is a JNI
     * call into `VpnService.protect`. Freeing the engine while that is in flight
     * is a use-after-free, and on this device it was a hard crash:
     *
     *     signal 11 (SIGSEGV), fault addr 0x20
     *       #00 libwatt_ffi.so BoxedProtector::protect+7
     *       #01 libwatt_ffi.so TcpRelay::service
     *       #02 libwatt_ffi.so Engine::step
     *
     * The same window double-released the protector's JNI global reference,
     * which surfaced separately as `decStrong() called too many times` on the
     * JVM's reference-queue thread — a crash the app had been blamed for without
     * anyone connecting it to this one.
     *
     * So teardown has to *join*, and the join is this function. It is bounded:
     * a step returns within a hundred milliseconds and `Job.join` observes the
     * cancellation, so a healthy loop comes back quickly. The bound exists so a
     * loop that somehow will not stop leaves a log line and a leaked engine
     * rather than a hang on exit — the same trade the Rust side makes.
     */
    private suspend fun stopLoop() {
        val running = loop ?: return
        loop = null
        running.cancel()
        val stopped = withTimeoutOrNull(LOOP_STOP_TIMEOUT_MS) { running.join() }
        if (stopped == null) {
            KernelState.log(
                KernelState.LogEntry.Level.WARN, TAG,
                "步进循环未在 ${LOOP_STOP_TIMEOUT_MS}ms 内退出，放弃等待；内核将泄漏而不是被释放",
            )
        }
    }

    private fun teardown() {
        // `runBlocking` because the callers are the STOP intent, `onRevoke`, and
        // `onDestroy` -- all of which are already off the main thread's hot path
        // and none of which returns a value anyone waits on. The wait is the
        // point. The STOP path is the reason this runs twice in a row
        // (`teardown()` then `stopSelf()` -> `onDestroy`).
        //
        // Everything below is idempotent except the log line, which is why "was
        // anything running" has to be read *before* `stopLoop()` clears `loop`:
        // logging unconditionally reported two disconnects for one, and reported
        // a disconnect after a start that never succeeded.
        val wasRunning = loop != null || engine != null || proxy != null || tunnel != null
        runBlocking { stopLoop() }
        engine?.close()
        engine = null
        // Cleared with the engine, so the hook's presence means "there is an
        // engine to ask". The lambda would already answer null here, but
        // leaving a closure that can never say anything is a lie about what is
        // running, and the next reader of this field has to be able to trust
        // that.
        KernelState.ipStats = null
        proxy?.close()
        proxy = null
        // Closing the descriptor is what actually tears the tunnel down; the
        // kernel only ever adopted it. After the join, because the engine reads
        // from it.
        runCatching { tunnel?.close() }
        tunnel = null
        KernelState.reset(KernelState.status.value.mode)
        // `applied` describes a running engine. There is none once this returns,
        // and leaving the old document behind would make the next comparison a
        // comparison against a tunnel that no longer exists.
        Prefs.of(this).recordAppliedKernelSettings("")
        if (wasRunning) {
            KernelState.log(KernelState.LogEntry.Level.INFO, TAG, "已断开")
        }
        stopForeground(STOP_FOREGROUND_REMOVE)
    }

    private fun fail(message: String) {
        KernelState.setStatus { it.copy(phase = KernelState.Phase.ERROR, message = message) }
        KernelState.log(KernelState.LogEntry.Level.ERROR, TAG, message)
        // `fail` is called *from* the run loop, so it must not join it — joining
        // the coroutine that is calling you is a deadlock. Cancelling is right
        // here and the loop is already on its way out; the free below is what
        // has to be safe, and the Rust-side claim is what makes it so.
        loop?.cancel()
        loop = null
        engine?.close()
        engine = null
        KernelState.ipStats = null
        proxy?.close()
        proxy = null
        runCatching { tunnel?.close() }
        tunnel = null
        updateNotification(getString(R.string.home_state_error))
    }

    override fun onRevoke() {
        // The system takes the tunnel away when another VPN is selected. Nothing
        // to do but stop cleanly and say so.
        KernelState.log(KernelState.LogEntry.Level.WARN, TAG, "系统收回了 VPN")
        teardown()
        stopSelf()
    }

    override fun onDestroy() {
        instance = null
        teardown()
        scope.cancel()
        super.onDestroy()
    }

    // --- notification ----------------------------------------------------------

    private fun createChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val manager = getSystemService(NotificationManager::class.java) ?: return
        if (manager.getNotificationChannel(CHANNEL_ID) != null) return
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                getString(R.string.nav_home),
                // Low: a status indicator, not something that should make a sound.
                NotificationManager.IMPORTANCE_LOW,
            ).apply { setShowBadge(false) },
        )
    }

    private fun notification(text: String): Notification {
        val open = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE,
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_sys_download_done)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(text)
            .setOngoing(true)
            .setContentIntent(open)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
    }

    private fun updateNotification(text: String) {
        getSystemService(NotificationManager::class.java)?.notify(NOTIFICATION_ID, notification(text))
    }

    companion object {
        private const val TAG = "DetourVpn"
        private const val NOTIFICATION_ID = 1
        private const val CHANNEL_ID = "detour.tunnel"
        /**
         * How often the rate is recomputed.
         *
         * Fixed at half a second on purpose: this is the display's refresh rate,
         * not a user setting. The row that *is* a setting — "统计输出间隔" — is how
         * often the counters are published, and the two are read separately in
         * [runLoop].
         */
        private const val RATE_SAMPLE_INTERVAL_MS = 500L

        /**
         * How long the teardown waits for the step loop to leave `step()`.
         *
         * A blocking step returns within a hundred milliseconds, so this is an
         * order of magnitude of headroom. It is not zero because a step really
         * can be inside a JNI call, and it is not unbounded because a loop that
         * will not stop must not turn "close the app" into "the app hangs".
         */
        private const val LOOP_STOP_TIMEOUT_MS = 2_000L

        private const val INTERFACE = "detour0"

        // 198.18.0.0/15 is reserved for benchmarking by RFC 2544 and is not
        // routable, which makes it a safe private identity for the tunnel.
        private const val TUNNEL_ADDRESS = "198.18.0.1"
        private const val TUNNEL_PREFIX = 15
        private const val TUNNEL_DNS = "198.18.0.2"

        const val ACTION_START = "dev.detour.action.START"
        const val ACTION_STOP = "dev.detour.action.STOP"
        const val ACTION_RESTART = "dev.detour.action.RESTART"
        const val EXTRA_MODE = "mode"

        /**
         * The running service, so a settings change can reach it.
         *
         * A static rather than a bound service: there is exactly one tunnel, the
         * app is the only thing that starts it, and binding would mean the rules
         * screen holding a connection it has no other use for.
         */
        @Volatile
        var instance: DetourVpnService? = null
            private set

        /** Re-apply the rule switches if a tunnel is up. A no-op otherwise. */
        fun reloadRulesIfRunning() {
            instance?.reloadRules()
        }

        fun startIntent(context: android.content.Context, mode: KernelState.Mode): Intent =
            Intent(context, DetourVpnService::class.java).apply {
                action = ACTION_START
                putExtra(EXTRA_MODE, mode.name)
            }

        fun stopIntent(context: android.content.Context): Intent =
            Intent(context, DetourVpnService::class.java).apply { action = ACTION_STOP }

        fun restartIntent(context: android.content.Context, mode: KernelState.Mode): Intent =
            Intent(context, DetourVpnService::class.java).apply {
                action = ACTION_RESTART
                putExtra(EXTRA_MODE, mode.name)
            }
    }

}

/** The instantaneous rate. Derived from the counters, so it lives beside them. */
object Rate {
    @Volatile var downBytesPerSecond: Long = 0
        private set

    @Volatile var upBytesPerSecond: Long = 0
        private set

    fun update(down: Long, up: Long) {
        downBytesPerSecond = down.coerceAtLeast(0)
        upBytesPerSecond = up.coerceAtLeast(0)
    }

    fun clear() {
        downBytesPerSecond = 0
        upBytesPerSecond = 0
    }
}
