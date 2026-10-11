package dev.detour.core

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.util.Log
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import org.json.JSONObject
import dev.detour.R

/**
 * Where the rule document comes from.
 *
 * ## The default source, and why it is a single document
 *
 * `github-hosts` (`maxiaof/github-hosts`) is a plain hosts file: 37 domains, and
 * no `# === [x] ===` section markers. (Its size is deliberately not written here
 * — it tracks the upstream document, which changes, and a stale byte count in a
 * comment is worse than no count.) `watt-rules` gives a
 * section-less hosts document **one** group, named `hosts` (`hosts.rs:165-169`),
 * so this source parses to a single group of 37 domains / 37 addresses. That is
 * the shape the rules screen has to be able to drill into — a group with nothing
 * under it is not a rendering bug, it is a document with one level.
 *
 * The source is a [RuleSource] now rather than a constant here, because the user
 * can add their own. This object fetches whichever one is selected.
 *
 * ## The retired second source, and why it was never merged in
 *
 * The previous pair of endpoints were two *different rule sets*, not two formats
 * of one: `/1` was UsbEAm host records (5230 domains) and `/2` was a
 * Steamcommunity-302 hijack block — 862 entries, every address `127.0.0.1`, and
 * **675 domains that `/1` did not have at all** (`0.gravatar.com`,
 * `0.downloader.disk.yandex.com`, …).
 *
 * Merging `/2` would not "add 675 domains": it would take 675 domains that
 * matched no rule and went **direct**, and turn them into RSTs, because
 * `Planner::can_relay` (`watt-stack/src/planner.rs`) refuses a loopback target
 * and `tcp.rs` then aborts the flow:
 *
 * ```text
 * if !planner.can_relay(&decision) { socket.abort(); stats.tcp_flows_rejected += 1; }
 * ```
 *
 * The identical `127.0.0.1 domain` line is *correct* in the upstream aggregator's
 * output, where it is a hosts file for a machine running S302's own local Caddy
 * reverse proxy — something really is listening there. watt does not MITM, so nothing
 * ever will, and the same line can only mean "refuse". Same data, opposite
 * meaning; the difference is whether a local proxy exists, not the endpoint.
 *
 * `/2` is therefore **retired** rather than offered: it was meaningful only next
 * to software this app does not ship. No source sets [RuleSource.unavailable] any
 * more, so "retired" here means *deleted*, not *hidden*. The document is still
 * fetched — by root mode, and in a different shape: `/3` carries the same rule set
 * with a **substitute upstream name** where `/2` has `127.0.0.1`, which is an
 * address a re-originating proxy can dial and a name it can be asked for. See
 * [RuleSource.rootSource] for that document and its measured coverage.
 *
 * ## The merge
 *
 * The merge itself is **not** implemented here. `watt_rules::merge_documents` in
 * the kernel does it, and the app calls it through the C ABI. That is deliberate:
 * the merge is per *domain*, not per entry, because the compiler keeps only the
 * first entry that claims a domain — keying on the entry would silently discard
 * the addresses the merge exists to keep. The Rust version has tests; a second
 * implementation in Kotlin would not.
 *
 * It is still used, for converting a hosts document to the kernel's own JSON —
 * see [toKernelDocument].
 *
 * ## Why the documents are fetched as hosts text
 *
 * They are **not** handed to the kernel raw: the kernel only eats its own JSON,
 * so each hosts document is converted with [toKernelDocument] before it reaches a
 * tunnel or the cache.
 *
 * The tempting move is to take `?format=json` and let the kernel's JSON path
 * handle it. That does not work, and it fails *silently*, which is why this
 * comment exists. The endpoint's `?format=json` is a different schema from the
 * document the kernel parses:
 *
 * ```text
 * endpoint  { "entries": [ { "ip": "127.0.0.1", "domain": "…", "comment": "S302" } ] }
 * watt-*    { "groups":  [ { "entries": [ { "domains": ["…"], "ips": ["…"] } ] } ] }
 * ```
 *
 * Every field on `RuleDocument` is `#[serde(default)]` so a new upstream key can
 * never break parsing — the flip side being that a whole-document shape mismatch
 * is accepted as `Ok(document with zero entries)`. Measured through this ABI: 862
 * records in, **0 entries out**, no error. The tunnel would then come up relaying
 * nothing. (The daemon does *not* share this failure mode: it compiles through
 * `RuleSet::from_document`, which rejects an empty set with `NoUsableEntries` and
 * fails loudly. Two layers, two behaviours.)
 *
 * `hosts` text has no such failure mode: the parser reads it natively, and it is
 * the shape the upstream files themselves contain.
 *
 * ## A source can also be a file on this device
 *
 * A [RuleSource] with a [RuleSource.localFile] is read from
 * `filesDir/imported-rules/` and never from the network, so [fetch] branches on
 * it before it opens a connection and [load] treats it as never stale — there is
 * no server whose copy could move on. Everything after that branch is the same
 * for both kinds: the same [parseDocument] gates, the same cache, the same
 * stamp. That is deliberate; a second read path with its own validation is how
 * the "an HTML page parses to one fabricated entry" hole would come back.
 *
 * The copy is named after the SHA-256 of its content ([importLocalBytes]), which
 * is what makes the cache stamp self-correcting: see [RuleSource.identity]. A
 * re-import of edited content lands under a *different* name, so the stamp
 * changes and the cache is a miss, without anything having to remember to
 * invalidate it.
 *
 * ## The cache carries the identity of what it holds
 *
 * `rules.json` is the upstream document, kept so a tunnel can start without a
 * network. Until this round it was validated **only by age** — `lastModified()`
 * against `refreshHours` — and never by content, which is a bug that shipped:
 *
 * ```text
 * prefs            rule_source = "hosts"
 * rules.json       197,584 B, 862 entries, every ips == ["127.0.0.1"]
 * ```
 *
 * That document is `/2` (s302), left behind by an earlier build and inherited by
 * the freshly installed one. `refresh_hours` was 6, the cache was two hours old,
 * so it was reused unexamined and the tunnel relayed nothing at all —
 * `flows_matched_rules = 0`, `flows_direct = 41` — while the UI reported
 * `rule_source: "hosts"` and looked correct. Every address being loopback is
 * what makes it silent: `Planner::can_relay` refuses those targets, so the flows
 * fall back to direct and the browser shows `ERR_CONNECTION_REFUSED` for a domain
 * that *is* in the rule set.
 *
 * The fix is that the cache now records **which document it holds**, in a sidecar
 * (`rules.json.source`) written at the same moment as the document, and a read
 * requires that identity to match what would be fetched now. A missing or
 * mismatched sidecar is a miss and refetches. Two consequences worth stating:
 *
 * * The stamp is a *sidecar* and not a field inside `rules.json`. That document
 *   is the kernel's own schema, parsed by `watt-rules`, whose every field is
 *   `#[serde(default)]` so an unknown key would be ignored rather than rejected —
 *   meaning folding the identity in would work until the day `watt-rules` decided
 *   to own that name, and would leave the file no longer being a valid kernel
 *   document for any other reader. A separate file cannot conflict with the
 *   schema because it is not part of it.
 * * The stamp is the source's **URL** ([RuleSource.identity]), not its id. An id
 *   is a name the user can delete and re-add; two different URLs must never share
 *   a stamp. The URL is the document itself, so it is the honest identity, and it
 *   survives the renames and retirements an id does not.
 *
 * Age is still checked. Identity answers "is this the right document", age
 * answers "is it recent enough"; a cache that is correct but a week old is still
 * a cache miss, and one that is fresh but for a different source is too.
 */
object RulesRepository {

    private const val TAG = "DetourRules"

    /**
     * The fewest entries a built-in document may parse to and still be accepted.
     *
     * Not an arbitrary round number — see [requireUsable] for the measurements it
     * sits between. A built-in's size is known, so a floor can be enforced on it;
     * a user-added source's is not.
     */
    private const val BUILTIN_MIN_ENTRIES = 10

    private const val CACHE_NAME = "rules.json"

    /**
     * Where imported copies live, under the app's private `filesDir`.
     *
     * Private rather than on shared storage: the whole point of copying the file
     * is that the source stops depending on a URI grant, and a world-readable
     * copy would be a second thing to keep in step. It also means no storage
     * permission is needed at any point — the picker hands over a stream, and the
     * app owns everything after that.
     */
    private const val IMPORTED_DIR = "imported-rules"

    /** The suffix given to an imported copy, so a listing says what these are. */
    private const val IMPORTED_SUFFIX = ".rules"

    /**
     * The largest imported file accepted, in bytes.
     *
     * The file is read into memory to be hashed and parsed, and the picker will
     * happily hand over a video. The real documents are tens of kilobytes
     * (`raw.hellogithub.com` measured at ~4 KiB, `maxiaof/github-hosts` similar),
     * so 8 MiB is three orders of magnitude of headroom for a legitimate list
     * while still refusing to allocate a gigabyte for a mistaken tap.
     */
    const val MAX_LOCAL_BYTES = 8 * 1024 * 1024

    /**
     * Records which rule source produced [CACHE_NAME].
     *
     * A sidecar rather than a field in the document — see the class comment for
     * why the kernel's schema must not gain a key. Written only after the document
     * write has succeeded, so a crash mid-fetch cannot leave a stamp vouching for
     * a document that is not there; read on every cache hit, so the pair is either
     * "document + correct stamp" or treated as absent.
     */
    private const val SOURCE_NAME = "rules.json.source"

    /**
     * A valid, empty kernel rule document.
     *
     * Used as the seed for hosts→document conversion: the kernel's merge parses
     * its **first** argument as JSON, so the hosts text has to arrive as the
     * second. `RuleDocument` defaults every field, so `{}` deserializes cleanly
     * and contributes no groups of its own.
     */
    private val EMPTY_DOCUMENT = "{}".toByteArray()

    /**
     * The document to hand the kernel.
     *
     * [source] is the source to load, or `null` for "whichever one is in force for
     * the mode the app is in" — the selection, except in root mode, where it is the
     * pinned source (see [RuleSource.forMode]).
     *
     * **The default is derived here rather than left to each caller**, because the
     * callers that do not name a source are not a minority: the rules screen's
     * index, the about screen's counts, the control channel, and — the one that
     * matters — [DetourVpnService.reloadRulesIfRunning], which re-reads the
     * document and hands it to the running engine. If that one resolved the
     * selection instead, a reload while root mode was up would fetch the *other*
     * source and rewrite the hosts block to that document's domains, mid-session.
     * A default that is wrong in one mode is a trap; a default that asks the mode
     * is not.
     *
     * The service's start path still passes its source explicitly, from the mode it
     * was told to start. That value is the same one this default would compute, but
     * naming it does not depend on the status having been updated first.
     *
     * Everything downstream that asks "which source" takes this value as an
     * argument instead of reading [Prefs] itself, so the stamp, the fetch and the
     * offline branch cannot come to three different answers.
     */
    @Synchronized
    fun load(
        context: Context,
        forceRefresh: Boolean = false,
        source: RuleSource? = null,
    ): ByteArray {
        val prefs = Prefs.of(context)
        val source = source
            ?: RuleSource.forMode(KernelState.status.value.mode, prefs.selectedSource)
        val cached = cacheFile(context)

        // A cache left by a build that wrote no identity stamp cannot be validated,
        // so the migration flags it and this is where the flag is honoured. Checked
        // *before* the cache is examined so the delete happens on the same load
        // that would otherwise have reused it, which is the whole point of the
        // upgrade repair being a flag rather than a silent wait for `refreshHours`.
        if (prefs.consumeRulesCacheDirty()) {
            discardCache(context)
            KernelState.log(
                context,
                KernelState.LogEntry.Level.INFO,
                TAG,
                R.string.log_cache_from_older_version,
            )
        }

        // "Usable" is now two facts, not one: a document has to be present **and**
        // the sidecar has to say it is the document that would be fetched now. A
        // document whose stamp is missing is treated exactly like no document at
        // all — that is the case the device hit, where a stale `/2` document sat
        // next to a `hosts` preference and was served as if it were current.
        //
        // The stamp is computed from the source *this* load will use, so switching
        // between root mode and the others is a stamp change and therefore a cache
        // miss: the two modes hold different documents, one file cannot be both, and
        // the cost is one fetch per mode switch. Paying it is what keeps the rule
        // the class comment describes — a document is only served for the source
        // whose stamp it carries — true for a source that is not the selected one.
        val stamp = sourceStamp(source)
        val cachedSource = readStamp(context)
        val identityMatches = cachedSource == stamp
        val hasCache = cached.isFile && cached.length() > 0 && identityMatches

        // Two settings decide whether the network is touched at all, and they are
        // not the same decision. `offline` is absolute — "use what is already
        // downloaded", no fetch however stale the cache is. `refreshHours` is the
        // cache's lifetime: before it elapses the download is reused, after it the
        // document is fetched again. Without the age check the cache was reused
        // forever, so "刷新间隔" was a number that changed nothing.
        //
        // Neither applies to a local source, which is read from this device rather
        // than downloaded: `offline` has nothing to forbid, and there is no
        // upstream copy whose age could matter — the content cannot change without
        // the source being re-imported, which lands it under a different name and
        // therefore a different stamp. Both checks are skipped here rather than
        // special-cased inside the offline branch, which is what keeps that branch
        // able to refuse honestly when it really has nothing to serve.
        //
        // Read from `source`, not from the selection: in root mode those differ, and
        // the answer has to describe the document this call is about to return.
        val localSource = source.isLocal

        val ageMillis = if (hasCache) {
            (System.currentTimeMillis() - cached.lastModified()).coerceAtLeast(0L)
        } else {
            Long.MAX_VALUE
        }
        val maxAgeMillis = prefs.refreshHours.coerceIn(1, 72) * 3_600_000L
        val stale = !localSource && ageMillis >= maxAgeMillis

        val merged = when {
            prefs.offline && !localSource -> {
                // Offline cannot fetch, so the file on disk is the only candidate.
                // Whether it may be *used* turns on how much is known about it,
                // and there are three cases, not two.
                if (!cached.isFile || cached.length() == 0L) {
                    // Refusing is the honest answer: the user asked for no network
                    // and there is nothing downloaded to fall back on. Returning
                    // an empty document would start a tunnel that relays nothing,
                    // which looks like a working tunnel that drops every packet.
                    throw IllegalStateException(context.getString(R.string.err_offline_no_cache))
                }
                if (cachedSource != null && cachedSource != stamp) {
                    // **A known mismatch is refused, not served.** The document's
                    // own stamp says it came from a different rule set than the one
                    // selected now — so it is known *not* to be the document that
                    // was asked for. Serving it anyway produces exactly the state
                    // the branch above rejects an empty document for: a tunnel that
                    // comes up green and relays nothing, because the stale document
                    // is `/2` whose every address is loopback and which the planner
                    // refuses. Measured on the device: prefs `hosts`, cache stamped
                    // `s302`, and the log shows the WARN followed by
                    // "使用已下载的规则" and a running tunnel with
                    // `flows_matched_rules = 0`.
                    //
                    // The old comment here argued the opposite — that a wrong
                    // document beats no tunnel. That is true of a document we merely
                    // *cannot identify*; it is false of one we can positively
                    // identify as wrong. Refusing is also consistent with the empty
                    // case, and inconsistency between two branches that produce the
                    // same observable symptom is itself the bug.
                    //
                    // The WARN is not enough on its own: `HomeScreen` only surfaces
                    // `status.message` when `phase == ERROR`, and this path leaves
                    // `phase` at `ON`, so nobody would ever see it.
                    KernelState.log(
                        context,
                        KernelState.LogEntry.Level.ERROR,
                        TAG,
                        R.string.log_offline_stamp_mismatch,
                        cachedSource,
                        stamp,
                    )
                    throw IllegalStateException(
                        context.getString(R.string.err_offline_source_mismatch, cachedSource, stamp),
                    )
                }
                if (!identityMatches) {
                    // `cachedSource` is null: the document predates the identity
                    // stamp, or the stamp is unreadable. Nothing is *known* to be
                    // wrong — it is merely unverifiable — and with no network there
                    // is no way to obtain a better candidate. A tunnel that might
                    // work is the better bet here, which is the case the old comment
                    // was actually about.
                    KernelState.log(
                        context,
                        KernelState.LogEntry.Level.WARN,
                        TAG,
                        R.string.log_offline_no_stamp,
                    )
                }
                KernelState.log(
                    context,
                    KernelState.LogEntry.Level.INFO,
                    TAG,
                    R.string.log_offline_using_cache,
                )
                cached.readBytes()
            }
            forceRefresh || stale || !identityMatches -> fetchInto(context, cached, source)
            else -> cached.readBytes()
        }
        return applySwitches(context, merged)
    }

    /**
     * The identity to stamp for [source].
     *
     * Total, and that is a consequence of the parameter rather than a choice: the
     * value used to be read from a live [Prefs] inside a `runCatching`, which meant
     * a `null` here stood for "the settings store could not be read". It cannot —
     * [load] has already called `Prefs.of` by the time this runs, and that call
     * caches its instance, so a second one cannot fail. A `null` return would
     * therefore have been a branch no input could reach, and a comment claiming a
     * failure mode that does not exist is worse than no comment.
     */
    private fun sourceStamp(source: RuleSource): String = source.identity

    /** Which source the cached document says it came from, or `null` if unstamped. */
    private fun readStamp(context: Context): String? =
        runCatching {
            val file = sourceFile(context)
            if (file.isFile) file.readText().trim().ifEmpty { null } else null
        }.getOrNull()

    /**
     * Fetch [source] and write it to the cache.
     *
     * On failure the cache is the fallback — a tunnel that cannot start without a
     * network is a tunnel that cannot start on a plane — and only when there is no
     * cache at all is the failure real. "The cache" means one whose stamp is
     * [source]'s: a failure while root mode is starting must not hand the proxy a
     * document fetched for some other source.
     *
     * # Write order
     *
     * The document goes first and the identity stamp second, and the order is not
     * cosmetic. A stamp written first would, after a crash between the two writes,
     * vouch for the *previous* document — declaring a `/2` document to be `hosts`,
     * which is precisely the pairing the stamp exists to detect. Document-then-
     * stamp fails the other way: a bare document with no stamp reads as a miss and
     * is refetched. A wasted fetch is the cheap side of this trade.
     */
    private fun fetchInto(context: Context, cached: File, source: RuleSource): ByteArray {
        return try {
            val fetched = fetch(context, source)
            cached.writeBytes(fetched)
            writeStamp(context, source)
            KernelState.log(
                context,
                KernelState.LogEntry.Level.INFO,
                TAG,
                R.string.log_rules_updated,
                fetched.size / 1024,
                sourceStamp(source),
            )
            fetched
        } catch (err: Throwable) {
            Log.w(TAG, "fetch failed", err)
            KernelState.log(
                context,
                KernelState.LogEntry.Level.WARN,
                TAG,
                R.string.log_fetch_failed,
                err.message,
            )
            val identityMatches = sourceStamp(source) == readStamp(context)
            // The fallback only exists for a cache that still describes a document
            // this build would accept. Serving one whose stamp does not match would
            // re-create the bug this round fixes — a fetch failure would hand the
            // tunnel the very document the stamp had just rejected — so a mismatched
            // cache is re-thrown as a failure instead of quietly used.
            if (cached.isFile && cached.length() > 0 && identityMatches) {
                KernelState.log(context, KernelState.LogEntry.Level.INFO, TAG, R.string.log_using_cache)
                cached.readBytes()
            } else {
                if (cached.isFile && cached.length() > 0) {
                    KernelState.log(
                        context,
                        KernelState.LogEntry.Level.WARN,
                        TAG,
                        R.string.log_cache_mismatch_and_fetch_failed,
                    )
                }
                throw err
            }
        }
    }

    /**
     * Record which document the cache now holds.
     *
     * Written **after** the document, deliberately — see [fetchInto]. Bumped to
     * the same `lastModified` as the document by writing it second, which also
     * means the one age check in [load] covers both files.
     *
     * The write can still fail (a full disk), and that is the failure [fetchInto]
     * is built around: the document is on disk and the stamp is not, so the next
     * load sees a miss and refetches.
     */
    private fun writeStamp(context: Context, source: RuleSource) {
        runCatching { sourceFile(context).writeText(sourceStamp(source)) }
    }

    /**
     * Delete the cached document **and** its stamp, always together.
     *
     * They are one fact stored in two files, so removing only one leaves the other
     * claiming a provenance that does not exist. Every delete path goes through
     * here for that reason.
     */
    private fun discardCache(context: Context) {
        runCatching { cacheFile(context).delete() }
        runCatching { sourceFile(context).delete() }
    }

    /**
     * Remove whatever the user switched off.
     *
     * Applied on every load rather than baked into the cache: the cache holds the
     * upstream document, which is the thing worth keeping. A filtered copy on disk
     * would go stale the moment a switch moved, and the user would see a rule come
     * back to life after a cache hit.
     */
    private fun applySwitches(context: Context, document: ByteArray): ByteArray {
        val disabled = Prefs.of(context).disabledRules
        if (disabled.isEmpty()) return document
        return try {
            val filtered = Kernel.filter(document, disabled)
            KernelState.log(
                context,
                KernelState.LogEntry.Level.INFO,
                TAG,
                R.string.log_rules_filtered,
                disabled.size,
                document.size / 1024,
                filtered.size / 1024,
            )
            filtered
        } catch (err: Throwable) {
            // Falling back to the unfiltered document is wrong in a specific way:
            // the user switched something off and it stays on. Saying so is the
            // only honest option, and it is still better than refusing to start.
            KernelState.log(
                context,
                KernelState.LogEntry.Level.ERROR,
                TAG,
                R.string.log_filter_failed,
                err.message,
            )
            document
        }
    }

    /**
     * Fetch [source] and return a kernel document that is known to hold at least
     * one rule.
     *
     * The gates are not decoration, and they catch different things: `fetchText`
     * refuses a non-200 ("the server said it failed"), [looksLikeHtml] refuses a
     * 200 that is a web page ("the server said it succeeded but did not send a
     * document"), and [parseDocument] refuses a document that parsed to too few
     * entries. None of them is redundant — a non-200 is not necessarily HTML, a
     * 200 HTML page is not zero entries (measured: 1), and the kernel accepts a
     * wrong-shaped document silently. See each for the measurement.
     *
     * Every URL in [RuleSource.fetchUrls] is tried in order and the first one
     * that clears every gate wins. Endpoints die — eight of the eleven measured
     * candidates for the default document were already gone — so a single URL is
     * a single point of failure the user cannot see or fix.
     *
     * A local source never reaches the loop: see [readLocal].
     */
    private fun fetch(context: Context, source: RuleSource): ByteArray {
        if (!source.usable) {
            // Reachable only if something selected a source whose `usable` later
            // became false — the UI will not offer one. Refusing loudly beats
            // opening a connection to an empty URL.
            throw IllegalStateException(context.getString(R.string.err_source_unavailable, source.id))
        }

        // Branched on before the loop rather than inside it: a local source has no
        // URLs to iterate, and the mirror-and-fallback machinery below exists only
        // because a network endpoint can be dead. A file on this device either
        // exists or has been deleted, and there is nothing to try instead.
        source.localFile?.let { return readLocal(context, source, it) }

        val urls = source.fetchUrls
        val failures = mutableListOf<String>()
        for ((index, url) in urls.withIndex()) {
            // The index is in the log because a fallback that succeeds on the
            // third try and one that succeeds on the first look identical in a
            // log otherwise, and "which endpoint is actually carrying this
            // install" is the question the mirrors exist to make answerable.
            KernelState.log(
                context,
                KernelState.LogEntry.Level.INFO,
                TAG,
                R.string.log_fetching,
                source.id,
                index + 1,
                urls.size,
                url,
            )
            try {
                val text = fetchText(url)
                return parseDocument(context, text, builtin = source.builtin, origin = url)
            } catch (err: Throwable) {
                failures += "$url → ${err.message}"
                KernelState.log(
                    context,
                    KernelState.LogEntry.Level.WARN,
                    TAG,
                    R.string.log_candidate_failed,
                    index + 1,
                    urls.size,
                    url,
                    err.message,
                )
            }
        }

        // Every address is reported, not just the last failure. The last error
        // alone cannot answer the only question worth asking here — "why did all
        // of them fail" — and it hides whether one endpoint returned HTML while
        // the rest were unreachable.
        throw IllegalStateException(
            context.getString(
                R.string.err_all_candidates_failed, source.id, urls.size, failures.joinToString("; "),
            ),
        )
    }

    /**
     * Read a source whose document is a file on this device.
     *
     * A missing file is an error and not a fallback to anything: the copy is
     * named after its own content and lives in the app's private directory, so
     * the only things that can remove it are this app deleting it (which also
     * removes the source — see [pruneImported]) or the user clearing the app's
     * data, which removes the preference naming it too. If it is gone anyway, the
     * honest answer is "import it again", and the message says so. Serving a
     * different document instead would be the same class of bug the identity
     * stamp exists to prevent.
     */
    private fun readLocal(context: Context, source: RuleSource, fileName: String): ByteArray {
        val file = importedFile(context, fileName)
        if (!file.isFile || file.length() == 0L) {
            throw IllegalStateException(
                context.getString(R.string.err_local_file_missing, source.label.ifBlank { fileName }),
            )
        }
        KernelState.log(
            context,
            KernelState.LogEntry.Level.INFO,
            TAG,
            R.string.log_reading_local,
            source.label.ifBlank { fileName },
            file.length() / 1024,
        )
        return parseDocument(
            context,
            String(file.readBytes(), Charsets.UTF_8),
            builtin = false,
            origin = source.label.ifBlank { fileName },
        )
    }

    /**
     * Whether a fetched body is an HTML page rather than a rule document.
     *
     * A status check is not enough, and this is measured, not hypothetical:
     * `gitcode.com` answers **HTTP 200 with a rendered HTML page** for a plain
     * text path that does not exist. The status check passes — so the only
     * question is whether the *entry count* catches it, and it does **not**.
     * Feeding the real GitCode error page through the real Rust parse chain
     * (verbatim `watt_merge_documents`, the chain behind `Kernel.merge`) yields
     * **entryCount = 1**, not 0, because the page contains the line
     *
     * ```text
     * hm.src = "https://hm.baidu.com/hm.js?62047c952451105d57bab2c4af9ce85b";
     * ```
     *
     * `hm.src` has a dot and passes `normalize_dial_name`'s character class as
     * the address, and `=` is accepted as the **domain** — `hosts.rs:116-120`
     * applies the character-class gate only to the *first* column and barely
     * checks the domain column at all. So an HTML error page can conjure a
     * handful of fabricated entries and sail through a "more than zero" check.
     * (Control: the same path counts **40** entries for `raw.hellogithub.com`,
     * and **2** for a hand-built file of two valid dotted names — the harness is
     * sound; it is the HTML that produces junk, not zero.)
     *
     * That is why this marker check is the **only** thing that recognises "this
     * is not a rule document", not a redundant second gate. The direction of the
     * structural argument still holds — an error page's own tokens (`<html>`,
     * `<!DOCTYPE`, `href="…"`) carry angle brackets, quotes and colons the
     * character class rejects, and a single-token line has no second field — but
     * it is not *sufficient*: a token like `hm.src = "…"` splits into two
     * plausible fields, so relying on the parser to reject HTML would leave the
     * page's junk entries in the document.
     *
     * The test is a plain case-insensitive `contains`, not a regex and not a
     * parse: the question is only "is this obviously *not* a rule document", and
     * a real parser here would be more code to be wrong with.
     */
    private fun looksLikeHtml(text: String): Boolean {
        val head = text.take(2048).lowercase()
        return "<html" in head || "<!doctype" in head || "<body" in head
    }

    /** Whether a fetched body is already a kernel rule document. */
    private fun looksLikeKernelJson(text: String): Boolean =
        runCatching { JSONObject(text).optJSONArray("groups") != null }.getOrDefault(false)

    /**
     * Turn fetched or imported text into a kernel document that would relay
     * something.
     *
     * One function for both kinds of source on purpose. The gates below are the
     * only thing standing between a web page and a tunnel that comes up green and
     * forwards nothing, and a second copy of them on the local path is exactly how
     * one of them would end up missing.
     *
     * Every field on the kernel's `RuleDocument` is `#[serde(default)]`, so a
     * whole-document shape mismatch is accepted as `Ok(document with zero
     * entries)` rather than an error — measured through this ABI at 862 records
     * in, 0 entries out, no error. A tunnel started on that document comes up
     * green and forwards nothing, which is the worst possible failure: it looks
     * like the rules are running and every listed site is broken. Counting the
     * entries is the only place that can catch it, so it happens here, before the
     * document is cached or handed to a tunnel.
     *
     * "More than zero" is not enough for a built-in, and the threshold is 10
     * because of what was measured, not because it is a round number:
     *
     * * legitimate documents — `maxiaof/github-hosts` **37** entries,
     *   `raw.hellogithub.com` **40**;
     * * garbage that parses anyway — the GitCode HTML error page **1** entry
     *   (from `hm.src = "https://…"`, see [looksLikeHtml]), a hand-built file of
     *   two valid dotted names **2**.
     *
     * 10 sits an order of magnitude above the garbage and roughly four times
     * below the legitimate values, so it has margin on both sides: it cannot be
     * reached by a stray line, and it will not trip on a real document that
     * shrinks a little upstream.
     *
     * The floor applies **only to built-ins**. Their size is something we have
     * measured, so a floor is safe; a user-added source's size is not — an
     * imported list of one host is a legitimate thing to import, and a floor here
     * would refuse it.
     *
     * [origin] names what was read: the address that was actually tried, or the
     * imported file's name. The address is the one that answered, not the primary
     * one — with mirrors in play the primary URL is often *not* what was fetched,
     * and an error naming an address that was never contacted sends the reader to
     * the wrong endpoint.
     */
    private fun parseDocument(
        context: Context,
        text: String,
        builtin: Boolean,
        origin: String,
    ): ByteArray {
        // A 200 can still not be a rule document — see [looksLikeHtml] — and this
        // check runs before the shape sniff so an HTML page is never fed to either
        // parser.
        if (looksLikeHtml(text)) {
            throw IOException(context.getString(R.string.err_not_a_rule_document))
        }
        // A source may publish the kernel's own JSON rather than hosts text, so
        // the shape is sniffed before the hosts conversion is applied. Converting
        // an already-JSON document would be a parse of the wrong grammar, and the
        // merge would return zero entries rather than fail.
        val document =
            if (looksLikeKernelJson(text)) text.toByteArray() else toKernelDocument(text)
        val count = entryCount(document)
        val floor = if (builtin) BUILTIN_MIN_ENTRIES else 1
        if (count < floor) {
            throw IllegalStateException(
                context.getString(R.string.err_too_few_entries, origin, count, floor),
            )
        }
        return document
    }

    /** The number of `entries` across every group, or 0 if the shape is wrong. */
    private fun entryCount(document: ByteArray): Int = runCatching {
        val groups = JSONObject(String(document, Charsets.UTF_8)).optJSONArray("groups")
            ?: return@runCatching 0
        var total = 0
        for (index in 0 until groups.length()) {
            total += groups.optJSONObject(index)?.optJSONArray("entries")?.length() ?: 0
        }
        total
    }.getOrDefault(0)

    /**
     * Turn one hosts document into the kernel's own JSON rule document.
     *
     * The kernel only ever consumes JSON: `RuleSet::from_slice` (behind
     * `startProxy`, the VPN `create`, and `filter`) parses with `parse_document`,
     * so handing it raw hosts text fails on the first byte. The *only* entry
     * point that speaks hosts is the merge's **second** argument, so a hosts
     * document is converted by merging it onto an empty JSON seed — the merge
     * does the parse, and it is the same tested Rust `parse_hosts` the daemon
     * uses.
     *
     * This is the path every hosts-shaped source takes — the default
     * `github-hosts` included — because both the cache and the tunnel need kernel
     * JSON, and a hosts file written to the cache is one that `filter` and the
     * tunnel cannot read at all.
     */
    private fun toKernelDocument(hosts: String): ByteArray =
        Kernel.merge(EMPTY_DOCUMENT, hosts.toByteArray(), secondIsHosts = true)

    private fun fetchText(url: String): String {
        val connection = (URL(url).openConnection() as HttpURLConnection).apply {
            connectTimeout = 20_000
            readTimeout = 90_000
            instanceFollowRedirects = true
        }
        return try {
            // The status check and [looksLikeHtml] answer two different
            // questions, and this one is not made redundant by the other. This
            // check is "did the server say it failed" — a non-200 (the old
            // endpoints were silently retired, so a 404 here is not hypothetical)
            // means there is nothing to read and the fetch must stop. It does
            // **not** catch a server that says it succeeded and then hands back a
            // web page: that is a 200, and [looksLikeHtml] is the only gate that
            // sees it. Nor would the entry count stop it — the measured GitCode
            // page parses to **1 fabricated entry**, not 0, from
            // `hm.src = "https://…"` (see [looksLikeHtml]). This check must not
            // be weakened on the theory that the HTML check covers it — a 404
            // body is an HTML page only by convention, and a non-200 body could
            // be anything.
            if (connection.responseCode != HttpURLConnection.HTTP_OK) {
                throw java.io.IOException("HTTP ${connection.responseCode} for $url")
            }
            connection.inputStream.bufferedReader().use { it.readText() }
        } finally {
            connection.disconnect()
        }
    }

    fun cacheFile(context: Context): File = File(context.filesDir, CACHE_NAME)

    /** The sidecar that records which source produced [cacheFile]. */
    fun sourceFile(context: Context): File = File(context.filesDir, SOURCE_NAME)

    /** Where imported copies live. Listed by [pruneImported], written by [importLocalBytes]. */
    fun importedDir(context: Context): File = File(context.filesDir, IMPORTED_DIR)

    /**
     * Drop the cache, so the next load fetches — and drop any imported copy that
     * no longer belongs to a source.
     *
     * The two are one call because they are the same decision seen twice: every
     * caller that reaches for this has just changed *which document should be
     * served*, and a copy left behind by a source the user deleted is a file that
     * nothing will ever read again. Doing it here rather than inside
     * [Prefs.removeRuleSource] is deliberate — [Prefs] has no file I/O by design
     * (see its KDoc on `rulesCacheDirty`), and this object already owns
     * everything under `filesDir`.
     */
    fun invalidate(context: Context) {
        discardCache(context)
        pruneImported(context)
    }

    /**
     * What [importLocal] and [importLocalBytes] did.
     *
     * A sealed result rather than a thrown exception because "this file is
     * already imported" is not a failure — the outcome the user asked for (this
     * document is now the active rule source) is exactly what happened — and a
     * caller that reported it as an error would be scolding them for picking the
     * right file twice.
     */
    sealed interface LocalImport {
        /** The copy was written and a new source was added and selected. */
        data class Added(val source: RuleSource) : LocalImport

        /** The same content was already imported; its source was selected. */
        data class AlreadyPresent(val source: RuleSource) : LocalImport

        /** Nothing was written and nothing changed. [reason] is user-facing. */
        data class Failed(val reason: String) : LocalImport
    }

    /**
     * Import the document behind [uri] and make it the active rule source.
     *
     * Blocking — it reads a stream and writes a file. Call it off the main thread.
     *
     * The whole operation lives here rather than in each screen because there are
     * two entry points (the settings screen and the rules screen) and the steps
     * have to agree: validate, copy, add, select, drop the stale cache, reload a
     * running tunnel. Five steps in two places is how the two screens end up
     * behaving differently, which this app has already paid for once.
     */
    fun importLocal(context: Context, uri: Uri): LocalImport {
        val bytes = try {
            readUri(context, uri)
        } catch (err: Throwable) {
            return LocalImport.Failed(
                  context.getString(
                      R.string.err_import_read_failed,
                      err.message ?: context.getString(R.string.common_unknown),
                  ),
              )
        } ?: return LocalImport.Failed(
            context.getString(R.string.err_import_too_large, MAX_LOCAL_BYTES / (1024 * 1024)),
        )
        return importLocalBytes(context, displayName(context, uri), bytes)
    }

    /**
     * Import [bytes] under the name [displayName], as [importLocal] does.
     *
     * Split out from [importLocal] because the control console imports from a
     * path rather than a URI, and that path has no `ContentResolver` to ask for a
     * display name. The two share everything after the bytes are in hand, which
     * is where all the decisions are.
     */
    fun importLocalBytes(context: Context, displayName: String, bytes: ByteArray): LocalImport {
        val label = displayName.trim().ifEmpty { context.getString(R.string.rules_local_default_label) }
        if (bytes.isEmpty()) return LocalImport.Failed(context.getString(R.string.err_import_empty))

        // Validated **before** anything is written. The point of the check is that
        // a file which cannot serve as a rule document never becomes a source at
        // all: writing first and validating after would leave a copy on disk, a
        // source that fails on every connect, and a delete button as the only way
        // out.
        try {
            parseDocument(context, String(bytes, Charsets.UTF_8), builtin = false, origin = label)
        } catch (err: Throwable) {
            return LocalImport.Failed(err.message ?: context.getString(R.string.err_import_not_a_document))
        }

        val fileName = sha256(bytes) + IMPORTED_SUFFIX
        val file = importedFile(context, fileName)
        try {
            file.parentFile?.mkdirs()
            file.writeBytes(bytes)
        } catch (err: Throwable) {
            return LocalImport.Failed(
                  context.getString(
                      R.string.err_import_write_failed,
                      err.message ?: context.getString(R.string.common_unknown),
                  ),
              )
        }

        val prefs = Prefs.of(context)
        val id = RuleSource.localId(fileName)
        val existing = prefs.ruleSources.firstOrNull { it.id == id }
        val source = existing ?: RuleSource(id = id, label = label, url = "", localFile = fileName)
        if (existing == null) {
            prefs.addRuleSource(source)
            KernelState.log(
                context,
                KernelState.LogEntry.Level.INFO,
                TAG,
                R.string.log_local_imported,
                label,
                bytes.size / 1024,
                fileName,
            )
        }
        // Selected as well as stored, and on both paths: a source that is added
        // but not selected changes nothing, and the user picked this file because
        // they want it used.
        prefs.updateRuleSource(source.id)
        invalidate(context)
        DetourVpnService.reloadRulesIfRunning()
        return if (existing == null) LocalImport.Added(source) else LocalImport.AlreadyPresent(source)
    }

    /**
     * Delete imported copies that no source names any more.
     *
     * Runs from [invalidate], which every source change already calls, so it
     * covers both ways a copy can be orphaned — the source was deleted, or an
     * import was superseded and the old source dropped — without either path
     * having to remember to clean up. Two guards matter: an unreadable [Prefs]
     * returns early rather than pruning against an empty set (which would delete
     * every copy on the device), and only files inside `imported-rules/` are
     * considered.
     */
    private fun pruneImported(context: Context) {
        val referenced = runCatching {
            Prefs.of(context).ruleSources.mapNotNull { it.localFile }.toSet()
        }.getOrNull() ?: return
        for (file in importedDir(context).listFiles() ?: return) {
            if (file.name in referenced) continue
            runCatching { file.delete() }
        }
    }

    private fun importedFile(context: Context, fileName: String): File =
        File(importedDir(context), fileName)

    /**
     * Read a whole stream, or `null` if it turns out to be larger than [cap].
     *
     * Bounded rather than `readBytes()`: the picker will hand over any file the
     * user taps, and `readBytes()` on a video is an `OutOfMemoryError` this app
     * cannot catch as a `Throwable`-and-continue. The cap is checked *while*
     * reading, so a huge file costs one buffer rather than its whole length.
     */
    private fun readBounded(input: InputStream, cap: Int): ByteArray? {
        val out = ByteArrayOutputStream()
        val buffer = ByteArray(16 * 1024)
        while (true) {
            val read = input.read(buffer)
            if (read < 0) break
            if (out.size() + read > cap) return null
            out.write(buffer, 0, read)
        }
        return out.toByteArray()
    }

    /**
     * The picked file's bytes, or `null` if it exceeds [MAX_LOCAL_BYTES].
     *
     * The stream is opened and checked separately rather than with `?:` on one
     * expression: `null` from the read means "too large", and `null` from
     * `openInputStream` means "cannot open", and collapsing the two would report
     * a file the user is allowed to pick as unopenable.
     */
    private fun readUri(context: Context, uri: Uri): ByteArray? {
        val stream = context.contentResolver.openInputStream(uri)
            ?: throw IOException(context.getString(R.string.common_input_stream_failed))
        return stream.use { readBounded(it, MAX_LOCAL_BYTES) }
    }

    /**
     * The picked file's display name, for the source's label.
     *
     * `OpenableColumns.DISPLAY_NAME` is what the picker itself shows the user, so
     * a source named after it is one they can match against what they tapped. The
     * fallback matters for the providers that do not implement the column — a
     * `content://` URI's last segment is usually an opaque id, but it is still
     * better than an empty label, and the last resort is a fixed string.
     */
    private fun displayName(context: Context, uri: Uri): String {
        val fallback = uri.lastPathSegment?.substringAfterLast('/')?.takeIf { it.isNotBlank() }
            ?: context.getString(R.string.rules_local_default_label)
        return runCatching {
            context.contentResolver
                .query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
                ?.use { cursor ->
                    if (cursor.moveToFirst()) {
                        cursor.getString(0)?.takeIf { it.isNotBlank() } ?: fallback
                    } else {
                        fallback
                    }
                } ?: fallback
        }.getOrDefault(fallback)
    }

    /** The lower-case hex SHA-256 of [bytes], which is how an imported copy is named. */
    private fun sha256(bytes: ByteArray): String =
        // `and 0xFF` on the widened int rather than relying on the formatter's own
        // handling of a negative `Byte`: Android's `Formatter` is its own
        // implementation, and a digest byte is negative half the time, so a
        // difference there would corrupt half of every name.
        MessageDigest.getInstance("SHA-256").digest(bytes)
            .joinToString("") { "%02x".format(it.toInt() and 0xFF) }
}
