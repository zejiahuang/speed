package dev.detour.core

import dev.detour.R
import org.json.JSONArray
import org.json.JSONObject

/**
 * One place the rule document can be fetched from.
 *
 * The list used to be three hard-coded option values (`merged`, `hosts`, `s302`)
 * with the fetch URL baked into [RulesRepository]. Two of those names pointed at
 * the same document — see [RulesRepository]'s class comment — so the settings
 * screen showed one document as two chips, and a fourth source could not be
 * added without a new build. This type replaces all of that with data: a source
 * is a URL plus how it should read, and the set of them lives in [Prefs].
 *
 * ## Built-in versus custom
 *
 * The two built-in sources are **defined in code, not in storage**. [decode]
 * always returns [defaults] first and reads only custom entries from the store,
 * so a built-in's `url` / `mirrors` / `labelRes` / `unavailable` follow the
 * installed version. That is deliberate: the default source has already moved
 * once (off the retired `abhuang` endpoints), and had the URL been persisted,
 * every existing install would have kept fetching a dead host with no way for an
 * update to fix it. Only user-added sources — the ones the app cannot know — are
 * stored.
 *
 * ## The third category: a source that is not a choice
 *
 * [rootSource] is neither of those. It is **defined in code like a built-in but
 * deliberately absent from [defaults]**, so it is never stored *and* never
 * offered: root mode loads it whatever [Prefs.selectedSource] says, and no screen
 * lists it as something to pick. It exists because root mode cannot work with a
 * source the user chose — see [rootSource] for the measurement — and a source
 * that is forced is not a source that should appear in a list of options.
 *
 * Keeping it out of [defaults] is what makes that airtight. [Prefs.selectedSource]
 * resolves against the stored list, so an id that is not in the list cannot be
 * selected, cannot be deleted, and cannot be reached by a `set rule_source`
 * command — the only way to get it is to ask for it by name, which is what the
 * service does when it starts in root mode.
 *
 * ## The third kind: a source that is a file on this device
 *
 * A user-added source used to mean exactly one thing — a URL. [localFile] adds a
 * second: the name of a copy, inside the app's own private directory, of a
 * document the user picked with the system file chooser. It is stored as a
 * *third* kind rather than as "a URL with a `file://` scheme" because every layer
 * below this one would have to grow a scheme check to tell the two apart —
 * [RulesRepository] would have to decide whether to open a socket, and a
 * `file://` URL is also a path a hostile document could point outside the
 * sandbox with. A separate field keeps "fetch this" and "read this" two distinct
 * questions with two distinct answers, and lets [usable] stop depending on [url]
 * for the local case.
 *
 * **The copy, not the original.** The picked file's content is copied into
 * `filesDir/imported-rules/` and named after its own SHA-256. That makes
 * [identity] content-addressed for free — see below — and it means the source
 * survives the original being moved, renamed or revoked, which is the normal
 * fate of a URI grant. The cost is stated in the UI: editing the original does
 * nothing until it is imported again.
 *
 * ## Why `unavailable` still exists with no producer
 *
 * Deleting the retired `s302` source leaves **no built-in that sets
 * [unavailable]**, so the field currently has nothing that turns it on. It is
 * kept anyway, and that is a decision rather than an oversight.
 *
 * Removing it would make [usable] true for every source the app can produce —
 * [decode] drops any stored entry with neither a URL nor a local copy, the
 * add-source dialog rejects an empty URL, and an imported file is usable by
 * construction. A `usable` that is true for every value the app can hold does
 * not shrink the code; it turns a whole chain into dead weight
 * that still has to be maintained: [unavailable]'s own branch in
 * `RuleSourceRow`'s subtitle, the `rules_source_unavailable` string, the "first
 * selectable source" step of `Prefs.selectedSource`'s fallback, and the
 * `enabled = source.usable` guards in `SettingsScreen` and `RulesScreen`. That
 * is five files, and the selection logic among them has been verified on a
 * device. Paying that cost is a larger and riskier change than the one being
 * made here, and "a source that cannot be selected" remains a state the type
 * should be able to express.
 *
 * So it stays, deliberately. Anyone who deletes it has to collect that entire
 * chain with it, not just this declaration.
 */
data class RuleSource(
    val id: String,
    /** Display name for a custom source. Built-ins use [labelRes] instead. */
    val label: String,
    /** The `strings.xml` resource for a built-in's name; `null` for custom. */
    val labelRes: Int? = null,
    /** Where to fetch. Empty means there is nothing to fetch. */
    val url: String,
    /**
     * Extra URLs that can serve this source, tried after [url].
     *
     * A single endpoint is a single point of failure: of the eleven candidate
     * endpoints measured for the default document, eight were already dead. A
     * list of mirrors is the only shape that survives one of them dying, and
     * because every entry serves the *same* document the order is a speed
     * preference, not a correctness one.
     */
    val mirrors: List<String> = emptyList(),
    val builtin: Boolean = false,
    val unavailable: Boolean = false,
    /**
     * The name of this source's copy in `filesDir/imported-rules/`, or `null` for
     * a source that is fetched over the network.
     *
     * A bare file name and never a path: the directory is fixed, and a stored
     * value that could walk out of it (`../`, an absolute path) would turn a
     * hand-edited settings file or backup into a read of anything the app can
     * reach. [decode] rejects such a name outright, so everything downstream may
     * assume a single path segment.
     */
    val localFile: String? = null,
) {
    /** Whether this source is a file on this device rather than an address. */
    val isLocal: Boolean get() = localFile != null

    /**
     * Whether this source can be selected and loaded.
     *
     * A local source has no [url] to be non-empty, so the old single expression
     * would have marked every imported file un-selectable — the one thing this
     * kind of source exists to be.
     */
    val usable: Boolean get() = when {
        unavailable -> false
        isLocal -> true
        else -> url.isNotEmpty()
    }

    /** Every URL that can serve this source, in the order they should be tried. */
    val fetchUrls: List<String> get() = if (url.isEmpty()) emptyList() else listOf(url) + mirrors

    /**
     * What the cache sidecar records for this source.
     *
     * The URL, not the [id]: an id is a name the user can delete and re-add, and
     * two different URLs must never share a cache stamp. The URL is the document
     * itself, so it is the honest identity.
     *
     * **[mirrors] is deliberately excluded.** Every mirror serves the same
     * document, so folding the one that happened to answer into the identity
     * would make the cache stamp depend on *which machine won a race*: the first
     * fetch succeeds on a mirror, the second falls back to [url], the two stamps
     * disagree, and a perfectly good cache is thrown away on every load. The
     * identity answers "which document is this", not "which host served it", and
     * only [url] is stable enough to name the document.
     *
     * A local source's identity is its **copy's file name**, which is the
     * SHA-256 of the content. That is the same principle one level stronger: for
     * a local file there is no host at all, and the only thing that can make the
     * document different is its bytes — so naming it after those bytes makes the
     * stamp change exactly when the content does. Re-importing the same file
     * keeps the stamp (and the cache); re-importing an edited one changes both.
     * The `local:` prefix keeps the stamp from ever colliding with a URL.
     */
    val identity: String get() = localFile?.let { "$LOCAL_ID_PREFIX$it" } ?: url

    companion object {
        const val GITHUB_HOSTS_ID = "github-hosts"
        const val HELLOGITHUB_HOSTS_ID = "hellogithub-hosts"

        /**
         * The id of the source root mode is pinned to. See [rootSource].
         *
         * Not in [defaults], so it is not a value [Prefs.selectedSource] can ever
         * return. The id is here for [forMode] to hand back, for the settings and
         * rules screens to recognise the pinned case, and for logs.
         */
        const val S302_UPSTREAM_ID = "s302-upstream"

        /**
         * The prefix that marks an [id] — and an [identity] — as belonging to a
         * local copy.
         *
         * A shared constant because the two uses have to agree: the id is what a
         * shell passes to `rules remove:<id>`, and the identity is what the cache
         * sidecar holds. Neither may be mistaken for a URL.
         */
        const val LOCAL_ID_PREFIX = "local:"

        /** The id of the source wrapping the local copy named [fileName]. */
        fun localId(fileName: String): String = "$LOCAL_ID_PREFIX$fileName"

        /**
         * Whether a stored [localFile] value is a bare file name.
         *
         * Checked on the way in from storage, not on the way out, because this is
         * the only place a value this app did not write can enter — a
         * `SharedPreferences` entry or a backup file edited by hand. Everything
         * downstream joins the name onto a fixed directory, so a name carrying a
         * separator or a `..` would resolve *outside* it.
         */
        fun isSafeLocalName(name: String): Boolean =
            name.isNotEmpty() &&
                name.length <= 128 &&
                name != "." && name != ".." &&
                name.none { it == '/' || it == '\\' || it == '\u0000' }

        /**
         * The `maxiaof/github-hosts` document: a plain hosts file with no
         * `# === [x] ===` sections, so it parses to a single `hosts` group. Its
         * shape is described in `RulesRepository`'s class comment, and for the
         * same reason that comment gives, no byte count is recorded here — it
         * tracks a document that changes.
         *
         * This is **no longer the default** — that moved to
         * [HELLOGITHUB_HOSTS_URL] — but it is still offered as the second
         * selectable source, so its transport still has to work for whoever picks
         * it. The primary URL is a **reverse proxy**, not the upstream raw URL,
         * and that is not a convenience. `raw.githubusercontent.com` is served by
         * GitHub, whose IPv4 addresses are blocked in mainland China, and
         * Android's `HttpURLConnection` does **not** fall back to IPv6 when the A
         * record is unreachable. A direct fetch therefore fails for every user in
         * that network, deterministically. The proxy serves the *same bytes* —
         * measured byte-for-byte identical to upstream — so this changes the
         * transport, not the document.
         */
        private const val GITHUB_HOSTS_URL =
            "https://gh-proxy.com/https://raw.githubusercontent.com/maxiaof/github-hosts/master/hosts"

        /**
         * The other endpoints that serve the same document, tried in this order
         * after [GITHUB_HOSTS_URL].
         *
         * Proxies first, CDNs second: the CDN entries are caches and were measured
         * a day behind on some of their gcore nodes, so a proxy that is live
         * delivers a fresher document. The upstream URL is **last on purpose** —
         * for a mainland user it is the one address that cannot work, but for a
         * user outside China it is the nearest route, and it is the only entry
         * here that is not a mirror, so it is the address we are certain will
         * still name this document if every mirror disappears. The cost of that
         * certainty is one extra connection timeout when nothing else answers.
         */
        private val GITHUB_HOSTS_MIRRORS = listOf(
            "https://fastly.jsdelivr.net/gh/maxiaof/github-hosts@master/hosts",
            "https://cdn.jsdelivr.net/gh/maxiaof/github-hosts@master/hosts",
            "https://raw.githubusercontent.com/maxiaof/github-hosts/master/hosts",
        )

        /**
         * The default source: `521xueweihan/GitHub520`, served by HelloGitHub.
         *
         * It was chosen over [GITHUB_HOSTS_URL] on measurements: 40 entries (the
         * `maxiaof` document has 37), the lowest latency of every candidate
         * (0.10–0.12 s), a host in Hong Kong (UCloud, AS135377) reachable from
         * mainland China without a proxy, and a document updated the same day it
         * was measured, covering the GitHub domains this whole feature exists for.
         *
         * ## The two risks, stated rather than hidden
         *
         * **The server expires on 2026-12-31.** That is why
         * [HELLOGITHUB_HOSTS_MIRRORS] exists — but it is not a reason to relax.
         * The mirror is another endpoint of the *same* upstream document, so it
         * keeps the document reachable after the Hong Kong host is gone; the
         * primary URL itself will stop working, and someone has to replace it
         * before then. Mirrors downgrade "immediately unusable" to "usable but
         * slower"; they do not make the deadline disappear.
         *
         * **The licence is CC BY-NC-ND 4.0**, *stricter* than the Mulan PSL v2 of
         * [GITHUB_HOSTS_URL]: NC forbids commercial use and ND forbids
         * derivatives. The owner was told both facts and chose it as the default
         * anyway. The app's position is that it references the document **by URL
         * at runtime and does not bundle it into the APK** — the download happens
         * on the user's device from the upstream host, so the app is a client of
         * the document rather than a redistributor of it.
         *
         * Its URL must **not** be folded into [GITHUB_HOSTS_MIRRORS] (nor the
         * reverse). The two serve different documents under different licences,
         * and one shared fallback list would silently hand users a document the
         * source label does not name.
         */
        private const val HELLOGITHUB_HOSTS_URL = "https://raw.hellogithub.com/hosts"

        /** Another endpoint for the same GitHub520 document, tried after the primary. */
        private val HELLOGITHUB_HOSTS_MIRRORS = listOf(
            "https://gh-proxy.com/https://raw.githubusercontent.com/521xueweihan/GitHub520/main/hosts",
        )

        /**
         * A GitHub *page* URL names the same file as its raw URL, but only the raw
         * one returns the file. `github.com/<owner>/<repo>/blob/<path>` is what a
         * browser address bar holds, so it is what a person pastes; rewriting it is
         * the difference between a source that works and one that downloads HTML.
         * Anything else is returned trimmed, unjudged.
         */
        private val GITHUB_BLOB =
            Regex("""^https?://github\.com/([^/]+)/([^/]+)/blob/(.+)$""")

        /**
         * The built-in sources, always in this order: the default document first,
         * then the other selectable one. The order is the order the settings
         * screen lists them, so the default has to be first — it is what a fresh
         * install is already on, and anything above it would make the top row
         * disagree with the selected radio.
         */
        fun defaults(): List<RuleSource> = listOf(
            RuleSource(
                id = HELLOGITHUB_HOSTS_ID,
                label = "",
                labelRes = R.string.rules_source_hellogithub,
                url = HELLOGITHUB_HOSTS_URL,
                mirrors = HELLOGITHUB_HOSTS_MIRRORS,
                builtin = true,
            ),
            RuleSource(
                id = GITHUB_HOSTS_ID,
                label = "",
                labelRes = R.string.rules_source_github_hosts,
                url = GITHUB_HOSTS_URL,
                mirrors = GITHUB_HOSTS_MIRRORS,
                builtin = true,
            ),
        )

        /**
         * The document root mode is pinned to, and why it is not the one the
         * settings screen offers.
         *
         * Root mode hijacks names into a local TLS-terminating proxy — the hosts
         * file points them at [RootHelper.REDIRECT_ADDRESS] and an iptables rule
         * sends port 443 to the proxy — so the proxy has to **re-originate** every
         * connection it terminates. A rule source is therefore usable in root mode
         * only if it says, per domain, where that connection should go.
         *
         * The s302 hosts document (`/2`) does not, and it fails in a way that looks
         * like a working rule set:
         *
         * ```text
         * /2   127.0.0.1  store.steampowered.com   (862 domains, every address 127.0.0.1)
         * ```
         *
         * `127.0.0.1` there is **s302's own listener**, correct for a machine
         * running s302 and meaningless on a phone. Measured on the device with the
         * app installed and the s302 hosts file absent: nothing is listening on
         * 443 at all (`ss -ltn | grep :443` empty, rc=1), while the proxy itself
         * binds a random high port. So every hijacked domain would be redirected
         * into a closed port — a RST per connection, for 862 domains.
         *
         * `/3` is the same rule set in the shape this needs:
         *
         * ```text
         * /3   steamstore-a.akamaihd.net.edgesuite.net<TAB>store.steampowered.com
         * ```
         *
         * The first field is a **name**, and `watt-rules` reads a non-IP first field
         * as a dial name (`hosts.rs`), so the proxy gets an address that resolves
         * from this network. Five sampled targets all completed TLS from the device
         * (the alternatives — resolving the original names out of band — were
         * measured and do not work: every foreign DoH endpoint is blocked here and
         * the reachable domestic ones answer with poisoned addresses).
         *
         * **The name is also what the origin has to be asked for.** Measured against
         * one edge address: the client's own name gave `rc=35`, the target name gave
         * `200` / 14399 bytes. That is why the kernel keeps the pairing between an
         * address and the name that produced it — see `Plan::dial_addresses`.
         *
         * ## Coverage, stated rather than implied
         *
         * `/2` has **862** domains and `/3` has **714**; the **148** in between are
         * not one category. They were counted from the caddyfile, not assumed, and
         * the split is **42 / 90 / 16**:
         *
         * * **42** are named literally in an HTTPS block whose `reverse_proxy`
         *   target is a **bare IPv4** — `itch.io` → `45.79.115.66`,
         *   `xdaforums.com` → `3.214.214.173`, `dropbox.com` → `162.125.248.18`.
         *   The aggregator accepts an upstream token only if it has a domain shape
         *   (`S302Parser::parseBlock`'s `/^[a-z0-9][a-z0-9.\-]*\.[a-z]{2,}$/`), and
         *   an address does not, so the target is discarded and the domain ends up
         *   with no upstream at all.
         * * **90** are reached only through a **wildcard head** on one of those same
         *   blocks (`*.gravatar.com`, `*.artstation.com`,
         *   `*.dl-eu.dropboxusercontent.com`). The hosts file expands the wildcard,
         *   so the names exist in `/2`, but no block ever names them, and the block
         *   they belong to is dropped whole.
         * * **16** are in `http://` blocks with **no `reverse_proxy` at all** — 12
         *   `redir` to a China mirror, 4 carry neither directive. These are port 80,
         *   which root mode does not hijack (the iptables rule rewrites 443), so
         *   they are out of scope rather than lost.
         *
         * **Both of the first two categories are recovered, and the earlier claim
         * that they could not be was wrong.** Accepting a bare IPv4 is not enough on
         * its own — the kernel would still put the **requested domain** in the SNI
         * (`proxy.rs` falls back to the client's name when no dial name produced the
         * address), and that is the case that fails: 11 of the 13 addresses reset the
         * handshake or time out when asked for the original name. What the format was
         * missing was a way to say "dial this address and send **no SNI**".
         *
         * **It has one now.** A first field of `!<address>` means exactly that:
         * `RuleSet` reads it with `strip_prefix('!')`, and `proxy.rs` presents nothing
         * when the address that answered is in the entry's `no_sni`. The puller writes
         * that form for a `reverse_proxy` target that is a bare IPv4.
         *
         * The **90** came back with the 42, and the reasoning that they would not was
         * wrong. An INI section's `Domain` field is **plaintext** and lists those names
         * outright (`0.gravatar.com`, `1.gravatar.com`, …); `parse()` unions it with
         * the block's `hosts`, and the aggregator attaches the section's upstreams to
         * *every* domain it holds. So they had no upstream for one reason only — the
         * section's sole target was a bare address and was dropped. The wildcard head
         * never mattered. Measured on the live `/3`: **714 → 846 domains,
         * +132 = 42 + 90, nothing lost.**
         *
         * ⚠️ **The `!<address>` form works, but only partly, and one measurement
         * disagrees with another.** On the emulator, six of the new domains went
         * through the MITM as `served from <address>:443 as <no SNI>`, with
         * `no_sni_upstreams` moving 0 → 6 while the named-upstream control stayed at 1.
         * **Two answered** (`dropbox.com` → 301, `0.gravatar.com` → 200) and four hung.
         * The bare-`curl` probe had recorded `artstation.com` → 403 for the same address
         * with no SNI, so **the two disagree and the disagreement is not explained** —
         * do not invent a reason. Nothing is worse off either way: these domains were
         * `direct` before, and direct does not reach them.
         */
        private const val S302_UPSTREAM_URL = "https://abhuang.dpdns.org/3"

        /**
         * The source root mode loads, whatever is selected elsewhere.
         *
         * `builtin = true` so [parseDocument] applies the built-in entry floor: this
         * document's size is known, and a truncated fetch must be rejected rather
         * than start a mode that hijacks a handful of names.
         */
        fun rootSource(): RuleSource = RuleSource(
            id = S302_UPSTREAM_ID,
            label = "",
            labelRes = R.string.rules_source_s302_upstream,
            url = S302_UPSTREAM_URL,
            builtin = true,
        )

        /**
         * The source [mode] will load: [rootSource] in root mode, [selected] in the
         * other two.
         *
         * **The mode is a parameter rather than read from [Prefs].** Two layers ask
         * this question and they must not be able to answer it differently — the
         * service asks it about the mode it was *told to start*, and the screens ask
         * it about the mode that is *running* (`KernelState.status.mode`, which is
         * also what the connect button passes on). Taking the mode as an argument is
         * what makes that one rule instead of two implementations of it; a version
         * that read the stored setting would be a third opinion.
         */
        fun forMode(mode: KernelState.Mode, selected: RuleSource): RuleSource =
            if (mode == KernelState.Mode.ROOT) rootSource() else selected

        /**
         * Serialise the user-added sources. Built-ins are skipped — see the class
         * comment for why they must not be frozen into storage.
         *
         * `local_file` is written only when there is one, so a fetched source's
         * entry keeps the shape every earlier build wrote and reads back
         * unchanged.
         */
        fun encode(sources: List<RuleSource>): String {
            val array = JSONArray()
            for (source in sources) {
                if (source.builtin) continue
                val entry = JSONObject()
                    .put("id", source.id)
                    .put("label", source.label)
                    .put("url", source.url)
                source.localFile?.let { entry.put("local_file", it) }
                array.put(entry)
            }
            return array.toString()
        }

        /**
         * The built-ins, plus whatever custom sources were stored.
         *
         * Malformed or absent JSON yields [defaults] alone rather than throwing:
         * this runs on the path that builds the settings screen, and a corrupt
         * list must not be able to stop the app from showing its own sources.
         *
         * A local entry is kept only if its `local_file` is a bare file name (see
         * [isSafeLocalName]) **and** it has no URL: the two are alternatives, and
         * an entry carrying both is a document this app never wrote. Dropping it
         * is the same call the "no URL" rule below makes — a source that cannot be
         * read is a row that can only disappoint.
         */
        fun decode(json: String?): List<RuleSource> {
            if (json.isNullOrBlank()) return defaults()
            val custom = runCatching {
                val array = JSONArray(json)
                (0 until array.length()).mapNotNull { index ->
                    val entry = array.optJSONObject(index) ?: return@mapNotNull null
                    val url = entry.optString("url").trim()
                    val localFile = entry.optString("local_file").trim().ifEmpty { null }
                    if (localFile != null && !isSafeLocalName(localFile)) return@mapNotNull null
                    if (localFile != null && url.isNotEmpty()) return@mapNotNull null
                    // A source with neither an address nor a local copy can never
                    // be loaded, so storing one would only ever produce an
                    // un-selectable row.
                    if (url.isEmpty() && localFile == null) return@mapNotNull null
                    RuleSource(
                        id = entry.optString("id").ifEmpty {
                            localFile?.let { localId(it) } ?: "custom:$url"
                        },
                        label = entry.optString("label"),
                        url = url,
                        localFile = localFile,
                    )
                }
            }.getOrNull() ?: return defaults()
            return defaults() + custom
        }

        /** Rewrite a GitHub blob URL to its raw equivalent; otherwise trim. */
        fun normalizeUrl(input: String): String {
            val trimmed = input.trim()
            val match = GITHUB_BLOB.matchEntire(trimmed) ?: return trimmed
            val (owner, repo, path) = match.destructured
            return "https://raw.githubusercontent.com/$owner/$repo/$path"
        }
    }
}
