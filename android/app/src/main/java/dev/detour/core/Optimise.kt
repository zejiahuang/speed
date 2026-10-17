package dev.detour.core

import android.content.Context
import android.util.Log
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
import java.io.InputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.security.cert.X509Certificate
import javax.net.ssl.SNIHostName
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocket
import javax.net.ssl.X509TrustManager

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
 * Two things, and they answer different questions.
 *
 * **A TCP connect, for every candidate.** It is cheap enough to do for all of
 * them (the s302-sourced document carries ~3000), needs no certificate handling,
 * and answers "is this address alive from here, and how far away is it". TLS
 * handshake time is *not* measured — the kernel's certificate pre-check already
 * decides whether an address may be used at all, and a candidate that fails it is
 * dropped there, not here. Throughput is not measured either: s302's rate-limited
 * download test is the reference for that, and it costs orders of magnitude more
 * per candidate.
 *
 * **An HTTP GET, for the candidate that would win.** A live address is not the
 * same thing as an address that serves the name. Measured on the device: one
 * gravatar edge answered `302` with `Location` pointing at the very URL it was
 * asked for, while three other candidates for the same entry answered `200` with
 * the real page — so the fastest candidate turned the name into a redirect loop
 * (`net::ERR_TOO_MANY_REDIRECTS`) for every client, in all three modes. TCP alone
 * cannot see that: the address is up, the handshake completes, and the failure
 * only exists in the HTTP response.
 *
 * So after the connect pass, each entry's candidates are walked in latency order
 * and the first reachable one is asked for `GET /` with the entry's own name in
 * `Host`. A candidate that answers a 3xx whose `Location` resolves to the same
 * URL is moved behind the ones that do not. The walk stops at the first candidate
 * that answers something else, because that is the one that would be dialled — so
 * an entry costs one TLS handshake, two only when the first one is broken.
 *
 * That check is deliberately **not** a general HTTP probe: it does not follow
 * redirects, does not look at the body, and treats every failure as "no verdict"
 * rather than "bad". A candidate is demoted only on positive evidence, because a
 * wrong demotion costs a working address while a missed one costs nothing that
 * the old ordering did not already cost.
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
    data class Probe(
        val ok: Boolean,
        val rttMs: Int,
        val atMillis: Long,
        /**
         * The address answered, but answered a redirect back to the URL it was
         * asked for. Only ever set by [probeHttp] on positive evidence; `false`
         * also covers "never checked", which is why it is a demotion signal and
         * not a health flag.
         */
        val selfRedirect: Boolean = false,
    )

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

    /**
     * How long the HTTP phase may take, in total.
     *
     * It is the expensive half — a TLS handshake per entry — so it gets a wall
     * clock rather than a per-probe timeout alone. Entries that are not reached
     * before this expires keep their TCP verdict and are simply not demoted; the
     * pass still finishes and still writes what it learned.
     */
    private const val HTTP_BUDGET_MS = 60_000L

    /**
     * How many candidates of one entry the HTTP phase may try.
     *
     * One is the normal case. A second is what a broken first candidate costs,
     * and a third covers a document that lists two broken ones in a row. Beyond
     * that the entry is left alone: the ordering already put the rest in latency
     * order, and a long walk would spend the budget on the entries that are
     * least likely to be dialled anyway.
     */
    private const val HTTP_TRIES_PER_ENTRY = 3

    /**
     * The TLS context the HTTP phase dials with, built once.
     *
     * **It trusts everything, on purpose.** The address being probed is one whose
     * certificate is not expected to cover the name — that is the entire premise
     * of the rule document, and the kernel does not verify it either. What is
     * being measured here is the HTTP answer, not the chain; verifying would turn
     * every candidate into "no verdict" and the check into a no-op.
     */
    private val insecureTls: SSLContext by lazy {
        SSLContext.getInstance("TLS").apply {
            init(null, arrayOf(object : X509TrustManager {
                override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) = Unit
                override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) = Unit
                override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
            }), null)
        }
    }

    /**
     * How old a measurement may be before launch starts a fresh pass.
     *
     * 24 h, matching "一天一次": the point of the daily run is that yesterday's
     * numbers are stale by tomorrow, and a shorter window would re-measure on every
     * cold start — the cost this gate exists to avoid.
     */
    private const val DAY_MS = 24L * 60 * 60 * 1000

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

    /**
     * One pass per day, on launch, whatever mode is in force.
     *
     * The gate is the measurement timestamp already on disk ([measuredAt]), not a
     * new preference: "when was this last measured" is the fact the decision needs,
     * and a second copy of it could only disagree with the file the reorder reads.
     * A missing or unreadable file answers `0`, which is "never measured" and
     * therefore runs.
     *
     * **Runs in every mode on purpose, and that is only partly useful.** Root mode
     * rewrites `/etc/hosts` so the rule's own names resolve to `127.0.0.2`; [probe]
     * skips candidates that resolve to loopback, because measuring our own proxy
     * says nothing about the address behind it. So in root mode the named
     * candidates are skipped and only the literal `!<address>` ones are measured.
     * The pass still runs — the daily cadence is the user's ask, and the alternative
     * (silently doing nothing in root mode) would be indistinguishable from a pass
     * that found everything unreachable. How many were skipped is in the log line
     * the pass already writes.
     *
     * @return true when a pass was started.
     */
    fun maybeRunDaily(context: Context): Boolean {
        val last = measuredAt(context)
        val now = System.currentTimeMillis()
        if (last != 0L && now - last < DAY_MS) return false
        return start(context)
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
                            selfRedirect = one.optBoolean("self", false),
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
        val entries = entries(document) ?: return@withContext null
        val targets = entries.flatMap { entry ->
            entry.candidates.map { key(it, entry.port) to (it to entry.port) }
        }.toMap().values.toList()
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

        // Second phase, over what the first one found. Runs on the merged map so a
        // candidate measured on an earlier pass is still eligible — the document
        // did not change, and re-connecting to it just to re-learn the latency
        // would be the expensive half of a pass spent on nothing.
        val demoted = demoteSelfRedirects(entries, probes(context) + fresh, gate)
        val merged = probes(context) + fresh + demoted
        write(context, merged, System.currentTimeMillis())
        if (demoted.isNotEmpty()) {
            Log.i(TAG, "HTTP 自指重定向：${demoted.size} 个候选被降级")
        }
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
    /**
     * One document entry: the name it serves, the port, and its candidates in the
     * document's own order.
     *
     * The HTTP phase needs the name — it is what goes in `Host` — and the connect
     * phase does not, which is why [targets] flattens this and this exists
     * separately rather than the other way round.
     */
    private data class Entry(val host: String, val port: Int, val candidates: List<String>)

    private fun entries(document: ByteArray): List<Entry>? = runCatching {
        val root = JSONObject(String(document, Charsets.UTF_8))
        val groups = root.optJSONArray("groups") ?: return emptyList()
        val out = ArrayList<Entry>()
        for (g in 0 until groups.length()) {
            val groupEntries = groups.optJSONObject(g)?.optJSONArray("entries") ?: continue
            for (e in 0 until groupEntries.length()) {
                val entry = groupEntries.optJSONObject(e) ?: continue
                val ips = entry.optJSONArray("ips") ?: continue
                val port = entry.optInt("port", DEFAULT_PORT)
                val candidates = (0 until ips.length())
                    .mapNotNull { ips.optString(it).takeIf(String::isNotBlank) }
                if (candidates.isEmpty()) continue
                out += Entry(entry.optString("name"), port, candidates)
            }
        }
        out
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
     * Does this candidate answer a redirect back to the URL it was asked for?
     *
     * `true` = yes, `false` = it answered something else, `null` = no verdict
     * (connection refused, handshake refused, timeout, unparseable). The third
     * state is the important one: it is what keeps a candidate that merely
     * dislikes being probed from being demoted for it.
     *
     * The request is the smallest one that can produce the answer — `GET /`, the
     * entry's name in `Host`, `Connection: close`. No ALPN is offered, so a
     * server that speaks h2 falls back to HTTP/1.1 and the head is readable as
     * text; that is the whole reason this can be parsed without an HTTP client.
     */
    private fun probeHttp(host: String, candidate: String, port: Int): Boolean? {
        if (host.isBlank()) return null
        val address = candidate.removePrefix("!")
        val target = runCatching { InetAddress.getByName(address) }.getOrNull() ?: return null
        if (target.isLoopbackAddress || target.isAnyLocalAddress) return null
        return runCatching {
            Socket().use { raw ->
                raw.soTimeout = TIMEOUT_MS
                raw.connect(InetSocketAddress(target, port), TIMEOUT_MS)
                // A named candidate is dialled with its own name in SNI, which is
                // what the kernel does; `!<address>` means the source reached that
                // block with no server name at all, so none is sent here either.
                // The peer host is the *address*, not the entry's name. Java derives
                // SNI from the peer host it is given, so passing the name here would
                // put that name in SNI for `!<address>` candidates too — and an edge
                // that was reached without a name in the first place answers such a
                // hello with a reset, which would turn every one of them into "no
                // verdict" instead of the answer this probe exists to read.
                val tls = insecureTls.socketFactory.createSocket(raw, address, port, true) as SSLSocket
                tls.soTimeout = TIMEOUT_MS
                tls.sslParameters = tls.sslParameters.apply {
                    serverNames = if (candidate.startsWith("!")) emptyList() else listOf(SNIHostName(address))
                }
                tls.startHandshake()
                val request = buildString {
                    append("GET / HTTP/1.1\r\n")
                    append("Host: ").append(host).append("\r\n")
                    append("User-Agent: speed-optimise\r\n")
                    append("Accept: */*\r\n")
                    append("Connection: close\r\n\r\n")
                }
                tls.outputStream.write(request.toByteArray(Charsets.ISO_8859_1))
                tls.outputStream.flush()
                selfRedirectsTo(host, readHead(tls.inputStream))
            }
        }.getOrNull()
    }

    /** Up to 8 KiB of response head, or whatever arrived before the socket closed. */
    private fun readHead(input: InputStream): String {
        val buffer = ByteArray(1024)
        val out = java.io.ByteArrayOutputStream()
        while (out.size() < 8192) {
            val n = runCatching { input.read(buffer) }.getOrDefault(-1)
            if (n <= 0) break
            out.write(buffer, 0, n)
            if (out.toString("ISO-8859-1").contains("\r\n\r\n")) break
        }
        return out.toString("ISO-8859-1")
    }

    /**
     * True when [head] is a 3xx whose `Location` resolves to the URL that was
     * requested — `https://<host>/`.
     *
     * Relative locations (`/`) count: they point at the same place by definition.
     * So does the same URL with no path at all. The comparison is on host and
     * path, case-insensitively on the host, because that is the part a redirect
     * loop turns on; a scheme or port difference is not a loop this check is
     * meant to catch.
     */
    private fun selfRedirectsTo(host: String, head: String): Boolean {
        val lines = head.split("\r\n")
        val status = lines.firstOrNull()?.split(' ')
            ?.getOrNull(1)?.toIntOrNull() ?: return false
        if (status !in 300..399 || status == 304) return false
        val location = lines.firstOrNull {
            it.startsWith("Location:", ignoreCase = true)
        }?.substringAfter(':')?.trim() ?: return false
        if (location.isEmpty()) return false
        if (location.startsWith("/")) return true
        val rest = location.removePrefix("https://").removePrefix("http://")
        val locationHost = rest.substringBefore('/').substringBefore(':')
        val path = rest.substringAfter('/', "").substringBefore('?')
        return locationHost.equals(host, ignoreCase = true) && path.isBlank()
    }

    /**
     * The second phase: demote the candidates that answer a redirect to
     * themselves.
     *
     * Only entries with more than one candidate are worth asking — demoting the
     * only candidate of an entry changes nothing, and the pass would pay a
     * handshake per entry to learn it. Within an entry the walk is in latency
     * order and stops at the first candidate that answers something else, since
     * that is the one the kernel would dial.
     *
     * Returns only the entries it changed, so the caller can merge them over the
     * connect results without rebuilding the map.
     */
    private suspend fun demoteSelfRedirects(
        entries: List<Entry>,
        probes: Map<String, Probe>,
        gate: Semaphore,
    ): Map<String, Probe> {
        val deadline = System.currentTimeMillis() + HTTP_BUDGET_MS
        val interesting = entries.filter { it.candidates.size > 1 && it.host.isNotBlank() }
        if (interesting.isEmpty()) return emptyMap()
        return coroutineScope {
            interesting.map { entry ->
                async {
                    if (System.currentTimeMillis() > deadline) return@async emptyMap()
                    gate.withPermit {
                        val ranked = entry.candidates
                            .filter { probes[key(it, entry.port)]?.ok == true }
                            .sortedBy { probes[key(it, entry.port)]?.rttMs ?: Int.MAX_VALUE }
                        val found = HashMap<String, Probe>()
                        for (candidate in ranked.take(HTTP_TRIES_PER_ENTRY)) {
                            if (System.currentTimeMillis() > deadline) break
                            val verdict = probeHttp(entry.host, candidate, entry.port) ?: continue
                            if (!verdict) break
                            val k = key(candidate, entry.port)
                            probes[k]?.let { found[k] = it.copy(selfRedirect = true) }
                        }
                        found
                    }
                }
            }.awaitAll().fold(HashMap()) { acc, part -> acc.apply { putAll(part) } }
        }
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

    /**
     * 0 reachable, 1 never measured, 2 reachable but redirects to itself,
     * 3 unreachable — the four bands, in order.
     *
     * Band 2 sits below the unmeasured ones on purpose: a candidate measured to
     * answer a self-redirect is *known* to fail for this name, while an
     * unmeasured one is merely unknown. Putting the broken one first because it
     * was faster is exactly the bug this band exists to fix.
     */
    private fun band(probe: Probe?): Int = when {
        probe == null -> 1
        !probe.ok -> 3
        probe.selfRedirect -> 2
        else -> 0
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
                        .put("at", probe.atMillis)
                        .put("self", probe.selfRedirect),
                )
            }
            val root = JSONObject().put("at", at).put("probes", entries)
            file(context).writeText(root.toString(), Charsets.UTF_8)
            cache = probes
        }
    }
}
