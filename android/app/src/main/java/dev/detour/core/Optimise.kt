package dev.detour.core

import android.content.Context
import dev.detour.R
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket

/**
 * The optimisation pass: measure the rule table's candidates once, and let the
 * order that produces reach the kernel.
 *
 * ## Why it is here and not in a mode
 *
 * Every mode dials candidates, and each one does it differently — the full-traffic
 * kernel races them, `watt-proxy` has its own dialer, `watt-mitm` resolves and
 * dials on its own. Measuring inside any of them would cover that mode and leave
 * the other two out.
 *
 * What all three *do* share is the rule document: it is loaded once, by this
 * class, and handed to whichever mode starts. So the measurement is applied to
 * the document on its way out of [RulesRepository.load], and the ordering reaches
 * all three at once — including modes that have no idea this exists.
 *
 * ## What is measured, and what is not
 *
 * A TCP connect, and only that. It is the one thing that is cheap enough to do
 * for every candidate (the s302-sourced document carries ~3000 of them), needs no
 * certificate handling, and answers the question that matters for ordering: is
 * this address alive from here, and how far away is it. TLS handshake time is
 * *not* measured — the kernel's certificate pre-check already decides whether an
 * address may be used at all, and a candidate that fails it is dropped there, not
 * here. Throughput is not measured either: s302's rate-limited download test is
 * the reference for that, and it costs orders of magnitude more per candidate.
 *
 * ## What the ordering is
 *
 * Per entry, three bands, each keeping the document's own relative order:
 *
 * 1. measured reachable — by latency, fastest first;
 * 2. never measured — untouched;
 * 3. measured unreachable — last.
 *
 * Band 2 sits *between* the other two on purpose. The document's order is a
 * claim by whoever wrote it; a measurement either confirms it or contradicts it,
 * and an unmeasured candidate is not evidence either way. Putting it after the
 * measured-good ones means a known-good address is tried first, while a document
 * that has never been measured comes out exactly as it went in.
 */
object Optimise {

    /**
     * One candidate's measurement.
     *
     * Keyed by the candidate *as the document writes it* — a name, or `!<address>`
     * — together with the port it was dialled on, which is the string [ordered]
     * looks up. Not by the resolved address: the document's entry is what gets
     * ordered, and two entries that name the same host share one measurement, which
     * is the point.
     *
     * The port belongs in the key. One name on two ports is two different
     * connections, and measuring `example.com:8443` says nothing about
     * `example.com:443`. It is also load-bearing for the reorder to work at all:
     * a map keyed by the bare candidate is a map [ordered] can never find anything
     * in, which makes the whole pass a silent no-op. See [run].
     */
    data class Probe(val ok: Boolean, val rttMs: Int, val atMillis: Long)

    /** The file the measurements live in, beside the rule cache. */
    private const val FILE = "optimise.json"

    /** The log tag. Named for this pass rather than for the rules it orders. */
    private const val TAG = "Optimise"

    /** Per candidate. Long enough for a slow path, short enough to bound the pass. */
    private const val TIMEOUT_MS = 3_000

    /**
     * How many candidates are dialled at once.
     *
     * The pass is I/O bound and the emulator this was developed on has two cores,
     * so the number is about not opening thousands of sockets at once rather than
     * about saturating a link. 32 keeps the whole pass under a minute on the
     * s302-sourced document while staying well inside the process's descriptor
     * budget (measured: 32768).
     */
    private const val CONCURRENCY = 32

    /** The port a candidate is measured on when its entry does not name one. */
    private const val DEFAULT_PORT = 443

    /** Parsed once, refreshed by [run]. `load` reads it on every call. */
    @Volatile
    private var cache: Map<String, Probe>? = null

    /** How far along the pass in flight is, or `null` when none is running. */
    data class Progress(val done: Int, val total: Int)

    private val _progress = MutableStateFlow<Progress?>(null)

    /** Observed by the rules screen. `null` means no pass is running. */
    val progress: StateFlow<Progress?> = _progress.asStateFlow()

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var running: Job? = null

    /**
     * Start a pass over the currently loaded document.
     *
     * Deliberately not a suspend function called from the command: the console's
     * dispatch is synchronous and a pass takes tens of seconds, so a command that
     * waited for it would hold the receiver's thread — and an adb broadcast would
     * time out long before the answer existed. This returns as soon as the pass is
     * *started*; the result arrives in the log and in [progress].
     *
     * @return false when a pass was already in flight.
     */
    fun start(context: Context): Boolean {
        if (running?.isActive == true) return false
        val app = context.applicationContext
        running = scope.launch {
            _progress.value = Progress(0, 0)
            val document = runCatching { RulesRepository.load(app) }.getOrNull()
            val measured = document?.let { run(app, it) { done, total -> _progress.value = Progress(done, total) } }
            _progress.value = null
            KernelState.log(
                app,
                if (measured == null) KernelState.LogEntry.Level.WARN else KernelState.LogEntry.Level.INFO,
                TAG,
                if (measured == null) R.string.log_optimise_failed else R.string.log_optimise_done,
                measured ?: 0,
            )
        }
        return true
    }

    /** Stop the pass in flight, if any. Its measurements are discarded. */
    fun cancel() {
        running?.cancel()
        running = null
        _progress.value = null
    }

    /** The measurements on disk, or an empty map. */
    fun probes(context: Context): Map<String, Probe> {
        cache?.let { return it }
        val loaded = runCatching {
            val file = file(context)
            if (!file.isFile) return emptyMap()
            val root = JSONObject(file.readText(Charsets.UTF_8))
            val entries = root.optJSONObject("probes") ?: return emptyMap()
            buildMap {
                for (key in entries.keys()) {
                    val one = entries.optJSONObject(key) ?: continue
                    put(
                        key,
                        Probe(
                            ok = one.optBoolean("ok", false),
                            rttMs = one.optInt("rtt", 0),
                            atMillis = one.optLong("at", 0L),
                        ),
                    )
                }
            }
        }.getOrDefault(emptyMap())
        cache = loaded
        return loaded
    }

    /** When the last pass finished, or 0. */
    fun measuredAt(context: Context): Long = runCatching {
        val file = file(context)
        if (!file.isFile) return 0L
        JSONObject(file.readText(Charsets.UTF_8)).optLong("at", 0L)
    }.getOrDefault(0L)

    /**
     * (reachable, total) over the stored measurements, for the rules screen.
     *
     * `total` counts measurements, not candidates in the document: the two differ
     * whenever a candidate was skipped (a name that resolves to loopback is not
     * measured at all — see [probe]), and a count that pretended otherwise would
     * report a reachability rate over addresses nobody tried.
     */
    fun summary(context: Context): Pair<Int, Int> {
        val probes = probes(context)
        return probes.values.count { it.ok } to probes.size
    }

    /**
     * Measure every candidate in [document] and store the result.
     *
     * Suspends, and is cancelled by cancelling the caller's scope — the pass is
     * user-triggered and a user who leaves the screen should not leave 3000
     * sockets behind. Partial results are **not** written: a pass that was
     * interrupted would otherwise reorder the document by a measurement that
     * only covers the candidates it happened to reach.
     *
     * @param onProgress called with (done, total) as candidates complete.
     * @return how many candidates were measured, or `null` when the pass was
     *         cancelled or the document could not be read.
     */
    suspend fun run(
        context: Context,
        document: ByteArray,
        onProgress: (Int, Int) -> Unit = { _, _ -> },
    ): Int? = withContext(Dispatchers.IO) {
        val targets = targets(document) ?: return@withContext null
        if (targets.isEmpty()) return@withContext 0

        val done = java.util.concurrent.atomic.AtomicInteger(0)
        val gate = Semaphore(CONCURRENCY)
        val measured = coroutineScope {
            targets.map { (candidate, port) ->
                async {
                    gate.withPermit {
                        val probe = probe(candidate, port)
                        onProgress(done.incrementAndGet(), targets.size)
                        // Keyed by candidate *and* port, the same key [targets]
                        // produced and the same one [ordered] looks up. Keying by
                        // the bare candidate here — which is what this did first —
                        // writes a file whose keys can never match, and the whole
                        // reorder then does nothing at all, silently: the pass
                        // completes, the file is full of real measurements, and the
                        // order never changes. Ports are not decoration, either:
                        // one name on two ports is two different connections.
                        key(candidate, port) to probe
                    }
                }
            }.awaitAll()
        }

        // `mapNotNull` on the probe, not on the pair: a pair is never null, so
        // `filterNotNull()` would keep every skipped candidate as a null value and
        // write "measured: null" into the file.
        val fresh = measured.mapNotNull { (k, probe) ->
            probe?.let { k to it }
        }.toMap()
        val merged = probes(context) + fresh
        write(context, merged, System.currentTimeMillis())
        fresh.size
    }

    /**
     * The document with each entry's candidates ordered by [probes].
     *
     * Returns [document] unchanged when nothing has been measured, when it is not
     * JSON this can read, or when no entry in it has a measurement — so an install
     * that never runs the pass hands the kernel exactly the bytes the source
     * published.
     */
    fun reorder(context: Context, document: ByteArray): ByteArray {
        val probes = probes(context)
        if (probes.isEmpty()) return document
        return runCatching {
            val root = JSONObject(String(document, Charsets.UTF_8))
            val groups = root.optJSONArray("groups") ?: return document
            var changed = false
            for (g in 0 until groups.length()) {
                val entries = groups.optJSONObject(g)?.optJSONArray("entries") ?: continue
                for (e in 0 until entries.length()) {
                    val entry = entries.optJSONObject(e) ?: continue
                    val ips = entry.optJSONArray("ips") ?: continue
                    val port = entry.optInt("port", DEFAULT_PORT)
                    val ordered = ordered(ips, port, probes) ?: continue
                    entry.put("ips", ordered)
                    changed = true
                }
            }
            if (changed) root.toString().toByteArray(Charsets.UTF_8) else document
        }.getOrDefault(document)
    }

    // --- internals -----------------------------------------------------------

    private fun file(context: Context): File = File(context.filesDir, FILE)

    private fun key(candidate: String, port: Int): String = "$candidate:$port"

    /** Every (candidate, port) the document asks for, de-duplicated. */
    private fun targets(document: ByteArray): List<Pair<String, Int>>? = runCatching {
        val root = JSONObject(String(document, Charsets.UTF_8))
        val groups = root.optJSONArray("groups") ?: return emptyList()
        val out = LinkedHashMap<String, Pair<String, Int>>()
        for (g in 0 until groups.length()) {
            val entries = groups.optJSONObject(g)?.optJSONArray("entries") ?: continue
            for (e in 0 until entries.length()) {
                val entry = entries.optJSONObject(e) ?: continue
                val ips = entry.optJSONArray("ips") ?: continue
                val port = entry.optInt("port", DEFAULT_PORT)
                for (i in 0 until ips.length()) {
                    val candidate = ips.optString(i).takeIf { it.isNotBlank() } ?: continue
                    out[key(candidate, port)] = candidate to port
                }
            }
        }
        out.values.toList()
    }.getOrNull()

    /**
     * One candidate, dialled.
     *
     * `null` when the candidate must not be measured at all: a name that resolves
     * to loopback is a name this app's own hosts entry owns (root mode points the
     * intercepted names at `127.0.0.2`), so a connect to it would land on the local
     * proxy — measuring the proxy, not the address. The kernel drops loopback from
     * dial-name resolution for the same reason (`resolve_dropping_loopback`), and
     * this is that rule applied one layer up.
     */
    private fun probe(candidate: String, port: Int): Probe? {
        val address = candidate.removePrefix("!")
        val started = System.currentTimeMillis()
        val target = runCatching { InetAddress.getByName(address) }.getOrNull() ?: return null
        if (target.isLoopbackAddress || target.isAnyLocalAddress) return null
        val ok = runCatching {
            Socket().use { socket ->
                socket.connect(InetSocketAddress(target, port), TIMEOUT_MS)
            }
            true
        }.getOrDefault(false)
        return Probe(ok, (System.currentTimeMillis() - started).toInt(), System.currentTimeMillis())
    }

    /**
     * The reordered candidates for one entry, or `null` when the entry has no
     * measurement and should be left alone.
     */
    private fun ordered(
        ips: JSONArray,
        port: Int,
        probes: Map<String, Probe>,
    ): JSONArray? {
        val candidates = (0 until ips.length()).map { ips.optString(it) }
        if (candidates.none { probes.containsKey(key(it, port)) }) return null
        val ranked = candidates.withIndex().sortedWith(
            compareBy(
                { band(probes[key(it.value, port)]) },
                { probes[key(it.value, port)]?.rttMs ?: Int.MAX_VALUE },
                { it.index },
            ),
        )
        val out = JSONArray()
        ranked.forEach { out.put(it.value) }
        return out
    }

    /** 0 reachable, 1 never measured, 2 unreachable — the three bands, in order. */
    private fun band(probe: Probe?): Int = when {
        probe == null -> 1
        probe.ok -> 0
        else -> 2
    }

    private fun write(context: Context, probes: Map<String, Probe>, at: Long) {
        runCatching {
            val entries = JSONObject()
            probes.forEach { (key, probe) ->
                entries.put(
                    key,
                    JSONObject()
                        .put("ok", probe.ok)
                        .put("rtt", probe.rttMs)
                        .put("at", probe.atMillis),
                )
            }
            val root = JSONObject().put("at", at).put("probes", entries)
            file(context).writeText(root.toString(), Charsets.UTF_8)
            cache = probes
        }
    }
}
