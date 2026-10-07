package dev.detour.core

import android.content.Context
import java.io.File
import java.security.MessageDigest
import java.util.concurrent.TimeUnit

/**
 * The privileged half of root mode.
 *
 * Root mode is three things that only work together, and this object owns all
 * three so that none of them can be left half-applied:
 *
 * 1. **The system hosts file.** Intercepted names are pointed at
 *    [REDIRECT_ADDRESS] so the client dials an address we control instead of the
 *    real one. Measured on this device: the resolver honours `/etc/hosts` ahead
 *    of DNS for the ordinary `getaddrinfo` path every app uses — a name mapped to
 *    `127.0.0.2` resolved there, over the real DNS answer. Not measured, and
 *    deliberately not claimed: an app that resolves names itself never reads
 *    `/etc/hosts`, so this edit reaches only clients that go through the system
 *    resolver. Private DNS (DoT) and a browser's own Secure DNS (DoH) both
 *    resolve out of band and bypass the file, which is why root mode does not
 *    capture all traffic.
 * 2. **The system trust store.** A self-signed CA is installed so the client
 *    accepts the certificates the proxy mints. Without it every connection the
 *    proxy terminates fails verification and nothing on the wire explains why.
 * 3. **An iptables redirect.** Port 443 to [REDIRECT_ADDRESS] is rewritten to the
 *    proxy's loopback port. The proxy cannot bind 443 itself — measured
 *    `net.ipv4.ip_unprivileged_port_start = 1024` — so the redirect is what makes
 *    an unprivileged listener look like the real endpoint.
 *
 * ## Why the class is a lease, not a set of statics
 *
 * Every one of the three edits something outside the app, and a failure halfway
 * through has to leave the device exactly as it was found — a hosts entry
 * pointing at a dead loopback address is a device-wide outage, not a broken app.
 * So [Session] records what it changed as it changes it, and both the failure
 * path inside [Session.apply] and the normal stop path call the same
 * [Session.restore]. There is no separate "undo the partial install" code to
 * drift from the "undo the complete install" code, because there is only one.
 *
 * ## Why recovery is the same operation as restore
 *
 * A process that is killed mid-install never runs [Session.restore], so the next
 * run has to be able to undo the leftovers before it starts. That is not a
 * second code path either: [Session.apply] begins by running the same undo it
 * would run on stop, and only then installs. Anything a crashed run left behind
 * is gone before the new run's first write, which is what makes "retry after a
 * failure" safe rather than additive.
 *
 * ## What this deliberately does not do
 *
 * It does not touch the network namespace, the app's own traffic, or any
 * firewall chain other than the single `OUTPUT` rule it adds. The rule is
 * matched on the exact destination and port it installs, so it cannot capture
 * unrelated traffic, and it is removed by the exact specification it was added
 * with.
 */
object RootHelper {

    const val TAG = "DetourRoot"

    /**
     * Where the entries point.
     *
     * `127.0.0.2`, not `127.0.0.1`: the redirect below is written against this
     * address, and keeping the marker address distinct from loopback proper means
     * the rule can be written to match only the traffic we deliberately routed
     * here. The proxy itself listens on `127.0.0.1` — measured, the kernel rewrites
     * a redirected packet's destination to loopback — so this is a marker, not a
     * listener.
     */
    const val REDIRECT_ADDRESS = "127.0.0.2"

    /** The port the client believes it is talking to, and therefore the one to redirect. */
    private const val REDIRECT_PORT = 443

    /**
     * The two trust stores, and why both are written.
     *
     * Measured on this device (Android 14): the two are **not** the same
     * directory. `/apex/com.android.conscrypt/cacerts` is a read-only APEX mount
     * and `/system/etc/security/cacerts` is a separate writable directory — they
     * hold the same 134 certificates as *copies*, with different inodes. Conscrypt
     * on Android 14 reads the APEX store, but older releases read the system one,
     * so the certificate is installed into both: the writable one directly, and
     * the read-only one by bind-mounting a mirror over it.
     *
     * The bind mount is what makes the read-only store writable in effect, and it
     * is **global**: measured on this device, a bind mount made from one `su`
     * process was still visible from a separate, non-root `adb shell` after that
     * process had exited. That measurement is the whole reason the mirror
     * technique is sound — it really does cover every process that reads the
     * store, not only the one that mounted it.
     *
     * **Coverage caveat — expected, not measured here.** Android reads the trust
     * store when a process starts, so an app that is already running may keep the
     * trust set it was born with until it is restarted, and a user could see a
     * certificate error in an already-open app until they relaunch it. We did not
     * measure this on the device; it is stated as the documented behaviour of a
     * process-start read, not as a confirmed result.
     */
    private const val SYSTEM_CACERTS = "/system/etc/security/cacerts"
    private const val APEX_CACERTS = "/apex/com.android.conscrypt/cacerts"

    private const val HOSTS = "/etc/hosts"

    /**
     * The staging directory, outside the app's own data.
     *
     * The app stages the certificate and the hosts block in its private
     * `filesDir` (root can read it), but the *markers* and the APEX mirror live
     * here because they are what a later run reads to know what to undo, and they
     * must survive the app being killed. `/data/local/tmp` is the conventional
     * place for exactly this and is present on every Android build.
     */
    private const val STAGE = "/data/local/tmp/detour-root"

    /** The marker that closes our block in the hosts file, for a human reading it. */
    private const val HOSTS_BEGIN = "# detour-root-begin"
    private const val HOSTS_END = "# detour-root-end"

    /**
     * Whether a usable `su` is present, cached once per process.
     *
     * **Blocking.** It spawns a process and may raise a consent prompt the first
     * time, so it must be called off the main thread. Only a positive result is
     * cached: a negative one is re-checked on the next call, because the usual
     * reason for it is that the user has not granted root yet, and pinning that
     * "no" for the life of the process would mean a grant could not take effect
     * without a restart.
     */
    @Volatile
    private var rootGranted: Boolean = false

    fun isAvailable(): Boolean {
        if (rootGranted) return true
        val granted = runCatching {
            val result = Su().run(listOf("id"), TIMEOUT_MS)
            result.ok && result.output.contains("uid=0")
        }.getOrDefault(false)
        if (granted) rootGranted = true
        return granted
    }

    /**
     * Open a lease. Returns null when there is no root to lease.
     *
     * The caller must [Session.restore] it — on stop, and on any failure after it
     * was opened. Both the service's teardown and its failure path do.
     */
    fun open(context: Context): Session? {
        if (!isAvailable()) return null
        return Session(context.applicationContext)
    }

    /**
     * Undo anything a previous run left behind, without installing anything.
     *
     * Called at startup for a device whose stored mode is root, so a run that was
     * killed mid-install does not leave the system hosts pointing at a dead
     * loopback address until the user happens to reconnect. It is the same undo
     * [Session.restore] runs, exposed because that is the only correct way to
     * clean up outside a live session.
     *
     * Best-effort and silent: every step is idempotent, and a failure here means
     * the files were already in the state it wanted them in.
     */
    fun recoverStale(context: Context) {
        if (!isAvailable()) return
        runCatching { Su().run(undoCommands(), TIMEOUT_MS) }
        // The `su` side cannot reach the app's private files, so the staging dir is
        // cleared here instead, on the same best-effort terms as the undo above.
        runCatching { stageDir(context).deleteRecursively() }
    }

    /**
     * The app-private staging dir: the CA copy and the intercepted-domain list.
     *
     * A cache directory the system is free to reclaim — but not one to *rely* on
     * it reclaiming, because what it holds is a list of the domains this run
     * intercepted, and that list should not outlive the session on disk. Defined
     * once so [Session.stage] and the two cleanup paths cannot drift apart.
     */
    private fun stageDir(context: Context): File = File(context.cacheDir, "root-stage")

    /**
     * The `X509_NAME_hash_old` of [certPem], as an eight-character lowercase hex
     * string, or null when the certificate cannot be parsed.
     *
     * **This is measured, not assumed.** The filenames in the device's trust store
     * are this hash and not `openssl x509 -subject_hash`: on forty real system
     * certificates pulled from the device, `-subject_hash` (SHA-1, big-endian)
     * matched the filename zero times, while `X509_NAME_hash_old` matched all
     * forty. The algorithm is:
     *
     * 1. take the DER encoding of the certificate's **subject** field (the
     *    `SEQUENCE` of RDN `SET`s, exactly as it appears in the certificate — the
     *    raw bytes, not a re-encoding);
     * 2. MD5 it;
     * 3. read the first four bytes as a little-endian `uint32`;
     * 4. print them as eight hex digits.
     *
     * Step 3 is the part that is easy to get wrong: the digest bytes are
     * little-endian, so `MD5(...)[0..3]` printed big-endian is the byte-reverse of
     * the filename, and a hash computed that way is accepted by nothing.
     */
    fun subjectHash(certPem: String): String? = runCatching {
        val der = pemToDer(certPem)
        val subject = subjectOf(tbsOf(der))
        val digest = MessageDigest.getInstance("MD5").digest(subject)
        val value = (digest[0].toLong() and 0xFFL) or
            ((digest[1].toLong() and 0xFFL) shl 8) or
            ((digest[2].toLong() and 0xFFL) shl 16) or
            ((digest[3].toLong() and 0xFFL) shl 24)
        "%08x".format(value and 0xFFFFFFFFL)
    }.getOrNull()

    /**
     * The domain names a rule document claims, in first-seen order.
     *
     * The document is the kernel's own JSON (`groups[].entries[].domains[]`), so
     * this is a read of that shape rather than a second parser for the hosts
     * text.
     *
     * An empty result is a real state — it is what the rule set looks like before
     * the first fetch lands — and its consequence is silent: the tunnel comes up
     * and intercepts nothing. So both a malformed document and a valid-but-empty
     * one are logged here, at the moment they are seen, rather than being allowed
     * to masquerade as a healthy start.
     */
    fun domainsOf(document: ByteArray): List<String> {
        val names = runCatching {
            val groups = org.json.JSONObject(String(document, Charsets.UTF_8))
                .optJSONArray("groups")
                ?: throw IllegalStateException("缺少 groups 字段")
            val collected = LinkedHashSet<String>()
            for (g in 0 until groups.length()) {
                val entries = groups.optJSONObject(g)?.optJSONArray("entries") ?: continue
                for (e in 0 until entries.length()) {
                    val domains = entries.optJSONObject(e)?.optJSONArray("domains") ?: continue
                    for (d in 0 until domains.length()) {
                        domains.optString(d).takeIf { it.isNotBlank() }?.let { collected.add(it) }
                    }
                }
            }
            collected.toList()
        }.getOrElse { err ->
            KernelState.log(
                KernelState.LogEntry.Level.WARN,
                TAG,
                "规则文档解析失败，root 模式将不拦截任何域名：${err.message}",
            )
            emptyList()
        }
        if (names.isEmpty()) {
            KernelState.log(
                KernelState.LogEntry.Level.WARN,
                TAG,
                "规则文档未包含任何域名，root 模式已启动但不拦截任何流量",
            )
        }
        return names
    }

    private const val TIMEOUT_MS = 20_000L

    /**
     * The steps that undo an install, in the order they have to run.
     *
     * Every one is guarded so it is a no-op when its marker is absent, which is
     * what makes this safe to run when nothing was installed and safe to run
     * twice. The markers are removed **last**, so a crash partway through an undo
     * leaves the remaining markers in place and the next run finishes the job
     * instead of believing it is already done.
     */
    private fun undoCommands(): List<String> = listOf(
        // 1. Put the hosts file back the way it was found.
        "[ -f $STAGE/hosts.orig ] && cp -f $STAGE/hosts.orig $HOSTS",
        // 1b. Belt and braces: if the backup is gone but our block somehow is not,
        //     strip it by its own markers rather than leaving names pointed at a
        //     dead loopback address. A no-op when step 1 already restored the file.
        "sed -i '/$HOSTS_BEGIN/,/$HOSTS_END/d' $HOSTS",
        // 2. Remove the certificate from the writable store. The path was
        //    recorded, not recomputed, because a recovery run may not have the
        //    certificate in hand.
        "[ -f $STAGE/ca.path ] && rm -f \"\$(cat $STAGE/ca.path)\"",
        // 3. Drop the APEX bind mount (only if we made it), then delete the mirror
        //    only once the mount is confirmed gone. If the umount above failed,
        //    removing the mount source would leave the APEX cacerts directory bound
        //    to a deleted path — an empty trust store, which is a device-wide TLS
        //    failure, not an app-local one. The marker is kept for the next retry.
        "[ -f $STAGE/apex.mounted ] && umount $APEX_CACERTS",
        "mount | grep -q \" $APEX_CACERTS \" || { rm -rf $STAGE/apex; rm -f $STAGE/apex.mounted; }",
        // 4. Remove the redirect, using the port it was added with.
        "[ -f $STAGE/redirect.port ] && iptables -t nat -D OUTPUT -p tcp -d " +
            "$REDIRECT_ADDRESS --dport $REDIRECT_PORT -j REDIRECT " +
            "--to-ports \"\$(cat $STAGE/redirect.port)\"",
        // 5. Forget the markers only once every undo above has been attempted.
        //    `apex.mounted` is deliberately absent here: the umount check above
        //    clears it, and listing it here would discard the marker a still-live
        //    mount needs in order to be retried.
        "rm -f $STAGE/hosts.orig $STAGE/ca.path $STAGE/redirect.port",
        // 6. Remove the stage directory itself. It only goes away when every file
        //    above is gone, so a still-mounted apex keeps it (and its markers) in
        //    place for the next run rather than hiding the unfinished work.
        "rmdir $STAGE 2>/dev/null",
    )

    // --- DER ------------------------------------------------------------------

    private data class Tlv(val tag: Int, val valueStart: Int, val valueLen: Int) {
        val valueEnd: Int get() = valueStart + valueLen
    }

    /** Read one tag-length-value at [offset]. */
    private fun readTlv(bytes: ByteArray, offset: Int): Tlv {
        var at = offset
        val tag = bytes[at].toInt() and 0xFF
        at++
        var length = bytes[at].toInt() and 0xFF
        at++
        if (length and 0x80 != 0) {
            val count = length and 0x7F
            length = 0
            for (i in 0 until count) {
                length = (length shl 8) or (bytes[at].toInt() and 0xFF)
                at++
            }
        }
        return Tlv(tag, at, length)
    }

    /** The `tbsCertificate` bytes of a certificate. */
    private fun tbsOf(der: ByteArray): ByteArray {
        val certificate = readTlv(der, 0)          // Certificate ::= SEQUENCE
        val start = certificate.valueStart
        val tbs = readTlv(der, start)              // tbsCertificate
        return der.copyOfRange(start, tbs.valueEnd)
    }

    /**
     * The `subject` bytes inside a `tbsCertificate`.
     *
     * The field order is fixed by RFC 5280: an optional `[0]` version, then
     * serial, signature algorithm, issuer, **validity**, and only then subject.
     * Validity is the one that is easy to forget, and forgetting it yields the
     * two `Time` values instead — bytes that parse as DER and hash to a value no
     * trust store ever looks up.
     */
    private fun subjectOf(tbs: ByteArray): ByteArray {
        val outer = readTlv(tbs, 0)
        var at = outer.valueStart
        var field = readTlv(tbs, at)
        if (field.tag == 0xA0) {                    // [0] EXPLICIT version
            at = field.valueEnd
            field = readTlv(tbs, at)
        }
        at = field.valueEnd                          // skip serialNumber
        repeat(3) {                                  // signature, issuer, validity
            field = readTlv(tbs, at)
            at = field.valueEnd
        }
        field = readTlv(tbs, at)                     // subject
        return tbs.copyOfRange(at, field.valueEnd)
    }

    /** Decode the first PEM block, ignoring anything around it. */
    private fun pemToDer(pem: String): ByteArray {
        val base64 = StringBuilder()
        var inside = false
        for (line in pem.lines()) {
            val trimmed = line.trim()
            if (trimmed.startsWith("-----BEGIN")) { inside = true; continue }
            if (trimmed.startsWith("-----END")) break
            if (inside) base64.append(trimmed)
        }
        return android.util.Base64.decode(base64.toString(), android.util.Base64.DEFAULT)
    }

    /**
     * One install, and the ability to undo it.
     *
     * Not thread-safe on purpose: it is driven from the service's single start
     * path and its single stop path, and a lease that could be restored from two
     * threads at once would need a lock to say what "restored" means. The service
     * guarantees the ordering.
     */
    class Session internal constructor(private val context: Context) {

        private val shell = Su()

        private var blockPath: File? = null

        /**
         * Install the CA, the hosts entries and the redirect.
         *
         * @param caPem       the authority's certificate in PEM
         * @param names       the domains to point at [REDIRECT_ADDRESS]
         * @param redirectPort the proxy's loopback port
         * @return null on success, or a user-facing reason on failure — with the
         *         device already returned to its previous state.
         */
        fun apply(caPem: String, names: List<String>, redirectPort: Int): String? {
            val hash = subjectHash(caPem) ?: return "无法计算 CA 证书的指纹（subject hash）"

            // Undo any leftover from a run that was killed, *before* the first
            // write. This is what makes a retry after a failure additive-free.
            shell.run(undoCommands(), TIMEOUT_MS)

            val caFile = stage("ca.pem", caPem.toByteArray(Charsets.UTF_8))
            val block = stage("hosts.block", hostsBlock(names).toByteArray(Charsets.UTF_8))
            blockPath = block

            val systemCert = "$SYSTEM_CACERTS/$hash.0"
            // Only mirror the APEX store if this build has one; an older device
            // has no APEX and the writable store is the whole answer.
            val hasApex = shell.run(listOf("[ -d $APEX_CACERTS ]"), TIMEOUT_MS).ok

            val commands = buildList {
                add("mkdir -p $STAGE")
                // Back up the hosts file as found. It is the clean base the block
                // is appended to, and the exact file restore puts back.
                add("cp -f $HOSTS $STAGE/hosts.orig")
                // The writable store.
                add("cp -f ${caFile.absolutePath} $systemCert")
                add("chmod 644 $systemCert")
                add("echo -n $systemCert > $STAGE/ca.path")
                // The APEX store, via a mirror we own and bind-mount over it.
                if (hasApex) {
                    add("mkdir -p $STAGE/apex")
                    // Copy the whole store, not `*.0`. The `*.0` glob assumes every
                    // entry ends in `.0`, but this store also ships `d16a5865.1`;
                    // the bind mount hides the real directory, so the mirror must be
                    // a superset of it — the single file a glob misses stops being a
                    // trusted root for the whole device.
                    add("cp -a $APEX_CACERTS/. $STAGE/apex/")
                    add("cp -f ${caFile.absolutePath} $STAGE/apex/$hash.0")
                    // Mark before mounting. If the process dies between the two, the
                    // marker must already exist so recovery still runs `umount`; the
                    // opposite order would leave a mounted-but-unmarked state that
                    // recovery cannot see.
                    add("touch $STAGE/apex.mounted")
                    add("mount --bind $STAGE/apex $APEX_CACERTS")
                }
                // The hosts entries, appended to the backup rather than to the
                // live file, so a second install cannot stack two blocks.
                add("cat $STAGE/hosts.orig ${block.absolutePath} > $HOSTS")
                // The redirect. `-C` first so a rule left by a killed run that
                // recovery missed is not added twice.
                add(
                    "iptables -t nat -C OUTPUT -p tcp -d $REDIRECT_ADDRESS " +
                        "--dport $REDIRECT_PORT -j REDIRECT --to-ports $redirectPort " +
                        "2>/dev/null || iptables -t nat -A OUTPUT -p tcp -d " +
                        "$REDIRECT_ADDRESS --dport $REDIRECT_PORT -j REDIRECT " +
                        "--to-ports $redirectPort",
                )
                add("echo -n $redirectPort > $STAGE/redirect.port")
            }

            val result = shell.run(commands, TIMEOUT_MS)
            if (!result.ok) {
                val failed = result.codes.indexOfFirst { it != 0 }
                restore()
                val step = if (failed >= 0) "第 ${failed + 1} 步" else "su 未返回"
                return "root 模式安装失败（$step）：${result.output.takeLast(200).trim()}"
            }
            return null
        }

        /**
         * Rewrite the hosts block for a new rule set, leaving everything else
         * alone.
         *
         * Called when a rule switch moves while the mode is up. Without it the
         * hosts file keeps pointing the *old* domains at the redirect: a domain
         * switched off would keep resolving to a proxy that then refuses it, and
         * one switched on would never be seen. Returns false when there is no
         * install to refresh.
         */
        fun setNames(names: List<String>): Boolean {
            val block = blockPath ?: return false
            block.writeBytes(hostsBlock(names).toByteArray(Charsets.UTF_8))
            val result = shell.run(
                listOf("cat $STAGE/hosts.orig ${block.absolutePath} > $HOSTS"),
                TIMEOUT_MS,
            )
            return result.ok
        }

        /** Undo the install. Idempotent; safe to call on a lease that never applied. */
        fun restore() {
            shell.run(undoCommands(), TIMEOUT_MS)
            // The `su` side cannot touch the app's private files, so the staging dir
            // is cleared here. It holds a copy of the CA and the intercepted-domain
            // list, and neither should outlive the session on disk.
            runCatching { stageDir(context).deleteRecursively() }
            blockPath = null
        }

        private fun stage(name: String, bytes: ByteArray): File {
            val dir = stageDir(context).apply { mkdirs() }
            val file = File(dir, name)
            file.writeBytes(bytes)
            return file
        }

        /** The block appended to the hosts file: one line per intercepted name. */
        private fun hostsBlock(names: List<String>): String = buildString {
            append(HOSTS_BEGIN).append('\n')
            for (name in names) {
                if (name.isBlank()) continue
                append(REDIRECT_ADDRESS).append(' ').append(name).append('\n')
            }
            append(HOSTS_END).append('\n')
        }
    }

    /**
     * A root shell, driven one command at a time so failures are visible.
     *
     * `su` is given the commands on stdin rather than a `-c` string, because the
     * certificate and the hosts block contain newlines and would have to be
     * escaped — and an escaping bug in a command that rewrites `/etc/hosts` is a
     * bug that bricks the device's DNS. Each command is followed by an `echo` of
     * its own exit status, so [Result.codes] is per command and the caller can
     * say which step failed rather than only that one did.
     *
     * Every invocation forces the **global** mount namespace; without it the shell
     * inherits the app's namespace, where `/` is mounted read-only and none of
     * these writes can land. [run] carries the measurement.
     */
    private class Su {

        class Result(val codes: List<Int>, val output: String) {
            /** True only when every command reported success. */
            val ok: Boolean get() = codes.isNotEmpty() && codes.all { it == 0 }
        }

        /**
         * Run [commands] as root, forcing the **global** mount namespace.
         *
         * **`--mount-master` is load-bearing, and this is measured, not assumed.**
         * `su` inherits the mount namespace of whoever asked for it, and the app's
         * namespace does not look like the shell's. On this device, the same block
         * device is mounted two different ways:
         *
         * ```text
         * adb shell : /dev/block/sda2 / ext4 rw,seclabel,...    <- writable
         * app (10078): /dev/block/sda2 / ext4 ro,seclabel,...   <- READ-ONLY
         * ```
         *
         * So a bare `su` from the app lands in a namespace where `/` is `ro`, and
         * every write this class makes fails — `cp` to
         * `/system/etc/security/cacerts` answers `Read-only file system` and the
         * hosts rewrite answers `can't create /etc/hosts: Read-only file system`.
         * Measured both ways on the same device with MagiskSU's `-t <pid>` (take the
         * namespace from a process) and `-M` (force the global one):
         *
         * ```text
         * su -t <app pid> -c 'cp ca.pem /system/etc/security/cacerts/<h>.0'
         *     -> cp: ...: Read-only file system            rc=1
         * su -M           -c 'cp ca.pem /system/etc/security/cacerts/<h>.0'
         *     -> rc=0, file present
         * ```
         *
         * The global namespace is required for a second reason too, and it is the
         * one that matters for interception: the APEX mirror is bind-mounted over
         * `/apex/com.android.conscrypt/cacerts`, and that mount has to be the one
         * every *other* app sees. Measured that it is: after a global-namespace
         * bind mount, the app's own namespace reads 135 entries (134 + ours) and
         * sees the new file, because the mount propagates. A bind mount made inside
         * the app's private namespace would have been visible to this app alone —
         * the CA would be trusted by nobody.
         *
         * The flag is a MagiskSU/KernelSU option, not a POSIX one, so it is *probed*
         * once instead of assumed — see [option].
         */
        fun run(commands: List<String>, timeoutMs: Long): Result =
            exec(commands, timeoutMs, option)

        /**
         * The mount-namespace flag this device's `su` accepts, decided once.
         *
         * Probing is deliberate, and the first attempt at this got it wrong in a way
         * worth recording rather than quietly fixing. It ran the real [commands]
         * with `--mount-master` and fell back to the plain form when the result
         * carried no status lines. That condition can never be true: [parseCodes]
         * returns one entry per command, `-1` for a line that never arrived, so the
         * fallback was **unreachable** and an `su` that rejected the flag simply
         * failed the install — with a comment next to it claiming the opposite.
         *
         * A dedicated probe cannot have that failure mode. It runs `:` , so there is
         * no command that could be half-applied, and "did the shell run" is answered
         * by one exit status instead of inferred from a failed install. A rejected
         * flag produces no `__rc_` line at all, so the code is `-1` and the plain
         * form is used from then on.
         *
         * Cost is one extra `su` per [Su] **instance**, not per process: the flag is
         * re-probed by each one, and `isAvailable` / `recoverStale` construct their
         * own, so a call that forked once now forks twice. That is deliberate rather
         * than an oversight — hoisting the probe to a process-wide cache would make
         * a probe that merely *timed out* (a consent prompt nobody answered) stick
         * for the life of the process, whereas per-instance it is retried and
         * self-corrects. The extra fork buys nothing but it costs nothing either,
         * and the probe cannot disturb the device: it runs a no-op, in the global
         * namespace, touching nothing.
         */
        private val option: String? by lazy {
            val probe = exec(listOf(PROBE_COMMAND), PROBE_TIMEOUT_MS, MOUNT_MASTER)
            if (probe.codes.firstOrNull() == 0) MOUNT_MASTER else null
        }

        private fun exec(commands: List<String>, timeoutMs: Long, option: String?): Result {
            val process = try {
                val argv = if (option == null) listOf("su") else listOf("su", option)
                ProcessBuilder(argv).redirectErrorStream(true).start()
            } catch (err: Throwable) {
                return Result(emptyList(), "无法启动 su：${err.message}")
            }
            // Drain stdout on a worker thread, not inline. `readText()` returns
            // only at EOF, and EOF only happens once `su` has exited — so reading
            // here would block until the very moment the waitFor() below could
            // have fired, and the timeout would never trigger. A hung `su` (a
            // consent prompt nobody answers, a command that never returns) would
            // then hang the caller forever. Draining off-thread lets the main
            // flow bound the wait, and still keeps every byte that was read.
            val sink = StringBuilder()
            val reader = Thread({
                try {
                    process.inputStream.bufferedReader(Charsets.UTF_8).use { stream ->
                        val buffer = CharArray(4096)
                        while (true) {
                            val read = stream.read(buffer)
                            if (read < 0) break
                            synchronized(sink) { sink.append(buffer, 0, read) }
                        }
                    }
                } catch (_: Throwable) {
                    // Expected when destroyForcibly() closes the stream on a
                    // timeout; whatever already reached `sink` is still returned.
                }
            }, "detour-su-reader")
            reader.isDaemon = true
            reader.start()

            return try {
                process.outputStream.bufferedWriter(Charsets.UTF_8).use { writer ->
                    for ((index, command) in commands.withIndex()) {
                        writer.write(command)
                        writer.write("\n")
                        // `$?` is literal here — Kotlin does not treat `$` before
                        // `?` as a template — and the shell expands it.
                        writer.write("echo \"__rc_${index}=$?\"\n")
                    }
                    writer.write("exit\n")
                }
                if (!process.waitFor(timeoutMs, TimeUnit.MILLISECONDS)) {
                    process.destroyForcibly()
                    // Closing the stream unblocks the reader; join it so the
                    // thread cannot outlive this call and the partial output is
                    // whole before we read it.
                    reader.join(READER_JOIN_MS)
                    val partial = synchronized(sink) { sink.toString() }
                    return Result(
                        emptyList(),
                        "su 超时（${timeoutMs}ms）：${partial.takeLast(200).trim()}",
                    )
                }
                reader.join(READER_JOIN_MS)
                val output = synchronized(sink) { sink.toString() }
                Result(parseCodes(output, commands.size), output)
            } finally {
                runCatching { process.destroy() }
            }
        }

        private fun parseCodes(output: String, expected: Int): List<Int> {
            val codes = IntArray(expected) { -1 }
            for (line in output.lineSequence()) {
                val match = RC_PATTERN.find(line.trim()) ?: continue
                val index = match.groupValues[1].toIntOrNull() ?: continue
                val code = match.groupValues[2].toIntOrNull() ?: continue
                if (index in 0 until expected) codes[index] = code
            }
            return codes.toList()
        }

        private companion object {
            val RC_PATTERN = Regex("""^__rc_(\d+)=(-?\d+)$""")

            /**
             * MagiskSU / KernelSU option that forces the global mount namespace.
             *
             * Without it the shell inherits the app's namespace, where `/` is `ro`
             * and nothing this class writes can land. See [run] for the measurement.
             */
            const val MOUNT_MASTER = "--mount-master"

            /**
             * What the one-off flag probe runs.
             *
             * A no-op, so the probe cannot leave a trace whether or not the flag is
             * accepted, and short enough to be unambiguous in a shell's output.
             */
            const val PROBE_COMMAND = ":"

            /**
             * How long the flag probe may take.
             *
             * Shorter than the install's budget because it runs one no-op: a probe
             * that reaches this bound is an `su` waiting on a consent prompt, and the
             * install it precedes would hit the same wall a moment later.
             */
            const val PROBE_TIMEOUT_MS = 10_000L

            /**
             * How long to wait for the drain thread once `su` has been killed.
             *
             * Normally this returns at once: killing `su` closes its stdout, the
             * reader sees EOF immediately. It is a bound, not a delay — if a
             * grandchild of `su` (the command shell, or a hung command) inherited
             * the pipe and is still holding it open, the reader cannot see EOF
             * yet, so we wait at most this long, harvest whatever was read, and
             * return. The reader is a daemon, so even then it can never keep the
             * process alive. The whole point is that a timeout is bounded by
             * `timeoutMs + READER_JOIN_MS`, never unbounded.
             */
            const val READER_JOIN_MS = 500L
        }
    }
}
