package dev.detour.core

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject

/**
 * The rule document, as a tree the rules screen can walk.
 *
 * Parsed with `org.json`, which is part of the platform: the document is a few
 * hundred kilobytes of a very simple shape, and pulling in a serialisation
 * library for that would be more dependency than the problem deserves.
 *
 * Parsing happens on a background dispatcher and the result is immutable, so the
 * screen holds one object and filters it on every keystroke without touching the
 * file again.
 */
class RuleIndex private constructor(
    val groups: List<Node.Group>,
) {

    sealed interface Node {
        val key: String

        data class Group(
            val name: String,
            val domains: List<Domain>,
            val addressCount: Int,
        ) : Node {
            override val key: String get() = "group:$name"
            val domainCount: Int get() = domains.size
        }

        data class Domain(val domain: String, val addresses: List<String>) : Node {
            override val key: String get() = "domain:$domain"
        }

        /**
         * Only produced when a search matches an address directly.
         *
         * The key carries the owning domain, not just the address. One address
         * can be claimed by several domains — that is the normal case after a
         * merge — and a bare-address key would then be duplicated in the list.
         * `LazyColumn` treats a repeated key as a programming error and throws,
         * so the search that surfaced this row is what would crash the screen.
         */
        data class Address(val address: String, val domain: String) : Node {
            override val key: String get() = "address:$domain:$address"
        }
    }

    /**
     * The rows to show, flattened to what is actually visible right now.
     *
     * This replaces a `filter` that returned only groups. The tree has always had
     * three levels — a group holds domains, a domain holds addresses — but the
     * screen only ever rendered the first, so tapping a group flipped an arrow and
     * revealed nothing. Flattening here rather than nesting composables keeps the
     * screen a single `LazyColumn`: the row list *is* the expanded state, and
     * expanding a group is a change to this list, not to a nested layout.
     *
     * A disabled row still shows, greyed by its switch rather than hidden — hiding
     * it would make the switch impossible to find again.
     *
     * `key` stays unique because a domain is claimed by exactly one group in a
     * parsed document, so `domain:<name>` cannot repeat, and `address:<domain>:<ip>`
     * carries the owner for the same reason.
     */
    fun rows(
        query: String,
        expandedGroups: Set<String>,
        expandedDomains: Set<String>,
    ): List<Node> {
        val needle = query.trim().lowercase()
        val result = mutableListOf<Node>()

        // A group row, plus its domain rows when `children`, plus each expanded
        // domain's addresses. `domains` is what the row claims to hold, so a
        // search narrows it to the hits and the count line stays honest.
        fun appendGroup(group: Node.Group, domains: List<Node.Domain>, children: Boolean) {
            result += if (domains.size == group.domains.size) {
                group
            } else {
                // `addressCount` is a stored field, not a computed one, so it has
                // to be recomputed alongside a narrowed domain list — otherwise the
                // count line would report the whole group's addresses under a
                // search that matched two domains.
                group.copy(domains = domains, addressCount = domains.sumOf { it.addresses.size })
            }
            if (!children) return
            for (domain in domains) {
                result += domain
                if (domain.key in expandedDomains) {
                    for (address in domain.addresses) {
                        result += Node.Address(address, domain.domain)
                    }
                }
            }
        }

        for (group in groups) {
            if (needle.isEmpty()) {
                appendGroup(group, group.domains, children = group.name in expandedGroups)
                continue
            }

            if (group.name.lowercase().contains(needle)) {
                appendGroup(group, group.domains, children = group.name in expandedGroups)
                continue
            }

            val domainHits = group.domains.filter { it.domain.lowercase().contains(needle) }
            if (domainHits.isNotEmpty()) {
                // The matched domains *are* the answer to the query, so they are
                // shown without waiting for the group to be expanded; their
                // addresses stay behind each domain's own expander.
                appendGroup(group, domainHits, children = true)
                continue
            }

            // An address match is worth surfacing on its own: "which domain is this
            // address for" is the question that comes up when a rule serves the
            // wrong certificate. These rows are shown without their group, because
            // the answer is the domain, not the section it lives in.
            val addressHits = group.domains.flatMap { domain ->
                domain.addresses
                    .filter { it.contains(needle) }
                    .map { Node.Address(it, domain.domain) }
            }
            if (addressHits.isNotEmpty()) result += addressHits
        }
        return result
    }

    companion object {
        /** Parse the cached document. */
        fun parse(context: Context): RuleIndex = decode(RulesRepository.load(context))

        /** Drop the cache, fetch again, and parse. */
        fun refresh(context: Context): RuleIndex {
            RulesRepository.invalidate(context)
            return decode(RulesRepository.load(context, forceRefresh = true))
        }

        /**
         * Every address the rule set claims, de-duplicated.
         *
         * This is what the tunnel routes. Not `0.0.0.0/0`: the app relays a listed
         * set of domains, and claiming the whole device's traffic would make every
         * unlisted packet pay a round trip through userspace to be handed straight
         * back.
         *
         * Only literals. A `{Cloudflare}` placeholder is not an address, and the
         * domains that carry one are reached by the DNS the kernel observes rather
         * than by a route installed up front.
         */
        fun routedAddresses(context: Context): List<String> =
            runCatching { parse(context).addresses() }.getOrDefault(emptyList())

        private val IPV4 = Regex("""^\d{1,3}(\.\d{1,3}){3}$""")

        private fun RuleIndex.addresses(): List<String> {
            val seen = LinkedHashSet<String>()
            for (group in groups) {
                for (domain in group.domains) {
                    for (address in domain.addresses) {
                        if (IPV4.matches(address)) seen += address
                    }
                }
            }
            return seen.toList()
        }

        fun decode(document: ByteArray): RuleIndex {
            val root = JSONObject(String(document, Charsets.UTF_8))
            val groups = root.optJSONArray("groups") ?: JSONArray()
            val parsed = ArrayList<Node.Group>(groups.length())

            for (g in 0 until groups.length()) {
                val group = groups.getJSONObject(g)
                val entries = group.optJSONArray("entries") ?: JSONArray()

                // A domain can appear in more than one entry of the same group
                // after a merge. Unioning here keeps the screen honest: what it
                // shows is what the kernel will try.
                val byDomain = LinkedHashMap<String, MutableList<String>>()
                for (e in 0 until entries.length()) {
                    val entry = entries.getJSONObject(e)
                    val ips = entry.optJSONArray("ips") ?: JSONArray()
                    val addresses = (0 until ips.length()).mapNotNull { ips.optString(it) }
                    val domains = entry.optJSONArray("domains") ?: JSONArray()
                    for (d in 0 until domains.length()) {
                        val name = domains.optString(d)
                        if (name.isEmpty()) continue
                        val bucket = byDomain.getOrPut(name) { mutableListOf() }
                        for (address in addresses) {
                            if (address.isNotEmpty() && address !in bucket) bucket += address
                        }
                    }
                }

                val domains = byDomain.map { (domain, addresses) ->
                    Node.Domain(domain, addresses)
                }
                parsed += Node.Group(
                    name = group.optString("group", "未命名"),
                    domains = domains,
                    addressCount = domains.sumOf { it.addresses.size },
                )
            }

            return RuleIndex(parsed)
        }
    }
}
