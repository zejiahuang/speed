package dev.detour.core

import android.os.Build
import java.io.ByteArrayOutputStream
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONArray
import org.json.JSONObject

/**
 * Fetches a release manifest and decides whether a newer build exists.
 *
 * The update source defaults to this project's own GitHub Releases (see
 * [DEFAULT_MANIFEST_URL]) and is served by the GitHub Releases API, so the body
 * is a GitHub release object rather than anything we control. A user may still
 * override the URL from settings and point it at the older hand-written
 * manifest format, so both shapes are accepted and told apart by their keys.
 *
 * Every input stays suspect: the URL may be blank, may point at an APK instead
 * of a JSON document, may 404, or may return HTML. None of those may crash the
 * app and none of them may be mistaken for "you are up to date" — hence
 * [Result.Failed] as a first-class outcome.
 *
 * Deliberately has **no UI**: it returns a [Result] and the caller decides how
 * to render it.
 */
object UpdateChecker {

    /**
     * Which stream of releases a check follows.
     *
     * **Two channels, and neither is a superset of the other.** GitHub defines
     * `/releases/latest` as the newest release that is neither a draft nor a
     * prerelease, so the stable channel is "everything except prereleases" and
     * the beta channel is "prereleases only". Neither contains the other, which
     * is why a channel cannot be expressed as a filter applied to one response —
     * it decides which endpoint gets asked.
     */
    enum class Channel {
        /** Stable releases only. */
        STABLE,

        /**
         * Prereleases only; never falls back to a stable release. See
         * `Prefs.updateChannel` for why the fallback is deliberately absent.
         */
        BETA,
    }

    /**
     * The mirror prefix shared by the manifest fetch and the APK download.
     *
     * **One constant, two hops, on purpose.** Checking for an update and
     * downloading it are two requests of the same transfer, and only the first
     * one used to go through the mirror. The second hop is the harder one: the
     * release page answers `github.com/.../releases/download/...` with a `302`
     * to `release-assets.githubusercontent.com`, which the rule document does
     * not carry, so an unmirrored download ends up on a host the tunnel only
     * ever dials directly. Measured through this prefix, the asset comes back
     * `206` with `application/vnd.android.package-archive`, and the client opens
     * a connection to the mirror alone — the redirect is followed server-side.
     * Deriving both URLs from one constant is what keeps them from drifting
     * apart into the state this replaces: the check working, the download not.
     */
    private const val MIRROR_PREFIX = "https://gh-proxy.com/"

    /**
     * The default update source: this project's GitHub Releases, reached through
     * the `gh-proxy.com` mirror rather than `api.github.com` directly.
     *
     * The proxy is not a preference, it is a reachability requirement. This
     * app's own traffic is deliberately kept *out* of its VPN tunnel —
     * `DetourVpnService` calls `addDisallowedApplication(packageName)` (see
     * `DetourVpnService.kt:262`) — so an update check leaves the device over the
     * raw network, exactly like an app with no proxy configured. We therefore
     * cannot assume `api.github.com` is reachable from wherever the user is, and
     * the direct endpoint is commonly blocked in mainland China. The mirror is
     * measured working from there (HTTP 200, byte-identical body), so it is the
     * default; a user who can reach GitHub directly can override the URL.
     *
     * The APK download is mirrored too, by [mirrorForDownload] — see
     * [MIRROR_PREFIX] for why one is not enough.
     *
     * It lives here rather than in `Prefs` on purpose: `Prefs` stores what the
     * *user* configured, and a compiled-in fallback is not a user setting. It
     * also belongs beside the parser that understands the GitHub release format,
     * so the default and the code that consumes it cannot drift apart.
     */
    const val DEFAULT_MANIFEST_URL =
        MIRROR_PREFIX + "https://api.github.com/repos/zejiahuang/speed/releases/latest"

    /**
     * The path [DEFAULT_MANIFEST_URL] ends with, and the only shape
     * [urlForChannel] knows how to rewrite for the beta channel.
     */
    private const val GITHUB_LATEST_PATH = "/releases/latest"

    /**
     * How many releases to ask for when listing. Five covers the newest
     * prerelease in any realistic situation while keeping the response well
     * under [MAX_BYTES]; the endpoint's default page is thirty, each with its
     * own Markdown body.
     */
    private const val BETA_PAGE_SIZE = 5

    /**
     * The address to request for [channel].
     *
     * **Why the beta channel needs a different address at all.** GitHub defines
     * `/releases/latest` as "the newest release that is neither a draft nor a
     * prerelease", so a prerelease can never appear in its response — asking the
     * same URL and filtering the answer would come back empty every time. The
     * list endpoint is the only one that carries prereleases.
     *
     * **A suffix rewrite, not a second compiled-in constant.** A user who
     * overrode `update_url` keeps their override on both channels, instead of
     * silently falling back to this project's own Releases the moment they pick
     * beta. A URL that does not end in `/releases/latest` is passed through
     * unchanged: it is not the GitHub shape this rewrite understands — a
     * hand-written manifest, or a mirror with its own layout — and inventing a
     * different address for it would turn a working source into a 404.
     */
    private fun urlForChannel(url: String, channel: Channel): String {
        if (channel == Channel.STABLE) return url
        val trimmed = url.trim()
        if (!trimmed.endsWith(GITHUB_LATEST_PATH)) return trimmed
        return trimmed.removeSuffix("/latest") + "?per_page=$BETA_PAGE_SIZE"
    }

    /**
     * A manifest is a few hundred bytes; a GitHub release body is a few KB.
     * Without a ceiling, a user who pastes an APK URL would pull the whole file
     * into memory (tens of MB) before we ever get to parse it, so the read is
     * capped and oversized bodies are rejected rather than buffered.
     *
     * **Raised from 64 KB when the beta channel was added.** That channel asks
     * the list endpoint, whose response is several release objects each carrying
     * a Markdown body — legitimately a few tens of KB where the single-release
     * endpoint is a few. The cap still does the job it was written for: what it
     * exists to stop is measured in megabytes, not kilobytes.
     */
    private const val MAX_BYTES = 256 * 1024

    // Shorter than RulesRepository.fetchText's 20s/90s: this is a tiny JSON
    // document, not a multi-thousand-line rules file.
    private const val CONNECT_TIMEOUT_MS = 15_000
    private const val READ_TIMEOUT_MS = 20_000

    /**
     * One published build.
     *
     * There is deliberately no `versionCode` here. GitHub releases carry only a
     * tag, so a numeric code is not always available, and the field was only
     * ever used for comparison, never shown. The caller displays [versionName]
     * and downloads [url]; nothing needs the integer.
     */
    data class Release(
        /** Shown to the user, e.g. "0.1.0". */
        val versionName: String,
        /** Direct APK download for this device's ABI, through the mirror; falls back to the release page. */
        val url: String,
        /** Release notes body as-is (GitHub `body`, raw Markdown), or `null`. */
        val notes: String?,
    )

    sealed interface Result {
        object UpToDate : Result
        data class Available(val release: Release) : Result
        data class Failed(val message: String) : Result
    }

    /**
     * Blocking network call; callers must not call this on the main thread.
     *
     * @param currentVersionCode the installed build's `versionCode`; used only
     *   by the custom manifest format, which still compares integers.
     * @param currentVersionName the installed build's `versionName` (e.g.
     *   "0.1.0"); used by the GitHub format, which has no integer to compare.
     * @param channel which release stream to ask for; see [Channel]. Defaults to
     *   [Channel.STABLE] so a caller with no channel concept still behaves
     *   exactly as it did before the channel existed.
     */
    fun check(
        url: String,
        currentVersionCode: Int,
        currentVersionName: String,
        channel: Channel = Channel.STABLE,
    ): Result {
        // Catch Throwable around the whole body: a malformed manifest, a
        // non-JSON body, a wrong-typed field or a socket error must all surface
        // as `Failed` rather than propagate. That is exactly why `Failed` is a
        // result type instead of an exception escaping to the caller.
        return try {
            val trimmed = url.trim()
            if (trimmed.isEmpty()) {
                return Result.Failed("未配置更新地址")
            }

            // Validate the scheme before touching URL(): `URL()` throws on an
            // unknown protocol (e.g. "ftp://", or a bare "example.com"), and a
            // throw here would be a crash-shaped failure where a plain message
            // is what the user needs.
            //
            // `http://` is refused here even though it parses fine, because it
            // cannot succeed on this app: targetSdk is 36 and the manifest sets
            // neither `usesCleartextTraffic` nor a `networkSecurityConfig`, so
            // the platform blocks cleartext before a socket is ever opened.
            // Measured on the emulator: an `http://` address got past this check
            // and surfaced as "Cleartext HTTP traffic to neverssl.com not
            // permitted" — an English platform string where the user needs a
            // Chinese one, arriving only after a pointless connection attempt.
            //
            // The alternative — adding `usesCleartextTraffic="true"` so that
            // `http://` works — is worse than the problem it solves: it would
            // permit cleartext for *every* request the app makes, including the
            // rule-source fetches that carry the whole routing table.
            val scheme = schemeOf(trimmed)
            if (scheme != "https") {
                return Result.Failed("系统不允许明文 HTTP，请改用 https://")
            }

            val target = urlForChannel(trimmed, channel)
            val connection = (URL(target).openConnection() as HttpURLConnection).apply {
                connectTimeout = CONNECT_TIMEOUT_MS
                readTimeout = READ_TIMEOUT_MS
                instanceFollowRedirects = true
            }
            try {
                // Same trap as RulesRepository.fetchText: a 404 here is an HTML
                // error page. Without the status check it fails to parse as a
                // manifest and could be read as "nothing newer", i.e. a
                // confident "up to date" for a URL that was simply wrong.
                val code = connection.responseCode
                if (code != HttpURLConnection.HTTP_OK) {
                    return Result.Failed("HTTP $code")
                }

                val body = readCapped(connection) ?: return Result.Failed("响应过大")
                parse(body, currentVersionCode, currentVersionName, channel)
            } finally {
                connection.disconnect()
            }
        } catch (err: Throwable) {
            Result.Failed(err.message ?: err.javaClass.simpleName)
        }
    }

    /**
     * The scheme without a `URL`/`URI` parse, so it can never throw on garbage.
     * Returns `null` when there is no `scheme://` prefix at all.
     */
    private fun schemeOf(url: String): String? {
        val separator = url.indexOf("://")
        if (separator <= 0) return null
        return url.substring(0, separator).lowercase()
    }

    /**
     * Reads the body as UTF-8, or returns `null` if it exceeds [MAX_BYTES].
     * Bails out as soon as the cap is crossed instead of buffering the whole
     * stream first.
     */
    private fun readCapped(connection: HttpURLConnection): String? {
        val out = ByteArrayOutputStream()
        val buffer = ByteArray(8 * 1024)
        connection.inputStream.use { input ->
            while (true) {
                val read = input.read(buffer)
                if (read < 0) break
                if (out.size() + read > MAX_BYTES) return null
                out.write(buffer, 0, read)
            }
        }
        return out.toString("UTF-8")
    }

    /**
     * Tells the two accepted shapes apart and dispatches.
     *
     * The GitHub format is tested first because the two are disjoint on the key
     * we look at: a GitHub release has `tag_name` and never has
     * `versionCode`/`url`, and a hand-written manifest is the other way round.
     * Testing GitHub first means a real GitHub body can never be misread as a
     * *broken* custom manifest — which would surface to the user as the
     * misleading "缺少 versionCode 或 url" for a perfectly valid release. A
     * custom manifest simply has no `tag_name` and falls through to the old
     * path unchanged.
     */
    private fun parse(
        body: String,
        currentVersionCode: Int,
        currentVersionName: String,
        channel: Channel,
    ): Result {
        // The beta channel's list endpoint answers with an array; every other
        // source answers with one object. Dispatch on the first non-whitespace
        // character rather than "try the object parser and catch": constructing a
        // `JSONObject` from an array throws, and that throw would reach the user
        // as a parse failure for a perfectly valid response.
        if (body.trimStart().startsWith("[")) {
            return parseGitHubList(JSONArray(body), currentVersionName, channel)
        }

        val json = JSONObject(body)

        val tagName = json.optString("tag_name", "")
        return if (tagName.isNotBlank()) {
            parseGitHub(json, tagName, currentVersionName, channel)
        } else {
            parseCustom(json, currentVersionCode)
        }
    }

    private fun parseCustom(json: JSONObject, currentVersionCode: Int): Result {
        // Only `versionCode` and `url` are required. `-1` is the sentinel for
        // "absent or not an integer": a real Android versionCode is always
        // positive, so it can never collide with it.
        val versionCode = json.optInt("versionCode", -1)
        val downloadUrl = json.optString("url", "")
        if (versionCode < 0 || downloadUrl.isBlank()) {
            return Result.Failed("缺少 versionCode 或 url")
        }

        val versionName = json.optString("versionName", "").ifBlank { "版本 $versionCode" }
        val notes = if (json.isNull("notes")) null else json.optString("notes", "").ifBlank { null }

        // Compare the integer `versionCode`, never the `versionName` string:
        // as strings, "0.10.0" sorts *before* "0.9.0", so a name comparison
        // would report a newer build as older. The project carries
        // `versionCode` precisely so there is a monotonic integer to compare.
        return if (versionCode > currentVersionCode) {
            Result.Available(Release(versionName, downloadUrl, notes))
        } else {
            Result.UpToDate
        }
    }

    /**
     * Parses a GitHub Releases API release object.
     *
     * @param tagName the already-read `tag_name`.
     */
    private fun parseGitHub(
        json: JSONObject,
        tagName: String,
        currentVersionName: String,
        channel: Channel,
    ): Result {
        // A draft is never offered on any channel: it is an unfinished build that
        // GitHub itself keeps out of every public listing. A prerelease is
        // offered on exactly one channel, so the test is an equality against what
        // the channel asked for rather than the single "reject prereleases"
        // branch this used to be — back when the only endpoint in use could not
        // return one, and the branch was accordingly documented as dead. It is
        // not dead now, and the two directions are not symmetric: on the stable
        // channel a prerelease must be refused, because offering it would hand a
        // test build to someone who never asked for one; on the beta channel a
        // *stable* release must be refused, because the user asked for
        // prereleases only.
        if (json.optBoolean("draft", false)) {
            return Result.UpToDate
        }
        if (json.optBoolean("prerelease", false) != (channel == Channel.BETA)) {
            return Result.UpToDate
        }

        val versionName = tagName.removePrefix("v").removePrefix("V")

        // A malformed version on *either* side is reported as Failed, never as
        // UpToDate. Coercing a bad tag into some default that compares as
        // "older" would dress a broken release up as "you are up to date" and
        // silently hide every future update until someone notices.
        val current = parseVersion(currentVersionName)
            ?: return Result.Failed("版本号无法解析：$currentVersionName")
        val remote = parseVersion(versionName)
            ?: return Result.Failed("版本号无法解析：$versionName")

        // Compare the parsed versions, never the strings: lexicographically
        // "0.10.0" sorts *before* "0.9.0", so a string compare would call a newer
        // release older and hide the update. `Version` additionally carries the
        // SemVer rule that a prerelease precedes the release it leads to, which
        // is what stops the beta channel from offering `0.2.4-beta.1` to a device
        // already running `0.2.4` — a downgrade dressed as an update.
        if (remote <= current) {
            return Result.UpToDate
        }

        // Both branches are mirrored: the asset URL and the release-page
        // fallback are the same GitHub host family, and the fallback is a
        // download link too — an unmirrored one would fail for the same reason
        // the asset URL would.
        val downloadUrl = mirrorForDownload(pickAssetUrl(json) ?: json.optString("html_url", ""))

        // `isNotBlank()` trims before testing, so a body of only whitespace
        // becomes `null` rather than an empty notes section. A real body is
        // returned verbatim, Markdown and all — stripping it is the display
        // layer's job, not this parser's.
        val notes = json.optString("body", "").takeIf { it.isNotBlank() }

        return Result.Available(Release(versionName, downloadUrl, notes))
    }

    /**
     * Picks the newest acceptable release out of a `/releases` array.
     *
     * **The array is scanned in full rather than taking the first entry.** The
     * list is ordered by creation time, not by version, so the newest *version*
     * is not necessarily the first *element* — a patch to an older line
     * published after a newer prerelease would sit in front of it. Comparing
     * every entry with the same [parseVersion] the single-object path uses is
     * also what keeps the two shapes from disagreeing about which of two
     * versions is newer.
     *
     * **An unparseable entry is skipped, not fatal.** A list is a set of
     * independent releases, and one mislabelled tag among them must not hide the
     * rest. The single-object path deliberately does the opposite — see its note
     * — because there the one bad tag *is* the whole response.
     */
    private fun parseGitHubList(array: JSONArray, currentVersionName: String, channel: Channel): Result {
        var best: JSONObject? = null
        var bestVersion: Version? = null

        for (i in 0 until array.length()) {
            val item = array.optJSONObject(i) ?: continue
            if (item.optBoolean("draft", false)) continue
            if (item.optBoolean("prerelease", false) != (channel == Channel.BETA)) continue

            val tag = item.optString("tag_name", "").removePrefix("v").removePrefix("V")
            val version = parseVersion(tag) ?: continue

            val previous = bestVersion
            if (previous == null || version > previous) {
                best = item
                bestVersion = version
            }
        }

        val winner = best ?: return Result.UpToDate
        // Hand the winner to the single-object path instead of repeating its work
        // here, so both shapes share one implementation of the version
        // comparison, the mirror rewrite and the notes handling. The channel
        // checks are re-run inside and pass, since this loop applied the same
        // test to the same entry.
        return parseGitHub(winner, winner.optString("tag_name", ""), currentVersionName, channel)
    }

    /**
     * Picks the asset that best fits this device, or `null` when none matches.
     *
     * `Build.SUPPORTED_ABIS` is ordered by preference — the device's primary ABI
     * comes first — so scanning it in that order and taking the first asset
     * whose name ends in `-<abi>.apk` means "the package built for the ABI this
     * phone would rather run". The suffix is matched case-insensitively because
     * asset names are typed in CI and `ARM64-V8A` and `arm64-v8a` mean the same
     * thing. When nothing matches we return `null` so the caller falls back to
     * `html_url` (the release page): the user can still download by hand,
     * whereas handing back a guess at a wrong-ABI APK is worse. In practice the
     * two APKs share a signing key, so even a mis-pick would only waste a 10 MB
     * download, not install a broken app — but we do not rely on that.
     */
    private fun pickAssetUrl(json: JSONObject): String? {
        val assets = json.optJSONArray("assets") ?: return null
        for (abi in Build.SUPPORTED_ABIS) {
            val suffix = "-$abi.apk"
            for (i in 0 until assets.length()) {
                val asset = assets.optJSONObject(i) ?: continue
                val name = asset.optString("name", "")
                if (name.endsWith(suffix, ignoreCase = true)) {
                    val url = asset.optString("browser_download_url", "")
                    if (url.isNotBlank()) return url
                }
            }
        }
        return null
    }

    /**
     * Rewrites a GitHub download URL so it goes through [MIRROR_PREFIX].
     *
     * **Why the download needs the mirror even though the manifest has it.** The
     * manifest reaching GitHub says nothing about the asset reaching it. The
     * release page answers `github.com/.../releases/download/...` with a `302`
     * to `release-assets.githubusercontent.com`, and that host is not in the
     * rule document, so the tunnel plans the flow as a direct dial of the
     * client's own address (`Planner::decide`, step 4) — the same unprotected
     * path the manifest would have taken. Measured through the mirror, the asset
     * comes back `206` with `application/vnd.android.package-archive` and a
     * `PK\x03\x04` body, and the client opens a connection to the mirror alone.
     * So the mirror is not an extra fallback here, it removes a hop that could
     * not have worked.
     *
     * Two guards, both about not breaking what the mirror is not for:
     *
     * - **Idempotent.** A URL that already carries the prefix is returned
     *   unchanged. A manifest handing back an already-mirrored URL — or a caller
     *   that starts mirroring earlier in the chain — would otherwise get a
     *   doubled prefix, which is a 404, not a download.
     * - **GitHub hosts only.** The GitHub branch of [parse] trusts the *shape*
     *   of the body, not its origin: `update_url` is user-settable, so a
     *   manifest of the user's own may carry `tag_name` and a download link to
     *   their own server. Routing that through a third party would be a silent
     *   detour of somebody else's file. Anything that is not a GitHub host is
     *   left exactly as given.
     */
    private fun mirrorForDownload(url: String): String {
        val target = url.trim()
        if (target.isEmpty()) return target
        if (target.startsWith(MIRROR_PREFIX)) return target
        if (!isGitHubHost(target)) return target
        return MIRROR_PREFIX + target
    }

    /**
     * Whether [url]'s host is one the mirror exists to reach.
     *
     * Matched on the **host** and never on a substring of the whole URL: a
     * `contains("github.com")` test also fires for
     * `https://example.com/?ref=github.com` and would reroute it. The userinfo
     * and port are stripped first so `https://user@github.com:443/x` still
     * matches. `.github.com` covers the API and `codeload`;
     * `.githubusercontent.com` covers the release-asset, raw and objects hosts —
     * the family the redirect actually lands in.
     */
    private fun isGitHubHost(url: String): Boolean {
        val afterScheme = url.substringAfter("://", "")
        if (afterScheme.isEmpty()) return false
        val host = afterScheme
            .substringBefore('/')
            .substringBefore('?')
            .substringBefore('#')
            .substringAfterLast('@')
            .substringBefore(':')
            .lowercase()
        return host == "github.com" || host.endsWith(".github.com") ||
            host == "githubusercontent.com" || host.endsWith(".githubusercontent.com")
    }
}
