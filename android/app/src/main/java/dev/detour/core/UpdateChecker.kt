package dev.detour.core

import java.io.ByteArrayOutputStream
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject

/**
 * Fetches a release manifest and decides whether a newer build exists.
 *
 * There is no update server yet, so the manifest URL is whatever the user
 * typed into settings. That makes every input suspect: the URL may be blank,
 * may point at an APK instead of a JSON manifest, may 404, or may return
 * HTML. None of those may crash the app and none of them may be mistaken for
 * "you are up to date" — hence [Result.Failed] as a first-class outcome.
 *
 * Deliberately has **no UI**: it returns a [Result] and the caller decides how
 * to render it.
 */
object UpdateChecker {

    /**
     * A manifest is a few hundred bytes. Without a ceiling, a user who pastes
     * an APK URL would pull the whole file into memory (tens of MB) before we
     * ever get to parse it, so the read is capped and oversized bodies are
     * rejected rather than buffered.
     */
    private const val MAX_BYTES = 64 * 1024

    // Shorter than RulesRepository.fetchText's 20s/90s: this is a tiny JSON
    // document, not a multi-thousand-line rules file.
    private const val CONNECT_TIMEOUT_MS = 15_000
    private const val READ_TIMEOUT_MS = 20_000

    /** One published build, as described by the manifest. */
    data class Release(
        val versionCode: Int,
        val versionName: String,
        val url: String,
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
     * @param currentVersionCode the installed build's `versionCode`.
     */
    fun check(url: String, currentVersionCode: Int): Result {
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
            val scheme = schemeOf(trimmed)
            if (scheme != "http" && scheme != "https") {
                return Result.Failed("地址不是 http/https")
            }

            val connection = (URL(trimmed).openConnection() as HttpURLConnection).apply {
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
                parse(body, currentVersionCode)
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

    private fun parse(body: String, currentVersionCode: Int): Result {
        val json = JSONObject(body)

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
            Result.Available(Release(versionCode, versionName, downloadUrl, notes))
        } else {
            Result.UpToDate
        }
    }
}
