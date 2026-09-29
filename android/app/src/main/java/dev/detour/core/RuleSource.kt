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
 * The three built-in sources are **defined in code, not in storage**. [decode]
 * always returns [defaults] first and reads only custom entries from the store,
 * so a built-in's `url` / `mirrors` / `labelRes` / `unavailable` follow the
 * installed version. That is deliberate: the default source has already moved
 * once (off the retired `abhuang` endpoints), and had the URL been persisted,
 * every existing install would have kept fetching a dead host with no way for an
 * update to fix it. Only user-added sources — the ones the app cannot know — are
 * stored.
 *
 * ## The retired second source
 *
 * `s302` is kept as a built-in **that cannot be selected** rather than deleted.
 * The owner asked for it to read "2暂不可以使用" and to be un-selectable, which
 * needs a row to exist; a source that is simply absent cannot show that text.
 * [unavailable] is what makes it un-selectable — [usable] is false — and its
 * empty [url] means a fetch would have nothing to open even if something
 * selected it by hand.
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
        const val S302_ID = "s302"

        /**
         * The default source: `maxiaof/github-hosts`, a plain hosts file with no
         * `# === [x] ===` sections. Measured: 1740 bytes, 37 domains, so it parses
         * to a single `hosts` group of 37 domains / 37 addresses.
         *
         * The primary URL is a **reverse proxy**, not the upstream raw URL, and
         * that is not a convenience. `raw.githubusercontent.com` is served by
         * GitHub, whose IPv4 addresses are blocked in mainland China, and Android's
         * `HttpURLConnection` does **not** fall back to IPv6 when the A record is
         * unreachable. A direct fetch therefore fails for every new user in that
         * network, deterministically, which is exactly the first-run experience
         * this constant exists to prevent. The proxy serves the *same bytes* —
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
         * The optional second source: `521xueweihan/GitHub520`, served by
         * HelloGitHub. A **different document** from the default (40 domains, not
         * 37), so it is offered as its own selectable source.
         *
         * It is **not** the default, for two reasons that are about the document
         * and not the transport:
         *
         * * Its licence is **CC BY-NC-ND 4.0** — non-commercial and no
         *   derivatives. That is acceptable for a source a user opts into, but it
         *   should not be what the app ships as its default.
         * * Its server is scheduled to expire on **2026-12-31**, so an endpoint
         *   the app cannot replace on its own must not be the one every install
         *   depends on.
         *
         * Its URL must **not** be folded into [GITHUB_HOSTS_MIRRORS]. The two
         * serve different documents under different licences; mixing the
         * endpoints into one fallback list would silently hand some users a
         * different document, with a different licence, than the source label
         * says they are on.
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
         * The built-in sources, always in this order: the default document, the
         * optional HelloGitHub document, then the retired `s302` row. The order is
         * the order the settings screen lists them, so the default has to be first
         * — it is what a fresh install is already on.
         */
        fun defaults(): List<RuleSource> = listOf(
            RuleSource(
                id = GITHUB_HOSTS_ID,
                label = "",
                labelRes = R.string.rules_source_default,
                url = GITHUB_HOSTS_URL,
                mirrors = GITHUB_HOSTS_MIRRORS,
                builtin = true,
            ),
            RuleSource(
                id = HELLOGITHUB_HOSTS_ID,
                label = "",
                labelRes = R.string.rules_source_hellogithub,
                url = HELLOGITHUB_HOSTS_URL,
                mirrors = HELLOGITHUB_HOSTS_MIRRORS,
                builtin = true,
            ),
            RuleSource(
                id = S302_ID,
                label = "",
                labelRes = R.string.rules_source_s302_retired,
                url = "",
                builtin = true,
                unavailable = true,
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
