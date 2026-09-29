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
 * ## Why `unavailable` still exists with no producer
 *
 * Deleting the retired `s302` source leaves **no built-in that sets
 * [unavailable]**, so the field currently has nothing that turns it on. It is
 * kept anyway, and that is a decision rather than an oversight.
 *
 * Removing it would make [usable] unconditionally true — `url.isNotEmpty()` is
 * true for every source the app can produce, since [decode] drops any stored
 * entry without a URL and the add-source dialog rejects one. A constant-true
 * `usable` does not shrink the code; it turns a whole chain into dead weight
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
) {
    /** Whether this source can be selected and fetched. */
    val usable: Boolean get() = url.isNotEmpty() && !unavailable

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
     */
    val identity: String get() = url

    companion object {
        const val GITHUB_HOSTS_ID = "github-hosts"
        const val HELLOGITHUB_HOSTS_ID = "hellogithub-hosts"

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
            "https://no-such-mirror-12345.invalid/hosts"

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
         * Serialise the user-added sources. Built-ins are skipped — see the class
         * comment for why they must not be frozen into storage.
         */
        fun encode(sources: List<RuleSource>): String {
            val array = JSONArray()
            for (source in sources) {
                if (source.builtin) continue
                array.put(
                    JSONObject()
                        .put("id", source.id)
                        .put("label", source.label)
                        .put("url", source.url),
                )
            }
            return array.toString()
        }

        /**
         * The built-ins, plus whatever custom sources were stored.
         *
         * Malformed or absent JSON yields [defaults] alone rather than throwing:
         * this runs on the path that builds the settings screen, and a corrupt
         * list must not be able to stop the app from showing its own sources.
         */
        fun decode(json: String?): List<RuleSource> {
            if (json.isNullOrBlank()) return defaults()
            val custom = runCatching {
                val array = JSONArray(json)
                (0 until array.length()).mapNotNull { index ->
                    val entry = array.optJSONObject(index) ?: return@mapNotNull null
                    val url = entry.optString("url").trim()
                    // A source with no URL can never be fetched, so storing one
                    // would only ever produce an un-selectable row.
                    if (url.isEmpty()) return@mapNotNull null
                    RuleSource(
                        id = entry.optString("id").ifEmpty { "custom:$url" },
                        label = entry.optString("label"),
                        url = url,
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
