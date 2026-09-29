package dev.detour.core

import android.content.Context
import android.util.Log
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject

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
 * to software this app does not ship. It survives as an un-selectable
 * [RuleSource] so the owner can still see that it exists.
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

    private const val CACHE_NAME = "rules.json"

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

    /** The document to hand the kernel. */
    @Synchronized
    fun load(context: Context, forceRefresh: Boolean = false): ByteArray {
        val prefs = Prefs.of(context)
        val cached = cacheFile(context)

        // A cache left by a build that wrote no identity stamp cannot be validated,
        // so the migration flags it and this is where the flag is honoured. Checked
        // *before* the cache is examined so the delete happens on the same load
        // that would otherwise have reused it, which is the whole point of the
        // upgrade repair being a flag rather than a silent wait for `refreshHours`.
        if (prefs.consumeRulesCacheDirty()) {
            discardCache(context)
            KernelState.log(
                KernelState.LogEntry.Level.INFO, TAG,
                "规则缓存来自旧版本，已丢弃并重新拉取",
            )
        }

        // "Usable" is now two facts, not one: a document has to be present **and**
        // the sidecar has to say it is the document that would be fetched now. A
        // document whose stamp is missing is treated exactly like no document at
        // all — that is the case the device hit, where a stale `/2` document sat
        // next to a `hosts` preference and was served as if it were current.
        val stamp = sourceStamp(context)
        val cachedSource = readStamp(context)
        val identityMatches = stamp != null && cachedSource == stamp
        val hasCache = cached.isFile && cached.length() > 0 && identityMatches

        // Two settings decide whether the network is touched at all, and they are
        // not the same decision. `offline` is absolute — "use what is already
        // downloaded", no fetch however stale the cache is. `refreshHours` is the
        // cache's lifetime: before it elapses the download is reused, after it the
        // document is fetched again. Without the age check the cache was reused
        // forever, so "刷新间隔" was a number that changed nothing.
        val ageMillis = if (hasCache) {
            (System.currentTimeMillis() - cached.lastModified()).coerceAtLeast(0L)
        } else {
            Long.MAX_VALUE
        }
        val maxAgeMillis = prefs.refreshHours.coerceIn(1, 72) * 3_600_000L
        val stale = ageMillis >= maxAgeMillis

        val merged = when {
            prefs.offline -> {
                // Offline cannot fetch, so the file on disk is the only candidate.
                // Whether it may be *used* turns on how much is known about it,
                // and there are three cases, not two.
                if (!cached.isFile || cached.length() == 0L) {
                    // Refusing is the honest answer: the user asked for no network
                    // and there is nothing downloaded to fall back on. Returning
                    // an empty document would start a tunnel that relays nothing,
                    // which looks like a working tunnel that drops every packet.
                    throw IllegalStateException("离线模式，且本地还没有已下载的规则")
                }
                if (cachedSource != null && stamp != null && cachedSource != stamp) {
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
                        KernelState.LogEntry.Level.ERROR, TAG,
                        "离线模式：缓存来自 $cachedSource，与当前来源 $stamp 不符，拒绝使用" +
                            "（该文档必然不是所选规则集，用它只会得到一个不转发任何流量的隧道）",
                    )
                    throw IllegalStateException(
                        "离线模式：已下载的规则属于 $cachedSource，与当前选择的 $stamp 不符；" +
                            "请联网重新下载，或切回 $cachedSource",
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
                        KernelState.LogEntry.Level.WARN, TAG,
                        "离线模式：缓存无来源标记，无法校验，仍按离线要求使用",
                    )
                }
                KernelState.log(KernelState.LogEntry.Level.INFO, TAG, "离线模式：使用已下载的规则")
                cached.readBytes()
            }
            forceRefresh || stale || !identityMatches -> fetchInto(context, cached)
            else -> cached.readBytes()
        }
        return applySwitches(context, merged)
    }

    /**
     * The identity to stamp, or `null` when the source cannot be resolved.
     *
     * `null` means "do not trust anything": a stamp can never equal it, so the
     * cache is treated as a miss and refetched, and [writeStamp] writes nothing. A
     * lazily-constructed `Prefs` can only fail to materialise if the settings store
     * itself is unreadable, which is a state where guessing the cache's provenance
     * is worse than refetching.
     */
    private fun sourceStamp(context: Context): String? =
        runCatching { Prefs.of(context).selectedSource.identity }.getOrNull()

    /** Which source the cached document says it came from, or `null` if unstamped. */
    private fun readStamp(context: Context): String? =
        runCatching {
            val file = sourceFile(context)
            if (file.isFile) file.readText().trim().ifEmpty { null } else null
        }.getOrNull()

    /**
     * Fetch the document and write it to the cache.
     *
     * On failure the cache is the fallback — a tunnel that cannot start without a
     * network is a tunnel that cannot start on a plane — and only when there is no
     * cache at all is the failure real.
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
    private fun fetchInto(context: Context, cached: File): ByteArray {
        return try {
            val fetched = fetch(context)
            cached.writeBytes(fetched)
            writeStamp(context)
            KernelState.log(
                KernelState.LogEntry.Level.INFO, TAG,
                "规则已更新（${fetched.size / 1024} KiB，来源 ${sourceStamp(context) ?: "未知"}）",
            )
            fetched
        } catch (err: Throwable) {
            Log.w(TAG, "fetch failed", err)
            KernelState.log(
                KernelState.LogEntry.Level.WARN, TAG, "拉取规则失败：${err.message}",
            )
            val identityMatches = sourceStamp(context)?.let { it == readStamp(context) } ?: false
            // The fallback only exists for a cache that still describes a document
            // this build would accept. Serving one whose stamp does not match would
            // re-create the bug this round fixes — a fetch failure would hand the
            // tunnel the very document the stamp had just rejected — so a mismatched
            // cache is re-thrown as a failure instead of quietly used.
            if (cached.isFile && cached.length() > 0 && identityMatches) {
                KernelState.log(KernelState.LogEntry.Level.INFO, TAG, "使用缓存的规则")
                cached.readBytes()
            } else {
                if (cached.isFile && cached.length() > 0) {
                    KernelState.log(
                        KernelState.LogEntry.Level.WARN, TAG,
                        "缓存来源与当前不符，拉取又失败，不能使用",
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
     */
    private fun writeStamp(context: Context) {
        val stamp = sourceStamp(context) ?: return
        runCatching { sourceFile(context).writeText(stamp) }
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
                KernelState.LogEntry.Level.INFO, TAG,
                "已按 ${disabled.size} 个开关过滤规则：" +
                    "${document.size / 1024} KiB → ${filtered.size / 1024} KiB",
            )
            filtered
        } catch (err: Throwable) {
            // Falling back to the unfiltered document is wrong in a specific way:
            // the user switched something off and it stays on. Saying so is the
            // only honest option, and it is still better than refusing to start.
            KernelState.log(
                KernelState.LogEntry.Level.ERROR, TAG,
                "过滤规则失败，本次使用未过滤的规则：${err.message}",
            )
            document
        }
    }

    /**
     * Fetch the selected source and return a kernel document that is known to
     * hold at least one rule.
     *
     * The gates are not decoration. `fetchText` already refuses a non-200,
     * because a 404 here is an HTML page that would parse as zero rules — but a
     * *successful* response can also be unusable, and the kernel accepts it
     * silently. See [looksLikeHtml] and [requireUsable].
     *
     * Every URL in [RuleSource.fetchUrls] is tried in order and the first one
     * that clears both gates wins. Endpoints die — eight of the eleven measured
     * candidates for the default document were already gone — so a single URL is
     * a single point of failure the user cannot see or fix.
     */
    private fun fetch(context: Context): ByteArray {
        val source = Prefs.of(context).selectedSource
        if (!source.usable) {
            // Reachable only if something selected a source whose `usable` later
            // became false — the UI will not offer one. Refusing loudly beats
            // opening a connection to an empty URL.
            throw IllegalStateException("规则源「${source.id}」暂不可用")
        }

        val urls = source.fetchUrls
        val failures = mutableListOf<String>()
        for ((index, url) in urls.withIndex()) {
            // The index is in the log because a fallback that succeeds on the
            // third try and one that succeeds on the first look identical in a
            // log otherwise, and "which endpoint is actually carrying this
            // install" is the question the mirrors exist to make answerable.
            KernelState.log(
                KernelState.LogEntry.Level.INFO, TAG,
                "拉取规则：${source.id}（候选 ${index + 1}/${urls.size}，$url）",
            )
            try {
                val text = fetchText(url)
                // A 200 can still not be a rule document — see [looksLikeHtml] —
                // and this check runs before the shape sniff so an HTML page is
                // never fed to either parser.
                if (looksLikeHtml(text)) {
                    throw java.io.IOException("响应是 HTML 页面，不是规则文档")
                }
                // A custom source may publish the kernel's own JSON rather than
                // hosts text, so the shape is sniffed before the hosts conversion
                // is applied. Converting an already-JSON document would be a parse
                // of the wrong grammar, and the merge would return zero entries
                // rather than fail.
                val document =
                    if (looksLikeKernelJson(text)) text.toByteArray() else toKernelDocument(text)
                return requireUsable(document, url)
            } catch (err: Throwable) {
                failures += "$url → ${err.message}"
                KernelState.log(
                    KernelState.LogEntry.Level.WARN, TAG,
                    "候选 ${index + 1}/${urls.size} 失败：$url（${err.message}）",
                )
            }
        }

        // Every address is reported, not just the last failure. The last error
        // alone cannot answer the only question worth asking here — "why did all
        // of them fail" — and it hides whether one endpoint returned HTML while
        // the rest were unreachable.
        throw IllegalStateException(
            "规则源「${source.id}」的 ${urls.size} 个地址全部失败：" + failures.joinToString("；"),
        )
    }

    /**
     * Whether a fetched body is an HTML page rather than a rule document.
     *
     * A status check is not enough, and this is measured, not hypothetical:
     * `gitcode.com` answers **HTTP 200 with a rendered HTML page** for a plain
     * text path that does not exist. The status check passes, the document parses
     * to zero entries, and the tunnel comes up relaying nothing — a "successful"
     * fetch that produces the worst possible state. HTML is what that false
     * success looks like, so it is what this refuses.
     *
     * It is not a redundant second gate on top of the entry count. The two catch
     * different things, and this one catches it *earlier*: an HTML page is
     * rejected as "not a document at all", whereas [requireUsable] only sees the
     * aftermath (zero entries). The reason HTML also happens to produce zero
     * entries is structural — the tokens in an error page (`<html>`, `<!DOCTYPE`,
     * `href="…"`) carry angle brackets, quotes and colons that
     * `normalize_domain`'s character class rejects, and a single-token line has
     * no second field for the hosts parser to read — but relying on that would
     * mean a future parser change could turn an HTML page back into a silent
     * zero-rule tunnel.
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
     * Refuse a document that would relay nothing.
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
     * [url] is the address that was actually tried, not the source's primary one:
     * with mirrors in play the primary URL is often *not* what was fetched, and
     * an error naming an address that was never contacted sends the reader to the
     * wrong endpoint.
     */
    private fun requireUsable(document: ByteArray, url: String): ByteArray {
        if (entryCount(document) == 0) {
            throw IllegalStateException("规则源没有解析出任何规则：$url")
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
            // A 404 from this host is an HTML error page. Without the status
            // check it parses as zero rules and the tunnel starts up relaying
            // nothing — a working-looking tunnel that drops every packet, which
            // is exactly the failure `watt-rules` warns about. The old
            // endpoints were silently retired, so this is not hypothetical.
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

    /** Drop the cache, so the next load fetches. */
    fun invalidate(context: Context) {
        discardCache(context)
    }
}
