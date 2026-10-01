package dev.detour.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The ordering rules the update check compares releases with.
 *
 * **Why these are unit tests and not a device measurement.** Everything else in
 * the update path is a network behaviour and is verified on the emulator against
 * a real server. `Version.compareTo` is the exception: it is pure, it has no
 * Android dependency, and the cases that matter — `beta.10` against `beta.9`,
 * a release against its own prerelease — cannot be produced on demand by any
 * real repository. Reaching them through a live request would mean the coverage
 * depended on which tags happen to exist, which is not coverage.
 *
 * Each test below names the rule it pins, and several of them exist because the
 * obvious implementation gets the rule wrong.
 */
class VersionTest {

    private fun v(value: String): Version =
        parseVersion(value) ?: error("fixture is not parseable: $value")

    /** Asserts the ordering in both directions, so a sign error cannot pass. */
    private fun assertNewer(newer: String, older: String) {
        assertTrue("$newer should sort after $older", v(newer) > v(older))
        assertTrue("$older should sort before $newer", v(older) < v(newer))
    }

    @Test
    fun higherPatchIsNewer() {
        assertNewer("0.2.4", "0.2.3")
    }

    @Test
    fun higherMinorIsNewer() {
        assertNewer("0.3.0", "0.2.9")
    }

    @Test
    fun higherMajorIsNewer() {
        assertNewer("1.0.0", "0.99.99")
    }

    @Test
    fun releasePartsCompareNumericallyNotAsStrings() {
        // The reason `Version` parses integers at all: as strings "0.10.0" sorts
        // *before* "0.9.0", which would hide an update rather than show it.
        assertNewer("0.10.0", "0.9.0")
        assertNewer("0.2.10", "0.2.9")
    }

    @Test
    fun identicalVersionsCompareEqual() {
        assertEquals(0, v("0.2.3").compareTo(v("0.2.3")))
    }

    @Test
    fun aReleaseIsNewerThanItsOwnPrerelease() {
        // SemVer: a prerelease precedes the release it leads to. Without this the
        // beta channel would offer 0.2.4-beta.1 to a device already on 0.2.4, and
        // the "update" would be a downgrade.
        assertNewer("0.2.4", "0.2.4-beta.1")
    }

    @Test
    fun aLaterPrereleaseIsNewer() {
        assertNewer("0.2.4-beta.2", "0.2.4-beta.1")
    }

    @Test
    fun prereleaseNumbersCompareNumerically() {
        // "beta.10" against "beta.9": a string compare calls beta.10 the smaller.
        assertNewer("0.2.4-beta.10", "0.2.4-beta.9")
    }

    @Test
    fun alphanumericPrereleaseOutranksNumericOne() {
        // SemVer: numeric identifiers rank below alphanumeric ones.
        assertNewer("0.2.4-beta", "0.2.4-1")
    }

    @Test
    fun prereleaseIdentifiersCompareAlphabetically() {
        assertNewer("0.2.4-rc", "0.2.4-beta")
    }

    @Test
    fun longerPrereleaseIsNewerWhenTheSharedIdentifiersMatch() {
        assertNewer("0.2.4-beta.1", "0.2.4-beta")
    }

    @Test
    fun theReleaseNumbersDecideBeforeThePrereleaseRuleApplies() {
        // The prerelease ranking only applies once the three numbers are equal;
        // a higher number wins even when it carries a prerelease suffix.
        assertNewer("0.2.5-beta.1", "0.2.4")
    }

    @Test
    fun buildMetadataDoesNotAffectOrdering() {
        // SemVer defines build metadata as not participating in precedence.
        assertEquals(0, v("0.2.3+build1").compareTo(v("0.2.3+build2")))
    }

    @Test
    fun buildMetadataIsStrippedFromAPrerelease() {
        assertNewer("0.2.4-beta.2+x", "0.2.4-beta.1+x")
    }

    @Test
    fun aPrereleaseSuffixNoLongerFailsToParse() {
        // Before the beta channel existed, a tag like this parsed to null, so a
        // prerelease could only ever surface to the user as "版本号无法解析" — a
        // message that reads as a broken manifest rather than as the channel
        // having nothing to offer.
        assertEquals(listOf("beta", "1"), v("0.2.4-beta.1").pre)
    }

    @Test
    fun twoPartVersionIsRejected() {
        assertNull(parseVersion("1.2"))
    }

    @Test
    fun leadingVPrefixMustBeRemovedByTheCaller() {
        // `parseVersion` is given the tag with the prefix already stripped; a
        // bare "v0.2.3" is not a version and must not be silently accepted.
        assertNull(parseVersion("v0.2.3"))
    }

    @Test
    fun nonNumericReleasePartIsRejected() {
        assertNull(parseVersion("0.2.x"))
    }

    @Test
    fun negativeReleasePartIsRejected() {
        assertNull(parseVersion("0.-1.0"))
    }

    @Test
    fun emptyValueIsRejected() {
        assertNull(parseVersion(""))
    }

    @Test
    fun aDateShapedTagParsesAsAVersion() {
        // Recorded rather than recommended: the parser accepts any three
        // dot-separated integers, so a date-shaped tag is compared as major=2026
        // and would beat every real version. Kept as a test so that tightening
        // the rule later is a deliberate change instead of a surprise.
        assertEquals(Version(2026, 10, 1, emptyList()), v("2026.10.01"))
    }
}
