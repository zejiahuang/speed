package dev.detour.core

import android.content.Context
import dev.detour.R
import java.security.MessageDigest

/**
 * The disclaimer the app shows before it will do anything, and the identity of
 * the text the user agreed to.
 *
 * The text is not written here. It is `DISCLAIMER.md` at the repository root,
 * copied into `res/raw` by the `syncDisclaimer` task at build time, so the app
 * shows byte-for-byte what the repository publishes and there is no second copy
 * to keep in step. That is the whole reason this is a resource read rather than a
 * constant: a legal notice maintained in two places will disagree with itself,
 * and the copy that gets forgotten is always the one a user is shown.
 *
 * ## Why acceptance is keyed on a digest rather than a boolean
 *
 * "Has the user accepted?" is the wrong question, because a stored `true` outlives
 * the text it was given for. Change the disclaimer and a boolean still says yes —
 * the app would keep asserting consent to a document nobody has seen. So what is
 * stored is the **digest of the exact text** that was accepted, and the test is
 * equality against the text in this build. Any edit re-asks, which is the correct
 * behaviour for consent and the reason the digest is over the whole file rather
 * than over a hand-kept version number that could be left unbumped.
 *
 * The digest is an identity, **not** a security control: it is never compared
 * against a value from outside the app, so there is no adversary it needs to
 * resist. It is a hex string in `SharedPreferences` next to the other settings.
 */
object Disclaimer {

    /**
     * The bundled text, or `null` if the resource cannot be read.
     *
     * A missing `R.raw.disclaimer` means the build did not run `syncDisclaimer`,
     * which is a build defect rather than a runtime condition. It is still
     * returned as `null` instead of thrown so that a broken build degrades into
     * "no disclaimer shown" rather than a crash on every launch.
     */
    fun text(context: Context): String? = runCatching {
        context.resources.openRawResource(R.raw.disclaimer)
            .use { it.readBytes().toString(Charsets.UTF_8) }
    }.getOrNull()

    /** SHA-256 of [text], lowercase hex. See this object's note on what it is for. */
    fun digest(text: String): String =
        MessageDigest.getInstance("SHA-256")
            .digest(text.toByteArray(Charsets.UTF_8))
            .joinToString(separator = "") { "%02x".format(it) }

    /**
     * The document's own first-level heading, and everything after it.
     *
     * The dialog needs a title, and the document already has one — its opening `#`
     * line. Spelling those words a second time in `strings.xml` is precisely the
     * duplication this project keeps refusing to keep: edit the document's heading
     * and the window would go on announcing the old one. Reading the title out of
     * the document has a second effect that a separate title string cannot have:
     * the heading is drawn once instead of twice, once as the dialog's title and
     * again as the body's first line.
     *
     * A document with no opening `#` line yields an empty title and the whole text
     * as its body, so a malformed document degrades to an untitled dialog rather
     * than to no dialog — the caller supplies the fallback title.
     */
    fun titleAndBody(text: String): Pair<String, String> {
        val lines = text.lines()
        val first = lines.indexOfFirst { it.isNotBlank() }
        if (first < 0 || !lines[first].trimStart().startsWith("# ")) return "" to text
        val title = lines[first].trimStart().removePrefix("#").trim()
        val body = lines.filterIndexed { index, _ -> index != first }
            .joinToString("\n")
            .trim('\n')
        return title to body
    }

    /**
     * Whether [text] — this build's bundled disclaimer, as read by [text] — has
     * already been accepted.
     *
     * **The text is a parameter rather than read here**, even though reading it is
     * what [text] does, because the two callers have different tolerances for the
     * read. `DetourApp` asks once per process and does not care. The Compose
     * caller asks on every recomposition of the app body, and re-reading a ~10 KB
     * raw resource on each one is work with no result; it reads once into
     * `remember` and passes the value in. Keeping the *rule* here while letting
     * the caller own the *read* is what stops the rule being restated at the call
     * site, where it would drift.
     *
     * **Unreadable text counts as accepted, and that is deliberate.** The only way
     * to reach that branch is a build whose resource is missing, and the
     * alternative — reporting "not accepted" — would put a dialog on screen that
     * cannot draw its own body and cannot be closed for ten seconds. Turning a
     * packaging mistake into an unusable app is worse than not showing a notice
     * that could not have been shown anyway, so this returns `true` and
     * [dev.detour.DetourApp] logs the failure loudly.
     */
    fun isAccepted(text: String?, prefs: Prefs): Boolean {
        if (text == null) return true
        return prefs.disclaimerAcceptedDigest == digest(text)
    }
}
