package dev.detour.core

import android.util.Log
import org.json.JSONArray
import org.json.JSONObject
import java.io.Closeable
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Kotlin side of `watt-ffi`.
 *
 * The native library is the Rust kernel built by `scripts/android-build.sh` and
 * copied into `src/main/jniLibs/<abi>/`. The C surface is described in
 * `core-rs/crates/watt-ffi/include/watt_ffi.h`; this file is the only place in
 * the app that knows its shape.
 *
 * The design rule that matters: **an engine without a protector is refused, not
 * defaulted.** On Android the kernel's own upstream sockets have to be exempted
 * from the tunnel it just created, and the only supported way to do that is
 * `VpnService.protect(fd)`. Without it the relay loops — measured on this
 * emulator, sixteen client connections produced `tcp_open=2048` and not one byte
 * came back. So [create] takes the protector as a required argument and surfaces
 * the native refusal as an exception rather than letting a broken engine run.
 */
object Kernel {

    private const val TAG = "DetourKernel"

    /** Set once the native library is in. */
    private val loaded = AtomicBoolean(false)

    /** Why loading failed, when it did. Surfaced in the UI rather than thrown. */
    @Volatile
    var loadError: String? = null
        private set

    fun ensureLoaded(): Boolean {
        if (loaded.get()) return true
        if (loadError != null) return false
        return try {
            System.loadLibrary("watt_ffi")
            loaded.set(true)
            true
        } catch (err: UnsatisfiedLinkError) {
            // Not fatal: the UI still runs, and the settings screen shows this.
            // A missing ABI is a packaging mistake, not a reason to crash on
            // launch and leave the user with nothing to look at.
            loadError = err.message ?: "libwatt_ffi.so could not be loaded"
            Log.e(TAG, "native library unavailable", err)
            false
        }
    }

    /** Counters, mirroring `WattStats` in the header. */
    data class Stats(
        val packetsIn: Long = 0,
        val packetsOut: Long = 0,
        val tcpOpened: Long = 0,
        val tcpClosed: Long = 0,
        val tcpRejected: Long = 0,
        val tcpConnectFailures: Long = 0,
        val udpOpened: Long = 0,
        val udpEvicted: Long = 0,
        val dnsQueries: Long = 0,
        val dnsAnsweredLocally: Long = 0,
        val dnsTrimmed: Long = 0,
        val bytesToUpstream: Long = 0,
        val bytesToClient: Long = 0,
        val liveFlows: Long = 0,
        /** Flows relayed to a rule address. */
        val flowsMatchedRules: Long = 0,
        /** Flows relayed to whatever the client asked for. */
        val flowsDirect: Long = 0,
        /**
         * Flows relayed without the kernel knowing the domain.
         *
         * Separate from [flowsDirect]: that one says no rule matched, this says
         * there was nothing to match against. Rule lookup and the certificate
         * check are both gated on the name, so a client resolving over HTTPS —
         * the default in every current browser — lands here and gets neither.
         */
        val flowsWithoutName: Long = 0,
        /**
         * Flows that opened with no name and recovered one from the client's own
         * TLS handshake.
         *
         * The part of [flowsWithoutName] the handshake rescues. Read the two
         * together: their difference is the traffic still relayed blind, which is
         * the number that says whether the rule set is reachable for a client that
         * resolves elsewhere. On its own this counter cannot be judged — a large
         * value means many flows were rescued, and a large [flowsWithoutName] with
         * a small value here means they were not.
         */
        val flowsNamedBySni: Long = 0,
        /**
         * Flows the handshake named but did not move.
         *
         * The rest of the handshake's work. A name that arrives after the client's
         * bytes have gone out, or one that names the rule the address already
         * implied, is recovered and used for attribution but cannot change the
         * destination — and that is not a failure, it is a recovery with nothing
         * to do. [flowsNamedBySni] counts only the ones that changed a route, so
         * read the two together or the handshake looks far weaker than it is.
         */
        val flowsNamedWithoutMove: Long = 0,
        /**
         * Of [flowsNamedBySni], how many names arrived alongside ECH.
         *
         * GREASE ECH leaves the client's real name in the clear; real ECH puts a
         * cover name there. RFC 9849 designs the two to be indistinguishable from
         * outside, so a name taken under ECH is a bet. A non-zero value here is the
         * size of that bet, and it is the first place to look if flows are ever
         * seen steering to the wrong address.
         */
        val flowsNamedUnderEch: Long = 0,
        /**
         * Handshakes watched that ended with no name to take.
         *
         * Not a TLS stream, a ClientHello with no server_name, or a hello that
         * outgrew the buffer. Separates "the handshake could not help" from "the
         * handshake was never given a chance" — the second is [flowsWithoutName]
         * rising while this stays flat.
         */
        val hellosWithoutName: Long = 0,
        /**
         * Flows the configured upstream exit agreed to carry.
         *
         * Always zero when no exit is configured, which is what makes it the
         * answer to "is the exit in use" — the question the switch cannot answer,
         * since a switch only reports what the user asked for.
         */
        val proxyHandshakes: Long = 0,
        /**
         * Handshakes the exit refused, or never answered.
         *
         * Read with [proxyHandshakes]: a non-zero value here means the exit is
         * reachable and the problem is the request — wrong protocol, missing
         * credential, or the exit being unable to reach the destination. An exit
         * that is unreachable moves neither counter, so the pair distinguishes
         * the two failures that look identical on screen.
         */
        val proxyRefusals: Long = 0,
        /**
         * Names the upstream resolver was asked about.
         *
         * Only names the rule set does **not** own: a name with a rule is
         * answered from the rule table and never leaves the device. So this is
         * the size of the "outside the list" traffic, which is the number that
         * says whether the resolver is being used at all.
         */
        val dnsUpstreamQueries: Long = 0,
        /**
         * Questions the upstream answered.
         *
         * Read with [dnsUpstreamQueries] and [dnsUpstreamFailed]: a query that
         * is neither answered nor failed is still in flight, and one that is
         * both would be a bug in the counting rather than in the network.
         */
        val dnsUpstreamAnswered: Long = 0,
        /**
         * Questions that used every attempt and still got no answer.
         *
         * Non-zero is expected rather than alarming — the endpoint is measurably
         * intermittent — but a value near [dnsUpstreamQueries] means the
         * resolver is barely working and the client is being handed its own
         * polluted answer instead.
         */
        val dnsUpstreamFailed: Long = 0,
        /**
         * Attempts after the first.
         *
         * The counter that says how intermittent the endpoint is. It is normal
         * for this to be a fraction of [dnsUpstreamQueries]; it should not
         * approach it.
         */
        val dnsUpstreamRetries: Long = 0,
        /**
         * Questions the resolver was not offered, because too many were already
         * in flight.
         *
         * Each one was forwarded as it would have been without a resolver. A
         * non-zero value means the ceiling is being hit, which is a capacity
         * answer rather than a correctness one.
         */
        val dnsUpstreamOverflowed: Long = 0,
    ) {
        val bytesTotal: Long get() = bytesToUpstream + bytesToClient
    }

    /**
     * One address's dial history, as the selector remembers it.
     *
     * This is the *byte-based* half of the picture: successes and failures are
     * counted from whether a connection produced traffic, not from a
     * certificate check. The other half is [CertificateVerdict].
     */
    data class AddressStat(
        val ip: String,
        val successes: Long,
        val failures: Long,
        /** Connections that opened and then moved zero bytes in either direction. */
        val silent: Long,
        val consecutiveFailures: Long,
        /** Mean round trip in milliseconds, or null when never measured. */
        val ewmaRttMs: Double?,
        /** Success ratio in `0.0..1.0`; the kernel rounds it to three places. */
        val health: Double,
        val inCooldown: Boolean,
        val sinceSuccessMs: Long?,
        val sinceFailureMs: Long?,
    )

    /**
     * One host/address pair's certificate probe result.
     *
     * [verdict] is one of `in_flight`, `covers`, `wrong_certificate`,
     * `unreachable`. Kept as the raw string rather than an enum because the set
     * is owned by the kernel and a new value there should show up on the screen
     * as itself, not as an unknown-enum crash.
     */
    data class CertificateVerdict(
        val host: String,
        val ip: String,
        val verdict: String,
        val ageMs: Long,
    ) {
        /** False while the probe is still out; the row shows a spinner for that. */
        val isDecided: Boolean get() = verdict != "in_flight"
    }

    /**
     * The two read-only tables, parsed from `watt_engine_ip_stats`.
     *
     * The certificate list is keyed by host, not by address, because that is the
     * question the rules screen asks: a domain row wants "what do we know about
     * the addresses this domain resolves to". [verdictsFor] answers it.
     */
    data class IpStats(
        val addresses: List<AddressStat>,
        val certificates: List<CertificateVerdict>,
    ) {
        /**
         * Lookup indices, built once on first use.
         *
         * `verdictsFor` and `addressStat` are called from inside composition, and
         * the screens call them per *row*: a domain row asks once, but a group
         * row asks once per domain it holds, and a group can hold hundreds. A
         * linear scan per call makes those aggregate reads quadratic in the size
         * of the tables, which is the difference between a rollup being free and
         * being a visible stall on every recomposition. `groupBy` keeps the
         * kernel's original order within each group, so a caller that relies on
         * "the order the kernel reported them" still gets it.
         *
         * `lazy` rather than a constructor-time map because the tables are
         * populated by the kernel whether or not anyone opens this screen.
         */
        private val byHost: Map<String, List<CertificateVerdict>> by lazy {
            certificates.groupBy { it.host }
        }

        private val byIp: Map<String, AddressStat> by lazy {
            addresses.associateBy { it.ip }
        }

        /** Probe results for [host], in the order the kernel reported them. */
        fun verdictsFor(host: String): List<CertificateVerdict> =
            byHost[host].orEmpty()

        /** The dial history for [ip], or null when the selector has no sample. */
        fun addressStat(ip: String): AddressStat? = byIp[ip]

        companion object {
            /**
             * Parse the kernel's document. Throws on malformed input; the caller
             * turns that into "no data" rather than a message, because the only
             * way to get here malformed is a version mismatch.
             */
            fun parse(json: String): IpStats {
                val root = JSONObject(json)
                val addresses = root.optJSONArray("addresses") ?: JSONArray()
                val certificates = root.optJSONArray("certificates") ?: JSONArray()
                return IpStats(
                    addresses = (0 until addresses.length()).mapNotNull { index ->
                        addresses.optJSONObject(index)?.let { row ->
                            AddressStat(
                                ip = row.getString("ip"),
                                successes = row.optLong("successes"),
                                failures = row.optLong("failures"),
                                silent = row.optLong("silent"),
                                consecutiveFailures = row.optLong("consecutive_failures"),
                                ewmaRttMs = row.optDoubleOrNull("ewma_rtt_ms"),
                                health = row.optDouble("health", 0.0),
                                inCooldown = row.optBoolean("in_cooldown", false),
                                sinceSuccessMs = row.optLongOrNull("since_success_ms"),
                                sinceFailureMs = row.optLongOrNull("since_failure_ms"),
                            )
                        }
                    },
                    certificates = (0 until certificates.length()).mapNotNull { index ->
                        certificates.optJSONObject(index)?.let { row ->
                            CertificateVerdict(
                                host = row.getString("host"),
                                ip = row.getString("ip"),
                                verdict = row.optString("verdict"),
                                ageMs = row.optLong("age_ms"),
                            )
                        }
                    },
                )
            }
        }
    }

    /** The protector, as the C side wants it: a bare function pointer. */
    fun interface Protector {
        /** Return true when the descriptor was successfully exempted. */
        fun protect(fd: Int): Boolean
    }

    // --- native ---------------------------------------------------------------

    private external fun nativeNew(
        tunFd: Int,
        tunName: String?,
        rules: ByteArray,
        protect: Protector?,
        configJson: String?,
    ): Long

    private external fun nativeMerge(first: ByteArray, second: ByteArray, secondIsHosts: Boolean): ByteArray?

    private external fun nativeFilter(rules: ByteArray, disabled: Array<String>): ByteArray?

    private external fun nativeProxyStart(port: Int, rules: ByteArray): Long
    private external fun nativeProxyPort(handle: Long): Int
    private external fun nativeProxyReplaceRules(handle: Long, rules: ByteArray): Int
    private external fun nativeProxyStop(handle: Long)

    private external fun nativeStep(handle: Long): Int
    private external fun nativeReplaceRules(handle: Long, rules: ByteArray): Int
    private external fun nativeStats(handle: Long, out: LongArray): Int
    private external fun nativeIpStats(handle: Long): String?
    private external fun nativeUptime(handle: Long): Double
    private external fun nativeFree(handle: Long)
    private external fun nativeLastError(): String?

    /**
     * A running kernel.
     *
     * Close it to release the native side. The tunnel descriptor is **not**
     * closed here — it belongs to `VpnService`, and closing it twice would take a
     * descriptor belonging to something else.
     */
    class Engine internal constructor(handle: Long) : Closeable {

        /**
         * The native handle, and the lock that makes `close()` run once.
         *
         * `var handle: Long` read-then-zeroed is not a guard, it only looks like
         * one. Two threads can both read a non-zero value before either writes
         * zero, and both then call `nativeFree` on the same pointer — a double
         * free, which corrupts the allocator's metadata and surfaces later as a
         * SIGSEGV in whatever allocation happens next. On the device that was a
         * crash in `drop_glue<Planner>` inside `nativeFree`, one in
         * `RawVec::grow_one`, and one in the rule set's `ip_index` rehash, all
         * with the same cause and none of them where the bug is.
         *
         * Both callers are real: `teardown()` runs on the main thread from
         * `onDestroy` and `onRevoke`, while `fail()` runs on the engine's own
         * coroutine when `step()` reports an error. A teardown that races the
         * tunnel failing is not exotic — it is what happens whenever the app is
         * closed while the tunnel is unhealthy.
         *
         * `getAndSet(0)` is the whole fix: exactly one caller observes a
         * non-zero value, so exactly one calls `nativeFree`.
         */
        private val handle = java.util.concurrent.atomic.AtomicLong(handle)

        val isOpen: Boolean get() = handle.get() != 0L

        /** One pass over the tunnel. Returns the packets emitted, or -1. */
        fun step(): Int {
            val h = handle.get()
            if (h == 0L) return -1
            return nativeStep(h)
        }

        /** Swap the rule document. Returns true when the kernel accepted it. */
        fun replaceRules(document: ByteArray): Boolean {
            val h = handle.get()
            if (h == 0L) return false
            return nativeReplaceRules(h, document) == 0
        }

        /** The counters, or null when the native side refused. */
        fun stats(): Stats? {
            val h = handle.get()
            if (h == 0L) return null
            val raw = LongArray(STATS_FIELDS)
            if (nativeStats(h, raw) != 0) return null
            return Stats(
                packetsIn = raw[0], packetsOut = raw[1],
                tcpOpened = raw[2], tcpClosed = raw[3],
                tcpRejected = raw[4], tcpConnectFailures = raw[5],
                udpOpened = raw[6], udpEvicted = raw[7],
                dnsQueries = raw[8], dnsAnsweredLocally = raw[9],
                dnsTrimmed = raw[10],
                bytesToUpstream = raw[11], bytesToClient = raw[12],
                liveFlows = raw[13],
                flowsMatchedRules = raw[14],
                flowsDirect = raw[15],
                flowsWithoutName = raw[16],
                flowsNamedBySni = raw[17],
                proxyHandshakes = raw[18],
                proxyRefusals = raw[19],
                dnsUpstreamQueries = raw[20],
                dnsUpstreamAnswered = raw[21],
                dnsUpstreamFailed = raw[22],
                dnsUpstreamRetries = raw[23],
                dnsUpstreamOverflowed = raw[24],
                flowsNamedWithoutMove = raw[25],
                flowsNamedUnderEch = raw[26],
                hellosWithoutName = raw[27],
            )
        }

        fun uptimeSeconds(): Double {
            val h = handle.get()
            if (h == 0L) return 0.0
            return nativeUptime(h)
        }

        /**
         * The per-address dial history and the certificate probe verdicts.
         *
         * Read-only: nothing in the returned tables feeds back into a routing
         * decision, so calling it cannot perturb what the tunnel is doing. The
         * rules screen calls this once when it opens rather than on a timer —
         * the tables are small, but taking the selector and verdict locks on
         * every recomposition would put the screen on the dial path for no
         * reason.
         *
         * Null covers three cases the caller renders identically: the handle is
         * closed, the native side answered nothing, or the library predates
         * this symbol. The last is why the call is wrapped — a stale `.so` on
         * an upgraded install would otherwise turn opening the rules screen
         * into an `UnsatisfiedLinkError`.
         */
        fun ipStats(): IpStats? {
            val h = handle.get()
            if (h == 0L) return null
            val json = runCatching { nativeIpStats(h) }.getOrNull() ?: return null
            return runCatching { IpStats.parse(json) }.getOrNull()
        }

        override fun close() {
            val h = handle.getAndSet(0L)
            if (h != 0L) nativeFree(h)
        }
    }

    /**
     * Build an engine on a tunnel descriptor.
     *
     * @param tunFd      from `VpnService.establish()`, or from `ParcelFileDescriptor.detachFd()`
     * @param name       the interface name to report; "detour0" when null
     * @param rules      the merged rule document
     * @param protector  **required** — see the class comment
     * @param settings   the user's kernel settings as a flat JSON object. Every
     *                   key is optional and an unparsable document falls back to
     *                   the kernel's defaults, because a typo in a settings field
     *                   should not be able to stop the tunnel.
     * @throws KernelException when the native side refuses
     */
    @Throws(KernelException::class)
    fun create(
        tunFd: Int,
        name: String?,
        rules: ByteArray,
        protector: Protector,
        settings: String? = null,
    ): Engine {
        if (!ensureLoaded()) {
            throw KernelException(loadError ?: "native library unavailable")
        }
        if (rules.isEmpty()) {
            throw KernelException("the rule document is empty")
        }
        val handle = nativeNew(tunFd, name, rules, protector, settings)
        if (handle == 0L) {
            throw KernelException(nativeLastError() ?: "the kernel refused to start")
        }
        return Engine(handle)
    }

    /**
     * A running local HTTP proxy.
     *
     * This is the mode that needs **neither TUN nor root**: point an app's HTTP
     * proxy setting at [port] and its requests are relayed — `CONNECT` tunnels
     * unchanged, plain-HTTP requests forwarded to their origin. Nothing is refused
     * for being outside the rule set; an unlisted domain goes out directly, the
     * same way the tunnel treats one. The cost is that only apps that can be
     * pointed at a proxy benefit — it is not 全流量.
     *
     * It reads no kernel settings document, and that is a shape rather than an
     * oversight: `watt_proxy_start` in the C ABI takes rules and nothing else, so
     * every network parameter on the settings screen — timeouts, candidate counts,
     * certificate pre-check, the upstream exit, the upstream resolver — is
     * VPN-mode-only. The home screen's proxy card says so where the user is
     * looking.
     */
    class Proxy internal constructor(handle: Long, val port: Int) : Closeable {

        // Same guard as `Engine.close`, and for the same reason: a read-then-
        // zero `var` lets two threads both stop the proxy. See that method's
        // comment — the failure it prevents is a native double free, and the
        // crash it produces is never at the free.
        private val handle = java.util.concurrent.atomic.AtomicLong(handle)

        val isOpen: Boolean get() = handle.get() != 0L

        /**
         * Swap in a new rule document without cutting in-flight CONNECTs.
         *
         * Only routing decisions not yet made change, which is the same promise
         * [Engine.replaceRules] makes for the tunnel. Returns false when the
         * handle is closed or the native side refused the document.
         */
        fun replaceRules(document: ByteArray): Boolean {
            if (document.isEmpty()) return false
            val h = handle.get()
            if (h == 0L) return false
            if (!ensureLoaded()) return false
            return nativeProxyReplaceRules(h, document) == 0
        }

        override fun close() {
            val h = handle.getAndSet(0L)
            if (h != 0L) nativeProxyStop(h)
        }
    }

    /**
     * Start the local proxy on [port].
     *
     * Pass 0 to let the kernel choose a free port; the one it picked comes back
     * on the returned [Proxy].
     *
     * @throws KernelException when the port is taken or the rule document is bad
     */
    @Throws(KernelException::class)
    fun startProxy(port: Int, rules: ByteArray): Proxy {
        if (!ensureLoaded()) throw KernelException(loadError ?: "native library unavailable")
        if (rules.isEmpty()) throw KernelException("the rule document is empty")
        val handle = nativeProxyStart(port, rules)
        if (handle == 0L) {
            throw KernelException(nativeLastError() ?: "the proxy did not start")
        }
        return Proxy(handle, nativeProxyPort(handle))
    }

    /**
     * Apply the user's switches to a rule document.
     *
     * Keys are `g:<group>`, `d:<domain>`, `a:<address>`. Delegated to the kernel
     * rather than done here: the alternative is a second implementation of
     * "which domains does this entry claim", and the failure mode of the two
     * drifting is silent — a domain quietly stops being routed, or quietly keeps
     * being routed after the user switched it off.
     */
    @Throws(KernelException::class)
    fun filter(rules: ByteArray, disabled: Set<String>): ByteArray {
        if (!ensureLoaded()) throw KernelException(loadError ?: "native library unavailable")
        if (disabled.isEmpty()) return rules
        return nativeFilter(rules, disabled.toTypedArray())
            ?: throw KernelException(nativeLastError() ?: "the filter failed")
    }

    /** The last native failure on this thread, or null. */
    fun lastError(): String? = if (loaded.get()) nativeLastError() else loadError

    /**
     * Merge two rule documents, unioning each domain's addresses.
     *
     * Delegated to the kernel rather than done here: the merge is per *domain*,
     * not per entry, and the compiler keeps only the first entry that claims a
     * domain — so an implementation that keys on the entry would silently drop
     * the addresses the merge exists to keep. The Rust version has tests.
     */
    @Throws(KernelException::class)
    fun merge(first: ByteArray, second: ByteArray, secondIsHosts: Boolean): ByteArray {
        if (!ensureLoaded()) throw KernelException(loadError ?: "native library unavailable")
        return nativeMerge(first, second, secondIsHosts)
            ?: throw KernelException(nativeLastError() ?: "the merge failed")
    }

    /**
     * How many `jlong`s [nativeStats] writes.
     *
     * Must equal the length of the array the bridge fills, which is written out by
     * hand on the Rust side — a mismatch does not fail loudly, it silently shifts
     * every field after the gap. So this is the one number to check when a counter
     * is added.
     */
    private const val STATS_FIELDS = 28
}

class KernelException(message: String) : Exception(message)

/**
 * Where the native library is expected, for the diagnostics screen.
 *
 * Reported rather than probed: `System.loadLibrary` searches the app's own
 * `lib/<abi>/` and there is no supported way to ask it afterwards which file it
 * took.
 */
fun nativeLibraryHint(): String =
    File("lib").let { "libwatt_ffi.so (packaged per ABI)" }

/**
 * `optDouble` answers `NaN` for a missing key, and `NaN` is a real value in the
 * arithmetic the screen does — so it cannot double as "absent". The kernel sends
 * an explicit `null` for "never measured", and this is what reads it back.
 */
private fun JSONObject.optDoubleOrNull(key: String): Double? =
    if (isNull(key)) null else optDouble(key)

/**
 * `optLong` answers `0` for a missing key, which collides with a genuine zero
 * ("failed just now"). The kernel sends `null` when there is no sample at all.
 */
private fun JSONObject.optLongOrNull(key: String): Long? =
    if (isNull(key)) null else optLong(key)
