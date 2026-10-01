package dev.detour.core

/**
 * Release versions, and the ordering the update check compares them with.
 *
 * **Why this is its own file rather than two private members of
 * `UpdateChecker`.** The ordering rules below are the one part of the update
 * path that is pure logic — no sockets, no JSON, no Android — and they are also
 * the part with the most ways to be subtly wrong: SemVer precedence is not
 * "compare the numbers", and every deviation from it shows up as a user either
 * being offered a downgrade or never being told about an upgrade. Sitting next
 * to the network code, they were reachable only through a real request against a
 * real server, which is no way to test `beta.10` against `beta.9`. Here they are
 * `internal` and covered directly by `VersionTest`.
 */

/**
 * A parsed version: three release numbers plus the prerelease identifiers.
 *
 * Ordered by the SemVer 2.0.0 precedence rules, which are not "compare the
 * numbers and stop": `0.2.4-beta.1` is *older* than `0.2.4`, because a
 * prerelease is defined to precede the release it leads to. Without that rule
 * the beta channel would offer `0.2.4-beta.1` to a device already on `0.2.4`,
 * and the "update" would be a downgrade.
 */
internal data class Version(
    val major: Int,
    val minor: Int,
    val patch: Int,
    val pre: List<String>,
) : Comparable<Version> {
    override fun compareTo(other: Version): Int {
        if (major != other.major) return major.compareTo(other.major)
        if (minor != other.minor) return minor.compareTo(other.minor)
        if (patch != other.patch) return patch.compareTo(other.patch)

        // Having no identifiers outranks having any: 0.2.4 > 0.2.4-beta.1.
        // Both-empty and exactly-one-empty fall out of the same subtraction,
        // which is why this is not split into two branches.
        if (pre.isEmpty() || other.pre.isEmpty()) {
            return other.pre.size - pre.size
        }

        for (i in 0 until minOf(pre.size, other.pre.size)) {
            val a = pre[i]
            val b = other.pre[i]
            val aNumber = a.toIntOrNull()
            val bNumber = b.toIntOrNull()
            val verdict = when {
                // Numeric identifiers compare numerically, so beta.10 sorts after
                // beta.9 — a string compare says the opposite.
                aNumber != null && bNumber != null -> aNumber.compareTo(bNumber)
                // A numeric identifier always ranks below an alphanumeric one.
                aNumber != null -> -1
                bNumber != null -> 1
                else -> a.compareTo(b)
            }
            if (verdict != 0) return verdict
        }

        // Every shared identifier is equal, so the longer set is the greater.
        return pre.size - other.pre.size
    }
}

/**
 * Parses a version tag into a [Version], or returns `null` when the value is not
 * a version at all.
 *
 * The release part must be exactly three dot-separated non-negative integers, so
 * a `v`-less tag like "1.2" or a date tag is still rejected here and the caller
 * turns that into a failure rather than guessing. What changed when the beta
 * channel was added is the suffix: `1.0.0-rc1` used to be rejected outright,
 * which meant a prerelease tag could only ever surface as "版本号无法解析" — a
 * message that reads as a broken manifest rather than as the channel having
 * nothing to offer.
 *
 * Build metadata (`+…`) is stripped and never carried into [Version]: SemVer
 * defines it as not participating in precedence, so two versions differing only
 * there compare equal, which is what the caller needs.
 */
internal fun parseVersion(value: String): Version? {
    val withoutBuild = value.trim().substringBefore('+')
    val core = withoutBuild.substringBefore('-')
    val pre = withoutBuild.substringAfter('-', "")
        .split('.')
        .filter { it.isNotEmpty() }

    val parts = core.split('.')
    if (parts.size != 3) return null
    val numbers = parts.map { it.toIntOrNull() ?: return null }
    if (numbers.any { it < 0 }) return null
    return Version(numbers[0], numbers[1], numbers[2], pre)
}
