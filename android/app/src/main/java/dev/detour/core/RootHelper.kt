package dev.detour.core

import android.content.Context
import java.io.File
import java.security.MessageDigest
import java.util.concurrent.TimeUnit
import dev.detour.R

/**
 * The privileged half of root mode.
 *
 * Root mode is three things that only work together, and this object owns all
 * three so that none of them can be left half-applied:
 *
 * 1. **The system hosts file.** Intercepted names are pointed at
 *    [REDIRECT_ADDRESS] so the client dials an address we control instead of the
 *    real one. The file itself is never written — it lives on a read-only
 *    partition, so a staged copy is bind-mounted over it (see [Session.apply]).
 *    Measured on this device: the resolver honours `/etc/hosts` ahead
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
     * The two trust stores, and why both are covered.
     *
     * Measured on this device (Android 14): the two are **not** the same
     * directory. `/apex/com.android.conscrypt/cacerts` and
     * `/system/etc/security/cacerts` hold the same 134 certificates as *copies*,
     * with different inodes. Conscrypt on Android 14 reads the APEX store, older
     * releases read the system one, so the certificate has to reach both.
     *
     * **Neither is written to.** Both live on read-only partitions on a real
     * device — a `cp` into `/system/etc/security/cacerts` answers `Read-only file
     * system` even as root — so each is mirrored into a directory we own and the
     * mirror is bind-mounted over it. The same technique covers `/etc/hosts`; see
     * [apply].
     *
     * The bind mount is **global**: measured on this device, a bind mount made
     * from one `su` process was still visible from a separate, non-root `adb
     * shell` after that process had exited. That measurement is the whole reason
     * the mirror technique is sound — it really does cover every process that
     * reads the store, not only the one that mounted it.
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

    /**
     * The SELinux label of the file being replaced, for the staged copy.
     *
     * A file under `/data` is born `shell_data_file`, and a bind mount does not
     * relabel it — the reader's `file:read` check is against the *inode*, so an
     * app asking for `/etc/hosts` would be denied rather than served. Copying
     * the label of the original is what makes the mount transparent.
     *
     * Both measured on the device (`ls -Z`): `/etc/hosts` is `system_file`, and
     * both trust stores are `system_security_cacerts_file`. The emulator runs
     * SELinux `Permissive`, which is why losing the label was never visible
     * there; on an enforcing device it is a denial.
     */
    private const val HOSTS_LABEL = "u:object_r:system_file:s0"
    private const val CACERTS_LABEL = "u:object_r:system_security_cacerts_file:s0"

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

    /**
     * The chain the DNS redirect lives in.
     *
     * Its own chain rather than a rule in OUTPUT, and that is not tidiness: the
     * teardown removes our redirects by *destination* (`purgeRedirect`), and a
     * port-53 redirect matches no particular destination — there is nothing to key
     * a delete on. A chain can be flushed and deleted as a unit, so a session that
     * was killed leaves nothing that a later one has to guess about.
     */
    private const val DNS_CHAIN = "DETOUR_DNS"

    /**
     * The port the DNS redirect captures.
     *
     * UDP only. DNS over TCP is rare, and answering it would mean a second
     * responder speaking a length-prefixed protocol nothing here implements — the
     * cost of getting that wrong is worse than the handful of queries it would
     * carry.
     */
    private const val DNS_PORT = 53

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

    fun isAvailable(context: Context): Boolean {
        if (rootGranted) return true
        val granted = runCatching {
            val result = Su(context).run(listOf("id"), TIMEOUT_MS)
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
        if (!isAvailable(context)) return null
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
        if (!isAvailable(context)) return
        runCatching { Su(context).run(undoCommands(), TIMEOUT_MS) }
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
    fun domainsOf(context: Context, document: ByteArray): List<String> {
        val names = runCatching {
            val groups = org.json.JSONObject(String(document, Charsets.UTF_8))
                .optJSONArray("groups")
                ?: throw IllegalStateException(context.getString(R.string.err_missing_groups_field))
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
                context,
                KernelState.LogEntry.Level.WARN,
                TAG,
                R.string.log_root_parse_failed,
                err.message,
            )
            emptyList()
        }
        if (names.isEmpty()) {
            KernelState.log(context, KernelState.LogEntry.Level.WARN, TAG, R.string.log_root_no_domains)
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
        // 1. Drop the hosts bind mount. This *is* the restore: the file on the
        //    read-only partition was never written, so once the mount is gone the
        //    device is back to exactly the bytes it shipped with. The marker is
        //    only cleared once the mount is confirmed gone, so a failed umount is
        //    retried instead of being forgotten.
        "[ -f $STAGE/hosts.mounted ] && umount $HOSTS",
        "mount | grep -q \" \$(readlink -f $HOSTS) \" || { rm -f $STAGE/hosts.new; rm -f $STAGE/hosts.mounted; }",
        // 1b. Leftovers from a version that wrote the file in place. Both are
        //     no-ops on a read-only partition (the `&&` / the redirect simply
        //     fails), and both matter on a device where that write used to land:
        //     without them the stale block would come back the moment the mount
        //     above is dropped.
        "[ -f $STAGE/hosts.orig ] && cp -f $STAGE/hosts.orig $HOSTS 2>/dev/null",
        "sed -i '/$HOSTS_BEGIN/,/$HOSTS_END/d' $HOSTS 2>/dev/null",
        // 2. Drop the system trust-store mount, then delete the mirror only once
        //    the mount is confirmed gone — deleting a live mount source would
        //    leave the directory bound to a deleted path, and an empty trust store
        //    is a device-wide TLS failure.
        "[ -f $STAGE/cacerts.mounted ] && umount $SYSTEM_CACERTS",
        "mount | grep -q \" $SYSTEM_CACERTS \" || { rm -rf $STAGE/cacerts; rm -f $STAGE/cacerts.mounted; }",
        // 2b. Leftover from a version that copied the certificate into the real
        //     store. The path was recorded, not recomputed, because a recovery run
        //     may not have the certificate in hand.
        "[ -f $STAGE/ca.path ] && rm -f \"\$(cat $STAGE/ca.path)\"",
        // 3. Drop the APEX bind mount (only if we made it), then delete the mirror
        //    only once the mount is confirmed gone. If the umount above failed,
        //    removing the mount source would leave the APEX cacerts directory bound
        //    to a deleted path — an empty trust store, which is a device-wide TLS
        //    failure, not an app-local one. The marker is kept for the next retry.
        "[ -f $STAGE/apex.mounted ] && umount $APEX_CACERTS",
        "mount | grep -q \" $APEX_CACERTS \" || { rm -rf $STAGE/apex; rm -f $STAGE/apex.mounted; }",
        // 4. Remove **every** redirect of ours, not just the one the recorded port
        //    names. A session that was killed rather than stopped — an install
        //    over it, a crash, an emulator restart — leaves its rule behind, and
        //    the next session binds a fresh port, so a delete keyed on the
        //    *current* port never matches it. The old rule then sits **first** in
        //    the chain and wins, sending every connection to a port nothing
        //    listens on. Measured on a device: the chain held
        //    `--to-ports 39347` above `--to-ports 45141`, the mode reported
        //    itself `on`, and every request failed with `connections = 0` — a
        //    tunnel that looks up and carries nothing.
        purgeRedirect(),
        // 4b. Drop the DNS chain, if one is left. Before the markers are forgotten,
        //     so a failure keeps them for the retry.
        "iptables -t nat -D OUTPUT -j $DNS_CHAIN 2>/dev/null",
        "iptables -t nat -F $DNS_CHAIN 2>/dev/null",
        "iptables -t nat -X $DNS_CHAIN 2>/dev/null",
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

    /**
     * Delete every REDIRECT rule of ours from the OUTPUT chain, whatever port it
     * points at.
     *
     * Keyed on the *destination* rather than on the port, because the port is the
     * part that changes between sessions and the stale rule is precisely the one
     * whose port no longer matches anything. `iptables -D` cannot wildcard
     * `--to-ports`, so the ports are read out of `-S` and deleted one by one.
     *
     * Appears in two places on purpose. [undoCommands] uses it so that `restore`
     * and [recoverStale] clean up after a session that was killed; the install
     * command list uses it again immediately before adding its own rule, because
     * the undo runs as a **separate `su` invocation whose result is not checked**
     * — if that invocation lost the mount-namespace probe it wrote nothing, and
     * only purging again in the invocation that installs the rule makes "exactly
     * one rule, pointing at the live port" true by construction.
     *
     * Verified on the device before it was written here: two rules
     * (`39347` above `45141`) → both deleted → one added for `45141`.
     */
    private fun purgeRedirect(): String =
        "for p in \$(iptables -t nat -S OUTPUT | grep -F -- \"-d $REDIRECT_ADDRESS/32\" " +
            "| sed -n 's/.*--to-ports \\([0-9][0-9]*\\).*/\\1/p'); do " +
            "iptables -t nat -D OUTPUT -p tcp -d $REDIRECT_ADDRESS --dport $REDIRECT_PORT " +
            "-j REDIRECT --to-ports \"\$p\"; done"

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

        private val shell = Su(context)

        private var blockPath: File? = null

        /**
         * The names in the block currently installed, or `null` before [apply].
         *
         * Kept so the block can be *rebuilt* without re-parsing the rule document,
         * and so [hostsIntact] knows how many entries to expect. The list is the
         * same one [hostsBlock] was given, so the two cannot disagree.
         */
        private var appliedNames: List<String>? = null

        /**
         * Install the CA, the hosts entries and the redirect.
         *
         * @param caPem       the authority's certificate in PEM
         * @param names       the domains to point at [REDIRECT_ADDRESS]
         * @param redirectPort the proxy's loopback port
         * @param dnsPort      where the DNS responder is listening, or `null` when
         *                     there is none — in which case no redirect is
         *                     installed and DNS keeps working exactly as before.
         * @return null on success, or a user-facing reason on failure — with the
         *         device already returned to its previous state.
         */
        fun apply(
            caPem: String,
            names: List<String>,
            redirectPort: Int,
            dnsPort: Int? = null,
        ): String? {
            val hash = subjectHash(caPem) ?: return context.getString(R.string.err_ca_fingerprint)

            // Undo any leftover from a run that was killed, *before* the first
            // write. This is what makes a retry after a failure additive-free.
            shell.run(undoCommands(), TIMEOUT_MS)

            val caFile = stage("ca.pem", caPem.toByteArray(Charsets.UTF_8))
            val block = stage("hosts.block", hostsBlock(names).toByteArray(Charsets.UTF_8))
            blockPath = block

            // Only mirror the APEX store if this build has one; an older device
            // has no APEX, and the system store — mirrored over the same way —
            // is then the whole answer.
            val hasApex = shell.run(listOf("[ -d $APEX_CACERTS ]"), TIMEOUT_MS).ok

            val commands = buildList {
                add("mkdir -p $STAGE")

                // --- hosts ---------------------------------------------------
                // **Nothing in this class writes the device's hosts file.** `/etc/hosts`
                // resolves to `/system/etc/hosts`, which lives on a read-only,
                // dm-verity protected partition on every real device. `sed -i`
                // needs to create a temp file in that directory, so the write
                // answers `Read-only file system` — and because it was the second
                // command in this list, that is exactly what a "第 2 步" install
                // failure is. It only ever passed on the emulator, where `/` and
                // `/system` are one ext4 that a `remount,rw` can open up.
                //
                // A bind mount needs no write access to its target, so a staged
                // copy is mounted over the real file. The device's file is never
                // opened for writing: a crash cannot corrupt it, and undo is an
                // `umount` rather than a restore.
                add("cp -f $HOSTS $STAGE/hosts.orig")
                // Strip a block of ours in the *staged* copy, not in the live file.
                // It is there because a previous install on a writable root wrote it
                // in place; keeping it would stack a second block the moment the
                // mount hides the original.
                add("sed '/$HOSTS_BEGIN/,/$HOSTS_END/d' $STAGE/hosts.orig > $STAGE/hosts.new")
                add("cat ${block.absolutePath} >> $STAGE/hosts.new")
                // The staged copy is on `/data`, where it is born `shell_data_file`.
                // A bind mount does not relabel it, and a reader's check is against
                // the inode, so without this every app's resolver is denied rather
                // than served. Measured: `ls -Z` on the staged file reports
                // `shell_data_file` until this runs.
                add("chcon $HOSTS_LABEL $STAGE/hosts.new")
                // Mark before mounting: if the process dies between the two, the
                // marker must already exist so recovery still runs `umount`.
                add("touch $STAGE/hosts.mounted")
                // Clear a mount a killed session left behind, so a retry cannot
                // stack a second one on the same target. `|| true` because "not
                // mounted" is the normal case and its status is not a failure.
                add("umount $HOSTS 2>/dev/null || true")
                add("mount --bind $STAGE/hosts.new $HOSTS")

                // --- system trust store --------------------------------------
                // Same partition and the same EROFS, so the same treatment: mirror
                // the directory, add ours to the mirror, bind-mount it over the
                // original. This is the pattern the APEX store below already used;
                // it was simply never applied here, which is why a real device
                // would have failed again at this step even with hosts fixed.
                add("mkdir -p $STAGE/cacerts")
                // The whole store, not `*.0`: it also ships `d16a5865.1`, and a
                // mirror that is not a superset of the directory it hides drops
                // that root for the whole device.
                add("cp -a $SYSTEM_CACERTS/. $STAGE/cacerts/")
                add("cp -f ${caFile.absolutePath} $STAGE/cacerts/$hash.0")
                add("chmod 644 $STAGE/cacerts/$hash.0")
                add("chcon -R $CACERTS_LABEL $STAGE/cacerts")
                add("touch $STAGE/cacerts.mounted")
                add("umount $SYSTEM_CACERTS 2>/dev/null || true")
                add("mount --bind $STAGE/cacerts $SYSTEM_CACERTS")
                // --- APEX trust store ----------------------------------------
                if (hasApex) {
                    add("mkdir -p $STAGE/apex")
                    // Copy the whole store, not `*.0`. The `*.0` glob assumes every
                    // entry ends in `.0`, but this store also ships `d16a5865.1`;
                    // the bind mount hides the real directory, so the mirror must be
                    // a superset of it — the single file a glob misses stops being a
                    // trusted root for the whole device.
                    add("cp -a $APEX_CACERTS/. $STAGE/apex/")
                    add("cp -f ${caFile.absolutePath} $STAGE/apex/$hash.0")
                    // 644, and this one is not a repeat of the /system store's
                    // chmod. The CA file is created by the app, which writes it
                    // 0600 — so `cp -f` lands 0600 here, and a trust anchor the
                    // unprivileged client cannot open is one it will never read
                    // (Conscrypt runs in the caller's uid). `/system` had a chmod
                    // since the beginning; the APEX mirror never did, and it is the
                    // store Android 14 actually reads.
                    add("chmod 644 $STAGE/apex/$hash.0")
                    // `cp -a` does **not** carry the SELinux label across
                    // filesystems here — measured, the mirror came out
                    // `shell_data_file`. Conscrypt reads these as
                    // `system_security_cacerts_file`, and on an enforcing device
                    // that mismatch is a denial, i.e. a CA nobody trusts.
                    add("chcon -R $CACERTS_LABEL $STAGE/apex")
                    // Mark before mounting. If the process dies between the two,
                    // the marker must already exist so recovery still runs `umount`;
                    // the opposite order would leave a mounted-but-unmarked state
                    // that recovery cannot see.
                    add("touch $STAGE/apex.mounted")
                    add("umount $APEX_CACERTS 2>/dev/null || true")
                    add("mount --bind $STAGE/apex $APEX_CACERTS")
                }
                // Purge, then add. The undo above purged too, but its result is not
                // checked and it is a separate `su` invocation — repeating the
                // purge here is what makes "exactly one rule, pointing at the live
                // port" hold even when that undo wrote nothing. A leftover rule
                // from a killed session is not a cosmetic problem: it sits first in
                // the chain and sends every connection to a dead port.
                add(purgeRedirect())
                add(
                    "iptables -t nat -A OUTPUT -p tcp -d $REDIRECT_ADDRESS " +
                        "--dport $REDIRECT_PORT -j REDIRECT --to-ports $redirectPort",
                )
                add("echo -n $redirectPort > $STAGE/redirect.port")

                // The DNS redirect, when a responder is up. Its own chain (see
                // [DNS_CHAIN]), and the exemption is by **source port**, not by uid:
                // the kernel on the device this was built against has no `owner`
                // match, and `iptables` answers "Couldn't find match `owner`" —
                // which inside this batch fails the whole install (measured: the
                // first attempt died at step 17 for exactly that). A packet whose
                // source port is the forwarder's can never come back to it, which
                // is the property that matters; see `DnsRedirect`.
                if (dnsPort != null) {
                    add("iptables -t nat -N $DNS_CHAIN 2>/dev/null || iptables -t nat -F $DNS_CHAIN")
                    add(
                        "iptables -t nat -A $DNS_CHAIN -p udp " +
                            "--sport ${DnsRedirect.FORWARD_PORT} -j RETURN",
                    )
                    add(
                        "iptables -t nat -A $DNS_CHAIN -p udp --dport $DNS_PORT " +
                            "-j REDIRECT --to-ports $dnsPort",
                    )
                    add(
                        "iptables -t nat -C OUTPUT -j $DNS_CHAIN 2>/dev/null || " +
                            "iptables -t nat -A OUTPUT -j $DNS_CHAIN",
                    )
                }
            }

            val result = shell.run(commands, TIMEOUT_MS)
            if (!result.ok) {
                val failed = result.codes.indexOfFirst { it != 0 }
                // A missing status line is not proof that a command failed. The
                // status lines come off a pipe drained on a worker thread, and that
                // drain has a bound; a shell still writing when the bound expires
                // loses its tail. Measured: an install failed at the last command —
                // `echo -n <port> > redirect.port`, which cannot fail — and the very
                // next attempt of the same build succeeded, with the mounts from the
                // "failed" run having been rolled back by [restore].
                //
                // So before believing the codes, check what they are codes *for*:
                // every code that did arrive is 0, and the hosts mount — the first
                // thing this install makes and the one every other step builds on —
                // is visible from this process. A shell that died before the mounts
                // fails that test and is still reported as a failure.
                val allArrivedSucceeded = result.codes.any { it == 0 } && result.codes.none { it > 0 }
                if (allArrivedSucceeded && hostsMountVisible()) {
                    KernelState.log(
                        KernelState.LogEntry.Level.WARN,
                        TAG,
                        context.getString(R.string.log_root_partial_status, result.codes.count { it < 0 }),
                    )
                    appliedNames = names
                    return null
                }
                restore()
                // The command itself is part of the message on purpose: "第 2 步"
                // names a position in a list that moves whenever this function
                // does, and the one time it mattered the report was a step number
                // with no way to tell which write it was. Truncated because one of
                // these commands is a shell loop.
                val step = if (failed >= 0) {
                      context.getString(R.string.err_root_step, failed + 1) +
                          "：" + commands[failed].take(90)
                  } else {
                      context.getString(R.string.err_root_no_su_reply)
                  }
                  return context.getString(
                        R.string.err_root_install_failed,
                        step,
                        result.output.takeLast(200).trim(),
                    )
              }
              // A mount made in a private namespace leaves this mode reporting
              // itself "on" while nothing is intercepted — a failure that looks
              // like success. This process's own mount table is the property that
              // matters: the redirect is only real if the app that reads
              // `/etc/hosts` sees the file mounted over it.
              if (!hostsMountVisible()) {
                  restore()
                  return context.getString(R.string.err_root_mount_private)
              }
              appliedNames = names
              return null
          }

        /**
         * Whether the hosts bind mount is visible from this process.
         *
         * `/proc/self/mountinfo` is readable without root and its fifth field is
         * the mount point. The path compared is the *canonical* one because the
         * kernel records where the mount actually landed: `/etc/hosts` is a
         * symlink to `/system/etc/hosts`, and that is the name in the table.
         *
         * Answers `true` when the file cannot be read at all — an unreadable
         * `mountinfo` is not evidence that the mount is missing, and failing an
         * install on it would turn a diagnostic into a regression.
         */
        private fun hostsMountVisible(): Boolean = runCatching {
            val point = File(HOSTS).canonicalPath
            File("/proc/self/mountinfo").readLines().any { line ->
                line.split(' ').getOrNull(4) == point
            }
        }.getOrDefault(true)

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
            if (result.ok) appliedNames = names
            return result.ok
        }

        /**
         * Whether the block this session wrote is still in `/etc/hosts`, whole.
         *
         * **Readable without root** — the file is world-readable — so this is cheap
         * enough to poll. The *repair* is what needs `su`, which is why detection
         * and repair are two calls.
         *
         * Markers plus a count, not a byte comparison: another app may rewrite the
         * file as long as our block survives, and comparing bytes would then report
         * a clobber that never happened. Two begin markers count as broken too — a
         * block that got duplicated is one the resolver may read in either order.
         *
         * Before [apply] there is nothing to keep, so this answers `true`.
         */
        fun hostsIntact(): Boolean {
            val names = appliedNames ?: return true
            return runCatching {
                val text = File(HOSTS).readText()
                val begin = text.indexOf(HOSTS_BEGIN)
                val end = text.indexOf(HOSTS_END)
                if (begin < 0 || end <= begin) return@runCatching false
                if (text.indexOf(HOSTS_BEGIN, begin + 1) >= 0) return@runCatching false
                text.substring(begin, end)
                    .lineSequence()
                    .count { it.startsWith("$REDIRECT_ADDRESS ") } == names.size
            }.getOrDefault(false)
        }

        /**
         * Put the block back if someone else took it out.
         *
         * This is the answer to the one failure root mode could not report: the
         * hosts file is a shared, world-writable-by-root resource, so a cleanup
         * tool, another proxy app or a ROM "optimiser" can rewrite it while the
         * tunnel is up. The mode then keeps reporting itself connected while
         * hijacking nothing — every domain resolves to its real address and goes
         * straight out. Nothing in the app could tell, because nothing was
         * watching.
         *
         * @return true when the file was rewritten. A `false` with the block still
         *         missing means the repair itself failed, which the caller reports.
         */
        fun reapplyHostsIfClobbered(): Boolean {
            if (hostsIntact()) return false
            val names = appliedNames ?: return false
            return setNames(names)
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
     * escaped — and an escaping bug in a command that ends up mounted over
     * `/etc/hosts` is a bug that bricks the device's DNS. Each command is followed
     * by an `echo` of its own exit status, so [Result.codes] is per command and the
     * caller can say which step failed rather than only that one did.
     *
     * Every invocation forces the **global** mount namespace; without it the shell
     * inherits the app's namespace and the mounts land where only this app can see
     * them, which is the same as not making them. [run] carries the measurement.
     */
    private class Su(private val context: Context) {

        class Result(val codes: List<Int>, val output: String) {
            /** True only when every command reported success. */
            val ok: Boolean get() = codes.isNotEmpty() && codes.all { it == 0 }
        }

        /**
         * Run [commands] as root, forcing the **global** mount namespace.
         *
         * **`--mount-master` is load-bearing, and this is measured, not assumed.**
         * `su` inherits the mount namespace of whoever asked for it, and the app's
         * namespace does not look like the shell's. On this device the same block
         * device has been seen mounted two different ways — `rw` for `adb shell`
         * and `ro` for the app — which is what made a bare `su` useless:
         *
         * ```text
         * adb shell : /dev/block/sda2 / ext4 rw,seclabel,...    <- writable
         * app (10078): /dev/block/sda2 / ext4 ro,seclabel,...   <- READ-ONLY
         * ```
         *
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
         * **That `rc=0` is an emulator fact and must not be generalised.** It held
         * because `/` and `/system` were one writable ext4 there; on a real device
         * `/system` is a separate, dm-verity protected partition, so the same `cp`
         * fails no matter which namespace it runs in. The install no longer writes
         * anything into `/system` at all — it bind-mounts staged copies over the
         * files (see [apply]) — which is why the namespace is now required for a
         * different reason: a mount is only useful if it is the mount every other
         * process sees.
         *
         * Measured that it is: after a global-namespace bind mount, the app's own
         * namespace reads 135 entries (134 + ours) and sees the new file, because
         * the mount propagates. A bind mount made inside the app's private
         * namespace would have been visible to this app alone — the CA would be
         * trusted by nobody, and the hosts redirect would intercept nothing.
         * [apply] asserts that propagation happened rather than assuming it.
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
                return Result(emptyList(), context.getString(R.string.err_su_start_failed, err.message))
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
                        context.getString(R.string.err_su_timeout, timeoutMs, partial.takeLast(200).trim()),
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
             *
             * **Measured: 500 ms was not enough.** One install in three reported
             * "step 26 failed" on a command that cannot fail (`echo -n <port> >
             * redirect.port`) while every line before it had arrived and the mounts
             * were in place — the tail was still in the pipe when the snapshot was
             * taken. The same build succeeded on the next attempt. Three seconds
             * is the same bound with enough room for a shell that outlives `su`.
             */
            const val READER_JOIN_MS = 3_000L
        }
    }
}
