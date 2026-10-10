package dev.detour.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Which source each mode loads, and the two properties of the pinned one that make
 * it a pin rather than a default.
 *
 * **Why this is a unit test and not a device measurement.** [RuleSource.forMode] is
 * a pure function of an enum and a value, so nothing about it needs an emulator. The
 * behaviour it decides, however, is only observable on a device: whether the pinned
 * document's names actually reach an origin is a network question and is measured
 * separately. What is testable here is the *routing* — that root mode ignores the
 * selection, that the other two modes do not, and that the pinned source cannot be
 * selected by accident.
 *
 * The last of those is the one worth a test. The pinned source is deliberately
 * absent from [RuleSource.defaults], and [Prefs.selectedSource] resolves the stored
 * id against that list — so the absence is not a cosmetic choice, it is what makes
 * the source unreachable from the settings screen and from `set rule_source`. If
 * someone later adds it to `defaults()` for convenience, root mode would keep
 * working and the *other* modes would silently gain a source whose behaviour there
 * has never been measured. That failure is invisible on screen, which is why it is
 * pinned here.
 */
class RuleSourceModeTest {

    private fun helloGithub(): RuleSource =
        RuleSource.defaults().first { it.id == RuleSource.HELLOGITHUB_HOSTS_ID }

    @Test
    fun rootModeIgnoresTheSelection() {
        val pinned = RuleSource.forMode(KernelState.Mode.ROOT, helloGithub())
        assertEquals(RuleSource.S302_UPSTREAM_ID, pinned.id)
        assertFalse(
            "root mode must not fall through to the selected source",
            pinned.id == RuleSource.HELLOGITHUB_HOSTS_ID,
        )
    }

    @Test
    fun theOtherTwoModesUseTheSelection() {
        // Both are asserted, not just one: a `when` that forgot a branch and fell
        // into the pinned case would pass a test that only checked VPN.
        for (mode in listOf(KernelState.Mode.VPN, KernelState.Mode.PROXY)) {
            val chosen = helloGithub()
            val used = RuleSource.forMode(mode, chosen)
            assertEquals("$mode must use the selection", RuleSource.HELLOGITHUB_HOSTS_ID, used.id)
            assertEquals("$mode must not rewrite the source", chosen.url, used.url)
        }
    }

    @Test
    fun thePinnedSourceIsNotInTheSelectableList() {
        // See the class comment: this is the property, not the id.
        assertTrue(
            "the pinned source must not be offered as a choice",
            RuleSource.defaults().none { it.id == RuleSource.S302_UPSTREAM_ID },
        )
    }

    @Test
    fun thePinnedSourceIsUsableAndBuiltIn() {
        val pinned = RuleSource.rootSource()
        // Usable: [RulesRepository.fetch] refuses a source that is not, and a
        // refusal here would be a mode that cannot start.
        assertTrue(pinned.usable)
        assertFalse("a pinned source is not a file on this device", pinned.isLocal)
        // Built-in, which is what makes [RulesRepository.parseDocument] apply the
        // entry floor for a document whose size is known.
        assertTrue(pinned.builtin)
        // `builtin` is also what `RuleSource.encode` filters on (`if (source.builtin)
        // continue`), so a pinned source cannot be frozen into storage by a future
        // `Prefs` edit. That consequence is **not** asserted here, and the reason is
        // a limitation rather than an oversight: `encode` builds a `JSONArray`, and
        // `org.json` on the JVM unit-test classpath is AGP's stub, so the call
        // throws `Method toString in org.json.JSONArray not mocked` instead of
        // returning. Adding a real `org.json` jar as a test dependency would assert
        // against a *different implementation* than the one the app runs — Android's
        // — which is the shape of test this project rejects: one that passes
        // without covering the thing it names.
    }

    @Test
    fun thePinnedSourcePointsAtTheUpstreamArtifact() {
        // The *path* is asserted rather than the whole URL: the host is this
        // deployment's and may move, while `/3` is the artifact the pin is about —
        // the document whose first field is a substitute upstream name. Pinning the
        // full URL would make a host change look like a behaviour change.
        assertTrue(
            "expected the /3 artifact, got ${RuleSource.rootSource().url}",
            RuleSource.rootSource().url.endsWith("/3"),
        )
    }

    @Test
    fun aPinnedSourceCarriesNoMirrors() {
        // Stated rather than left implicit: the two built-in documents each have a
        // fallback endpoint because they are mirrored elsewhere. This one is served
        // by one host, so a mirror list here would be an invented URL that only
        // surfaces as an extra timeout when the real one is down.
        assertTrue(RuleSource.rootSource().mirrors.isEmpty())
    }
}
