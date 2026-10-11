package dev.detour.core

import android.content.Context
import android.net.ConnectivityManager
import dev.detour.R
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.sync.withPermit
import kotlinx.coroutines.withContext
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.util.concurrent.atomic.AtomicInteger

/**
 * The UDP/53 half of root mode.
 *
 * ## What it adds over the hosts file
 *
 * The hosts file only reaches clients that go through the system resolver. An app
 * that asks a *specific* server over plain DNS — a hardcoded `8.8.8.8`, a
 * per-network resolver, anything that is not the system's — never reads it, so its
 * connections arrive at the real address and the mode silently does nothing for
 * it. That is a documented limitation of this mode, and this is the standard fix:
 * redirect port 53 and answer it here.
 *
 * It does **not** close the other half of that limitation. Private DNS (DoT, port
 * 853) and an app's own DoH (HTTPS to a name we do not own) are not on port 53 and
 * are not touched — a client that resolves by itself, encrypted, still bypasses
 * this.
 *
 * ## Why it lives in Kotlin
 *
 * The rule names are already here — `RootHelper.domainsOf` produces exactly the
 * list the hosts block is built from — and a responder inside `watt-mitm` would
 * put a second name database next to the one that exists.
 *
 * ## The loop, and why the exemption is by port
 *
 * A redirect that catches *every* port-53 packet also catches the ones this class
 * sends upstream, so the forwarder would answer itself: each query becomes the
 * next query, and the device's DNS becomes a spin.
 *
 * The obvious exemption is `-m owner --uid-owner <app>`, and it is the one that
 * does not work: **the kernel on the device this was built against has no `owner`
 * match at all** — `/proc/net/ip_tables_matches` lists u32, time, string, state,
 * conntrack, mark, socket … and not owner — and `iptables` answers "Couldn't find
 * match `owner`", which, inside a batch, fails the whole install. Measured: the
 * first attempt at this feature died at step 17 of the root install for exactly
 * that reason.
 *
 * So the forwarder owns a **fixed source port**, and the chain returns before the
 * redirect for anything carrying it. That is stronger than a uid match would have
 * been: it cannot go stale when the uid changes (a reinstall does), it needs no
 * kernel module, and the property it establishes is exactly the one required — a
 * packet this class sent can never come back to it.
 *
 * ## Why the answering socket is bound to a literal
 *
 * `InetAddress.getLoopbackAddress()` answers `::1` on this platform, so
 * `DatagramSocket(0, it)` comes up **IPv6-only** — and the packets this socket
 * exists to receive are IPv4: `REDIRECT` in `nat` `OUTPUT` rewrites a port-53
 * packet's destination to `127.0.0.1`. An IPv6-only socket never sees them.
 *
 * The symptom is worth spelling out because it is invisible from the outside.
 * The socket binds, `localPort` is real, the firewall installs, and the device's
 * DNS then fails for every name the hosts file does not cover — which is the
 * opposite of the fail-open behaviour below. Measured on the device with the
 * redirect in place (`ss -ulpn`): `[::1]:51933` for the answering socket and
 * `*:53953` for the forwarding one — wrong family on the first, fine on the
 * second. The first build of this feature shipped exactly that.
 *
 * What made it look like it worked is worth keeping in mind, because it is the
 * trap: **a name in the rule set still resolved to the redirect address** — via
 * the hosts file, which those names never needed this class for. So "an in-rule
 * name resolves" is *not* evidence that anything here is alive, and neither is
 * "the firewall rules are in the table". [selfTest] is that evidence, which is
 * why it runs before the redirect is installed rather than after.
 *
 * ## Fail-open, on purpose
 *
 * A broken responder would take the device's DNS with it, which is a much larger
 * blast radius than anything else in this mode. So:
 *
 * * both sockets are bound **before** the redirect is installed; either failing
 *   means the redirect is never installed and DNS keeps working as it did;
 * * the redirect is removed **before** the sockets are closed, so there is no
 *   window where queries are sent to a port nothing listens on;
 * * a query that cannot be answered or forwarded gets a SERVFAIL — which a client
 *   treats as "ask someone else" — rather than silence, which it treats as a
 *   timeout.
 */
class DnsRedirect(
    private val context: Context,
    names: Set<String>,
    private val answer: String = "127.0.0.2",
) {

    companion object {
        /** How long to wait for the upstream before giving up on one query. */
        private const val FORWARD_TIMEOUT_MS = 1_500

        /** How many queries may be handled at once. */
        private const val IN_FLIGHT = 16

        /** A DNS message this app builds is far smaller; the limit is the client's. */
        private const val BUFFER = 1_500

        /** Used when the device will not say what its resolver is. */
        private const val FALLBACK_UPSTREAM = "223.5.5.5"

        /** The record type that gets an answer; every other type gets NODATA. */
        private const val TYPE_A = 1

        /** The log tag. */
        private const val TAG = "DnsRedirect"

        /**
         * The address the answering socket binds to.
         *
         * A literal IPv4 address, and not `InetAddress.getLoopbackAddress()`,
         * which answers `::1` here — see the class comment. This is also exactly
         * where `REDIRECT` in `nat` `OUTPUT` sends the packets, so it is the
         * address they carry.
         */
        private const val ANSWER_ADDRESS = "127.0.0.1"

        /** How long [selfTest] waits for its own answer. */
        private const val SELF_TEST_TIMEOUT_MS = 500

        /**
         * The transaction id [selfTest] sends, and checks on the way back.
         *
         * A constant rather than a random one: the query goes to ourselves, so a
         * mismatch means the loop is broken, and a fixed value makes that
         * unambiguous.
         */
        private const val SELF_TEST_ID = 0x4454

        /**
         * The source port the forwarder sends from, and the one the chain exempts.
         *
         * Fixed, because the exemption is a firewall rule written before any query
         * exists. Chosen high and odd so it collides with nothing a client picks on
         * its own; a client that did use it would merely have its own DNS left
         * alone, which is the harmless direction.
         */
        const val FORWARD_PORT = 53953
    }

    /**
     * The names to answer for, folded to lower case once.
     *
     * [question] lowercases the name it parses, and DNS names are
     * case-insensitive, so the set has to be folded the same way. `domainsOf`
     * copies the rule document verbatim, so a document that spelled a name with a
     * capital would otherwise fall through to the forwarder and be answered with
     * the real address — the interception silently not happening for that name.
     */
    private val names: Set<String> = names.mapTo(HashSet()) { it.lowercase() }

    /** How many queries have arrived; the first one is logged. See [handle]. */
    private val seen = AtomicInteger()

    /** How many were forwarded successfully; the first one is logged. */
    private val forwarded = AtomicInteger()

    private var socket: DatagramSocket? = null
    private var forward: DatagramSocket? = null
    private var job: Job? = null
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val gate = Semaphore(IN_FLIGHT)

    /** One upstream exchange at a time; see [forward]. */
    private val forwardLock = Mutex()

    /**
     * Bind, start answering, and prove it works.
     *
     * @return the port the redirect should point at, or `null` when the sockets
     *         could not be bound **or** [selfTest] did not get an answer — in
     *         either case the caller must not install the redirect at all.
     */
    fun start(): Int? {
        // See ANSWER_ADDRESS: a literal, because the loopback helper answers
        // `::1` here and an IPv6-only socket never sees these packets.
        val bound = runCatching {
            DatagramSocket(0, InetAddress.getByName(ANSWER_ADDRESS))
        }.getOrElse { error ->
            // Not silent: the mode still works, but the names a client resolves by
            // itself stop being covered, and nothing else would say so.
            KernelState.log(
                context,
                KernelState.LogEntry.Level.WARN,
                TAG,
                R.string.log_dns_no_socket,
                ANSWER_ADDRESS,
                error.javaClass.simpleName,
            )
            return null
        }
        val out = runCatching {
            DatagramSocket(FORWARD_PORT)
        }.getOrElse { error ->
            runCatching { bound.close() }
            KernelState.log(
                context,
                KernelState.LogEntry.Level.WARN,
                TAG,
                R.string.log_dns_no_socket,
                FORWARD_PORT.toString(),
                error.javaClass.simpleName,
            )
            return null
        }
        socket = bound
        forward = out
        job = scope.launch {
            val buffer = ByteArray(BUFFER)
            while (isActive) {
                val packet = DatagramPacket(buffer, buffer.size)
                val result = runCatching { bound.receive(packet) }
                val error = result.exceptionOrNull()
                if (error != null) {
                    // A closed socket is how `stop()` ends this loop, and that is
                    // not worth a line; anything else means the device's DNS just
                    // lost its responder.
                    if (isActive) {
                        KernelState.log(
                            context,
                            KernelState.LogEntry.Level.WARN,
                            TAG,
                            R.string.log_dns_stopped,
                            error.javaClass.simpleName,
                        )
                    }
                    break
                }
                val query = packet.data.copyOf(packet.length)
                val from = packet.address
                val port = packet.port
                // Wrapped because this is a `launch` on a plain Job: an exception
                // escaping `handle` would cancel the receiver that launched it,
                // and take the device's DNS with it.
                launch {
                    gate.withPermit {
                        runCatching { handle(bound, from, port, query) }.onFailure {
                            KernelState.log(
                                context,
                                KernelState.LogEntry.Level.WARN,
                                TAG,
                                R.string.log_dns_handle_failed,
                                question(query)?.name ?: "?",
                                it.javaClass.simpleName,
                            )
                        }
                    }
                }
            }
        }
        val port = bound.localPort
        if (!selfTest(port)) {
            KernelState.log(context, KernelState.LogEntry.Level.WARN, TAG, R.string.log_dns_unavailable)
            stop()
            return null
        }
        return port
    }

    /**
     * Ask ourselves one question and require an answer.
     *
     * This is the check the class comment describes: a socket that bound is not a
     * responder that answers, and the difference between the two is a device whose
     * DNS is silently dead. The question is one of our own names, so the answer is
     * built here — no network, no upstream, no firewall, nothing that can be slow
     * or absent. What it establishes is precisely the property that was broken:
     * that a query sent to `127.0.0.1:<port>` arrives at [socket] and comes back.
     *
     * It does not cover the redirect itself (that the firewall sends packets here)
     * or the forward path (that the upstream answers) — the first is logged on the
     * first query that arrives, the second on the first that fails.
     *
     * A `names` set that is empty skips the test rather than failing it: there is
     * then no name to ask about, and the caller's own code already treats an empty
     * set as "nothing to intercept".
     */
    private fun selfTest(port: Int): Boolean {
        val name = names.firstOrNull() ?: return true
        val probe = runCatching {
            DatagramSocket(0, InetAddress.getByName(ANSWER_ADDRESS))
        }.getOrNull() ?: return false
        val message = query(name)
        val ok = runCatching {
            probe.soTimeout = SELF_TEST_TIMEOUT_MS
            val to = InetAddress.getByName(ANSWER_ADDRESS)
            probe.send(DatagramPacket(message, message.size, to, port))
            val buffer = ByteArray(BUFFER)
            val packet = DatagramPacket(buffer, buffer.size)
            probe.receive(packet)
            packet.length >= 12 && packet.data[0] == message[0] && packet.data[1] == message[1]
        }.getOrDefault(false)
        runCatching { probe.close() }
        return ok
    }

    /**
     * True when [message] is [selfTest]'s own query rather than a client's.
     *
     * It arrives on the same socket as everything else, so it has to be
     * recognised: [handle] logs the first query it sees, and that line exists to
     * answer "is a redirected packet arriving at all" — a probe from this class
     * would answer it wrongly, in the affirmative, every time.
     */
    private fun isSelfTest(message: ByteArray): Boolean =
        message.size >= 2 &&
            message[0] == (SELF_TEST_ID shr 8).toByte() &&
            message[1] == SELF_TEST_ID.toByte()

    /** Stop answering and release both ports. Safe to call twice. */
    fun stop() {
        job?.cancel()
        job = null
        runCatching { socket?.close() }
        runCatching { forward?.close() }
        socket = null
        forward = null
        scope.cancel()
    }

    // --- one query -----------------------------------------------------------

    private suspend fun handle(
        socket: DatagramSocket,
        from: InetAddress,
        port: Int,
        query: ByteArray,
    ) {
        val question = question(query)
        // The first query is worth one line, and only the first: it is the
        // difference between "installed and answering" and "installed and pointed
        // at nothing", and from the client those two are the same silence.
        if (!isSelfTest(query) && seen.incrementAndGet() == 1) {
            KernelState.log(
                context,
                KernelState.LogEntry.Level.INFO,
                TAG,
                R.string.log_dns_query_seen,
                "${question?.name ?: "?"} ($from:$port)",
            )
        }
        if (question == null || question.name !in names) {
            // Not ours: forward it unchanged. A message this cannot even read is
            // forwarded for the same reason — passing it along is the only answer
            // that cannot make things worse.
            val reply = forward(query)
            val server = upstream()?.hostAddress ?: "no upstream"
            if (reply == null) {
                KernelState.log(
                    context,
                    KernelState.LogEntry.Level.WARN,
                    TAG,
                    R.string.log_dns_forward_failed,
                    question?.name ?: "?",
                    "$from:$port -> $server",
                )
            } else if (forwarded.incrementAndGet() == 1) {
                // The first success is worth a line for the same reason: it is
                // what says the exemption on FORWARD_PORT works and the upstream
                // is reachable, neither of which can be seen from the client.
                KernelState.log(
                    context,
                    KernelState.LogEntry.Level.INFO,
                    TAG,
                    R.string.log_dns_forwarded,
                    question?.name ?: "?",
                    server,
                )
            }
            send(socket, reply ?: servfail(query), from, port, question?.name ?: "?")
            return
        }
        // Ours. An A query gets the redirect address; anything else gets NODATA —
        // an empty answer with the question echoed, which is how a client is told
        // "this name exists, it has no record of that type" and falls back to A.
        // Answering AAAA or HTTPS with an address we do not have would be a lie the
        // client acts on.
        val reply = if (question.type == TYPE_A) {
            withAnswer(query, question)
        } else {
            header(query, question, answers = 0)
        }
        send(socket, reply, from, port, question.name)
    }

    /**
     * Send one reply, and record it when that fails.
     *
     * A `send` failure here is the one part of this class that cannot be inferred
     * from the outside: the client sees a timeout, which is exactly what it would
     * see if the responder were not running at all.
     */
    private fun send(socket: DatagramSocket, reply: ByteArray, to: InetAddress, port: Int, name: String) {
        runCatching { socket.send(DatagramPacket(reply, reply.size, to, port)) }
            .onFailure {
                KernelState.log(
                    context,
                    KernelState.LogEntry.Level.WARN,
                    TAG,
                    R.string.log_dns_reply_failed,
                    "$name -> $to:$port (${it.javaClass.simpleName})",
                )
            }
    }

    /**
     * Ask the device's own resolver, one query at a time.
     *
     * Serialised because there is exactly one forwarding socket: its source port is
     * what the firewall exempts, and a second socket cannot share that port. One at
     * a time is the price of an exemption that does not depend on the kernel having
     * an `owner` match — and DNS is a request/response exchange that answers in
     * milliseconds, so a queue only forms when an upstream is slow, which the
     * timeout bounds anyway.
     *
     * The transaction id is checked on the way back. A query that timed out leaves
     * its reply to arrive later, and without the check the next query would be
     * handed the previous one's answer.
     */
    private suspend fun forward(query: ByteArray): ByteArray? = withContext(Dispatchers.IO) {
        val out = forward ?: return@withContext null
        val server = upstream() ?: return@withContext null
        forwardLock.withLock {
            runCatching {
                out.soTimeout = FORWARD_TIMEOUT_MS
                out.send(DatagramPacket(query, query.size, server, 53))
                val buffer = ByteArray(BUFFER)
                val packet = DatagramPacket(buffer, buffer.size)
                out.receive(packet)
                val reply = packet.data.copyOf(packet.length)
                if (reply.size < 2 || query.size < 2) return@runCatching null
                if (reply[0] != query[0] || reply[1] != query[1]) return@runCatching null
                reply
            }.getOrNull()
        }
    }

    /**
     * The device's own resolver, or the fallback.
     *
     * Read per query rather than cached: the device changes networks while the mode
     * is up, and a stale server is a black hole. Loopback is skipped — a loopback
     * resolver here would be this app answering itself.
     */
    private fun upstream(): InetAddress? = runCatching {
        val manager = context.getSystemService(ConnectivityManager::class.java) ?: return null
        val network = manager.activeNetwork ?: return null
        val properties = manager.getLinkProperties(network) ?: return null
        properties.dnsServers.firstOrNull { !it.isLoopbackAddress && !it.isAnyLocalAddress }
    }.getOrNull() ?: runCatching { InetAddress.getByName(FALLBACK_UPSTREAM) }.getOrNull()

    // --- the wire format -----------------------------------------------------

    private data class Question(val name: String, val type: Int, val end: Int)

    /**
     * A minimal A query for [name], used by [selfTest].
     *
     * The header is `SELF_TEST_ID`, RD set, one question. Only the pieces
     * [question] reads and [header] copies are filled in: this message never
     * leaves the process.
     */
    private fun query(name: String): ByteArray {
        val labels = name.trimEnd('.').split('.').filter { it.isNotEmpty() }
        val out = ByteArray(12 + labels.sumOf { it.length + 1 } + 1 + 4)
        out[0] = (SELF_TEST_ID shr 8).toByte()
        out[1] = SELF_TEST_ID.toByte()
        out[2] = 0x01                   // RD
        out[5] = 1                      // QDCOUNT
        var i = 12
        for (label in labels) {
            out[i++] = label.length.toByte()
            for (c in label) out[i++] = c.code.toByte()
        }
        out[i++] = 0                    // end of the name
        out[i++] = 0                    // QTYPE high
        out[i++] = TYPE_A.toByte()
        out[i++] = 0                    // QCLASS high
        out[i++] = 0x01
        return out
    }

    /**
     * The first question in [message], or `null` if it cannot be read.
     *
     * Only the first, and only in the uncompressed form: a query's QNAME is never
     * compressed — there is nothing before it to point at — so a pointer here means
     * the message is not a query this should be answering.
     */
    private fun question(message: ByteArray): Question? {
        if (message.size < 13) return null
        val out = StringBuilder()
        var i = 12
        while (i < message.size) {
            val length = message[i].toInt() and 0xff
            if (length == 0) {
                i += 1
                if (i + 4 > message.size) return null
                val type = ((message[i].toInt() and 0xff) shl 8) or (message[i + 1].toInt() and 0xff)
                return Question(out.toString().lowercase(), type, i + 4)
            }
            if (length > 63 || i + 1 + length > message.size) return null
            if (out.isNotEmpty()) out.append('.')
            out.append(String(message, i + 1, length, Charsets.US_ASCII))
            i += 1 + length
        }
        return null
    }

    /** A response with the question echoed and no answers. */
    private fun header(query: ByteArray, question: Question, answers: Int, rcode: Int = 0): ByteArray {
        val reply = ByteArray(12 + (question.end - 12))
        query.copyInto(reply, 0, 0, 12)
        reply[2] = ((query[2].toInt() and 0x01) or 0x80).toByte()   // QR, RD copied
        reply[3] = (0x80 or (rcode and 0x0f)).toByte()              // RA, RCODE
        reply[5] = 1                                                // QDCOUNT
        reply[7] = answers.toByte()                                 // ANCOUNT
        query.copyInto(reply, 12, 12, question.end)
        return reply
    }

    /** The response for an A query: the redirect address, TTL 60. */
    private fun withAnswer(query: ByteArray, question: Question): ByteArray {
        val base = header(query, question, answers = 1)
        val address = InetAddress.getByName(answer).address
        val reply = base.copyOf(base.size + 16)
        var i = base.size
        reply[i++] = 0xC0.toByte()      // name: pointer to offset 12
        reply[i++] = 0x0C
        reply[i++] = 0x00               // type A
        reply[i++] = TYPE_A.toByte()
        reply[i++] = 0x00               // class IN
        reply[i++] = 0x01
        reply[i++] = 0                  // TTL = 60
        reply[i++] = 0
        reply[i++] = 0
        reply[i++] = 60
        reply[i++] = 0                  // RDLENGTH = 4
        reply[i++] = 4
        address.copyInto(reply, i)
        return reply
    }

    /** A response saying "ask someone else", used when forwarding failed. */
    private fun servfail(query: ByteArray): ByteArray {
        val question = question(query)
        return if (question == null) {
            ByteArray(12).also {
                if (query.size >= 2) query.copyInto(it, 0, 0, 2)
                it[2] = 0x80.toByte()
                it[3] = 0x82.toByte()
            }
        } else {
            header(query, question, answers = 0, rcode = 2)
        }
    }
}
