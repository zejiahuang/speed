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
 * so a built-in's `url` / `labelRes` / `unavailable` follow the installed
 * version. That is deliberate: the default source has already moved once (off
 * the retired `abhuang` endpoints), and had the URL been persisted, every
 * existing install would have kept fetching a dead host with no way for an
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
    val builtin: Boolean = false,
    val unavailable: Boolean = false,
) {
    /** Whether this source can be selected and fetched. */
    val usable: Boolean get() = url.isNotEmpty() && !unavailable

    /**
     * What the cache sidecar records for this source.
     *
     * The URL, not the [id]: an id is a name the user can delete and re-add, and
     * two different URLs must never share a cache stamp. The URL is the document
     * itself, so it is the honest identity.
     */
    val identity: String get() = url

    companion object {
        const val GITHUB_HOSTS_ID = "github-hosts"
        const val S302_ID = "s302"

        /**
         * The default source: `maxiaof/github-hosts`, a plain hosts file with no
         * `# === [x] ===` sections. Measured: 1740 bytes, 37 domains, so it parses
         * to a single `hosts` group of 37 domains / 37 addresses.
         */
        private const val GITHUB_HOSTS_URL =
            "https://raw.githubusercontent.com/maxiaof/github-hosts/master/hosts"

        /**
         * A GitHub *page* URL names the same file as its raw URL, but only the raw
         * one returns the file. `github.com/<owner>/<repo>/blob/<path>` is what a
         * browser address bar holds, so it is what a person pastes; rewriting it is
         * the difference between a source that works and one that downloads HTML.
         * Anything else is returned trimmed, unjudged.
         */
        private val GITHUB_BLOB =
            Regex("""^https?://github\.com/([^/]+)/([^/]+)/blob/(.+)$""")

        /** The built-in sources, always in this order. */
        fun defaults(): List<RuleSource> = listOf(
            RuleSource(
                id = GITHUB_HOSTS_ID,
                label = "",
                labelRes = R.string.rules_source_default,
                url = GITHUB_HOSTS_URL,
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
