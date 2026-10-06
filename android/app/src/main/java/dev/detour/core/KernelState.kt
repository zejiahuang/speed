package dev.detour.core

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * What the UI observes, and the only thing it observes.
 *
 * A process-wide singleton rather than something the Activity binds for. The
 * tunnel outlives the Activity by design — it is a foreground service — so the
 * state has to live somewhere that does too. Binding would mean the screen loses
 * its view of a running tunnel the moment the user rotates the device or leaves
 * the app, which is exactly when they want to see it.
 *
 * The service is the only writer. Everything else reads.
 */
object KernelState {

    /** Which path the traffic takes. */
    enum class Mode {
        /**
         * An HTTP proxy on a local port: `CONNECT` tunnels and plain-HTTP
         * forwarding, on the one port. No root, no VPN permission, and no tunnel —
         * but only apps that can be pointed at a proxy benefit.
         */
        PROXY,

        /**
         * A TUN device handed over by `VpnService`. All traffic, at the cost of a
         * system consent dialog.
         */
        VPN,
    }

    enum class Phase { OFF, STARTING, ON, ERROR }

    data class Status(
        val phase: Phase = Phase.OFF,
        val mode: Mode = Mode.PROXY,
        /** Populated in [Phase.ERROR]; shown verbatim because it comes from the kernel. */
        val message: String? = null,
        /** `System.currentTimeMillis()` when the tunnel came up. */
        val sinceMillis: Long = 0L,
        /** The port the proxy is listening on, in [Mode.PROXY]. */
        val proxyPort: Int = 0,
    ) {
        val isRunning: Boolean get() = phase == Phase.ON
    }

    /** One line in the log screen. */
    data class LogEntry(
        val atMillis: Long,
        val level: Level,
        val tag: String,
        val message: String,
    ) {
        enum class Level { DEBUG, INFO, WARN, ERROR }
    }

    private val _status = MutableStateFlow(Status())
    val status: StateFlow<Status> = _status.asStateFlow()

    private val _stats = MutableStateFlow(Kernel.Stats())
    val stats: StateFlow<Kernel.Stats> = _stats.asStateFlow()

    private val _logs = MutableStateFlow<List<LogEntry>>(emptyList())
    val logs: StateFlow<List<LogEntry>> = _logs.asStateFlow()

    /**
     * Bounded on purpose. A tunnel moving a megabyte a second can produce log
     * lines faster than a person can read them, and an unbounded list is a memory
     * leak with extra steps.
     */
    private const val MAX_LOG_LINES = 500

    // --- writers, called by the service ---------------------------------------

    fun setStatus(transform: (Status) -> Status) {
        _status.update(transform)
    }

    fun setStats(stats: Kernel.Stats) {
        _stats.value = stats
    }

    /**
     * Where a log line also goes, when the archive is on.
     *
     * Installed by the Application through [LogArchive.install], and null in any
     * process that never did — which is every test and any future headless entry
     * point. The sink is what lets the archive honour its own setting: it is a
     * no-op when the user has turned archiving off, rather than the writer having
     * to know about the setting.
     */
    @Volatile
    var archive: ((LogEntry) -> Unit)? = null

    fun log(level: LogEntry.Level, tag: String, message: String) {
        val entry = LogEntry(System.currentTimeMillis(), level, tag, message)
        _logs.update { current ->
            // Prepending rather than appending: the newest line is what someone
            // opens this screen to see, and a list that grows downward would put
            // it off the bottom.
            (listOf(entry) + current).take(MAX_LOG_LINES)
        }
        // Outside the `update`: the archive writes to disk, and doing that inside
        // a state mutation would run it while a collector is waiting on the lock.
        archive?.invoke(entry)
    }

    fun clearLogs() {
        _logs.value = emptyList()
    }

    /** Put the state back to "off" without touching the log, which is history. */
    fun reset(mode: Mode) {
        _status.value = Status(phase = Phase.OFF, mode = mode)
        _stats.value = Kernel.Stats()
    }

    /**
     * Adopt the stored mode on startup.
     *
     * Called once from the Application, before any screen reads the status. The
     * holder is a process singleton, so without this the mode is whatever the
     * default is on every cold start — and a user who chose VPN would find
     * themselves back on the proxy with nothing on screen to explain it.
     */
    fun restoreMode(mode: Mode) {
        if (_status.value.phase == Phase.OFF) {
            _status.value = _status.value.copy(mode = mode)
        }
    }

    // --- commands --------------------------------------------------------------
    //
    // The service registers these on start and clears them on stop, so the UI can
    // ask for things without holding a reference to a running service.

    @Volatile
    var onConnect: ((Context, Mode) -> Unit)? = null

    @Volatile
    var onDisconnect: (() -> Unit)? = null

    /**
     * A read-only snapshot of the running engine's per-address history and
     * certificate verdicts, or null when there is no engine to ask.
     *
     * Registered by the service on start and cleared on stop, the same shape as
     * the command hooks above, and for the same reason: the screen must never
     * hold a [Kernel.Engine] of its own. The tunnel outlives the Activity by
     * design, so a screen that kept a reference would be holding a native
     * handle across the teardown that frees it. Asking through this hook means
     * the answer always comes from whatever the service currently has —
     * including the honest answer "nothing running".
     *
     * Read-only: nothing it returns feeds back into a routing decision, so
     * calling it cannot perturb the tunnel.
     */
    @Volatile
    var ipStats: (() -> Kernel.IpStats?)? = null
}
