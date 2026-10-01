package dev.detour.core

import android.content.Context
import dev.detour.BuildConfig
import java.time.OffsetDateTime
import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject

/**
 * Export and import the portable settings, as one JSON document.
 *
 * This file is the **single source of truth for "what is a portable setting"**.
 * Both directions iterate the same [ENTRIES] list, so the set that is written
 * out and the set that is read back cannot drift: adding a setting here adds it
 * to both, and there is no second list to forget to update.
 *
 * ## What is portable, and what is not
 *
 * The membership rule is deliberately not a hand-written list. It is:
 *
 * > the set of properties that [Prefs.restoreDefaults] resets, **minus**
 * > `wallpaper`, **minus** `lastUpdateCheckAt`, **plus** `updateUrl`.
 *
 * [Prefs.restoreDefaults] already defines what this codebase considers a
 * "setting" — it is the function a "restore defaults" button calls — so reusing
 * its set keeps this document from inventing a second, narrower definition of a
 * setting that would silently omit something. The two keys it deliberately does
 * not reset are excluded here for the same reasons it excludes them:
 *
 * * `applied_kernel_settings` records what the *running* engine was built from.
 *   That is runtime state, not a preference: it is the input to the "内核参数已修改
 *   / 应用并重连" banner, and importing someone else's copy would make a real pending
 *   change invisible or announce one that does not exist. [Prefs]' own KDoc says
 *   so; this file only obeys it.
 * * `mode` is the tunnel kind. Under `BuildFlags.TUN_ONLY` the picker that would
 *   change it is hidden, so a document that carried a mode could set a build to a
 *   mode its UI cannot show or undo — a state the user has no control left to
 *   leave. The app's own restore refuses it for that reason.
 *
 * `disclaimer_accepted_digest` is excluded for a third reason, and it is the only
 * key here that is excluded without [Prefs.restoreDefaults] touching it at all:
 * it records that *this person on this device* accepted the disclaimer, which is
 * neither a preference nor a fact about the device's configuration. Carrying it in
 * a file would let one device assert another device's consent, and a document that
 * travels is exactly the wrong shape for a consent record. Because the membership
 * rule above is "what [restoreDefaults] resets", keeping it out of that function
 * keeps it out of this file with no second list to maintain — see
 * `Prefs.disclaimerAcceptedDigest`.
 *
 * `wallpaper` is reset by [Prefs.restoreDefaults] but is excluded here because
 * its value is the SHA-256 identity of an image file in `filesDir`, and this
 * document does not carry the image. Importing the identity without the bytes
 * would set a preference whose backing file is absent, which is exactly the
 * state `WallpaperStore` was built to degrade away from — so the honest move is
 * not to carry the key at all rather than to carry a reference that cannot
 * resolve on the importing device.
 *
 * `lastUpdateCheckAt` is the second exclusion, and it is excluded for the
 * opposite reason to the pair above. [Prefs.restoreDefaults] *does* reset it —
 * a restored device should be able to check at once rather than inherit the
 * previous owner's throttle window — but that reset is a local action, and
 * portability is not local. The value answers "when did **this** device last
 * try", so importing it would make the importing device wait out the exporting
 * device's remaining throttle: its first startup check would be silently skipped
 * for up to 24 hours, which reads exactly like auto-check being broken. A fact
 * about one device does not travel inside a file of another device's
 * preferences, so the key is not written.
 *
 * `updateUrl` is the one addition: it is a setting that [Prefs.restoreDefaults]
 * resets, so it is in the reset set too, and it is added explicitly here so the
 * export set stays readable against the rule rather than being inferred. The
 * About page no longer displays it (see `AboutScreen`), and that does not change
 * the rule: membership here is decided by [Prefs.restoreDefaults], not by whether
 * some screen currently shows the value. Dropping it from the file because its
 * row was deleted would silently lose a fork-user's mirror on the next import.
 *
 * ## Why the JSON keys are spelled out
 *
 * The keys inside `settings` are the `SharedPreferences` key strings, not the
 * Kotlin property names — `proxy_port`, `glass_blur`, `rule_source_id`. The
 * `Prefs` key constants are private to [Prefs], so the strings are written out
 * here. That is not duplication to be factored away: this file *is* the wire
 * format, and a key string here is a format decision that must survive the
 * constant being renamed. The comment on [Prefs]' own version note makes the same
 * point in the other direction.
 */
object SettingsBackup {

    /**
     * The `format` field, and the only value [import] accepts.
     *
     * A constant rather than the class name so the document stays readable if the
     * package or the class is ever renamed — the string is the contract, not the
     * identifier that happens to produce it.
     */
    const val FORMAT = "detour.settings"

    /**
     * One portable setting: the key it is stored under, and the two one-liners
     * that move it between a [Prefs] and the document.
     *
     * [write] always writes; [read] applies only when the document has the key, so
     * an absent key leaves the current value alone rather than overwriting it with
     * a default. That is what makes a partial document a partial restore instead
     * of a destructive one.
     */
    private class Entry(
        val key: String,
        val write: (JSONObject, Prefs) -> Unit,
        val read: (JSONObject, Prefs) -> Unit,
    )

    private fun bool(key: String, get: (Prefs) -> Boolean, set: (Prefs, Boolean) -> Unit) =
        Entry(key, { o, p -> o.put(key, get(p)) }, { o, p -> if (o.has(key)) set(p, o.getBoolean(key)) })

    private fun int(key: String, get: (Prefs) -> Int, set: (Prefs, Int) -> Unit) =
        Entry(key, { o, p -> o.put(key, get(p)) }, { o, p -> if (o.has(key)) set(p, o.getInt(key)) })

    // `org.json` has no `getFloat` — `JSONObject` offers `getDouble` and nothing
    // narrower — so a Float crosses the wire as a JSON number and comes back
    // through `getDouble`. Narrowing back to Float is exact: the value written
    // was a Float in the first place, so the double is that same number widened
    // and nothing was lost on the way out.
    //
    // The write side deliberately passes the Float itself instead of
    // `get(p).toDouble()`. Android's `numberToString` falls back to
    // `Number.toString()` whenever the value is not integral, so a Float prints
    // as "1.1" while the widened double would print as "1.100000023841858" — the
    // same number, spelled in a way that makes a hand-read export look broken.
    private fun float(key: String, get: (Prefs) -> Float, set: (Prefs, Float) -> Unit) =
        Entry(key, { o, p -> o.put(key, get(p)) }, { o, p -> if (o.has(key)) set(p, o.getDouble(key).toFloat()) })

    private fun string(key: String, get: (Prefs) -> String, set: (Prefs, String) -> Unit) =
        Entry(key, { o, p -> o.put(key, get(p)) }, { o, p -> if (o.has(key)) set(p, o.getString(key)) })

    /**
     * The merged glass switch. The one entry whose read side understands a
     * **previous** key spelling.
     *
     * Until 2026-10-01 the material had two switches, exported as `glass_liquid`
     * and `glass_frost`. A document written before then names those and not
     * `glass_enabled`, so [Entry]'s usual "an absent key leaves the value alone"
     * rule would turn a full restore into a partial one: everything would come
     * back except the glass the user had chosen. The read therefore accepts either
     * spelling, and "either half was on" is the translation — the same one
     * `Prefs.migrate` version 9 makes, for the same reason.
     *
     * The write side emits only the new key, so the two old names are read-only
     * here and age out as pre-merge documents stop being imported. They are
     * spelled out rather than shared with `Prefs`' constants for the reason the
     * class comment gives: these strings are the wire format, and this file is
     * where the format is decided.
     */
    private val glassEnabled: Entry = Entry(
        "glass_enabled",
        { o, p -> o.put("glass_enabled", p.glassEnabled) },
        { o, p ->
            when {
                o.has("glass_enabled") -> p.updateGlassEnabled(o.getBoolean("glass_enabled"))
                o.has("glass_liquid") || o.has("glass_frost") ->
                    p.updateGlassEnabled(o.optBoolean("glass_liquid") || o.optBoolean("glass_frost"))
            }
        },
    )

    /**
     * `disabled_rules` is a `Set<String>` in prefs, so it crosses the wire as a
     * JSON array of strings — a real array, not the `StringSet`'s `toString`,
     * which is not JSON and could not be read back.
     *
     * The read goes through [Prefs.replaceDisabledRules] rather than touching the
     * store, and that mutator is the only one that can replace the whole set at
     * once — which is what an import needs, since the document is the complete
     * list and not a delta.
     */
    private val disabledRules: Entry = Entry(
        "disabled_rules",
        { o, p -> o.put("disabled_rules", JSONArray(p.disabledRules.toList())) },
        { o, p ->
            if (o.has("disabled_rules")) {
                val array = o.getJSONArray("disabled_rules")
                val keys = (0 until array.length()).mapTo(mutableSetOf()) { array.getString(it) }
                p.replaceDisabledRules(keys)
            }
        },
    )

    /**
     * `rule_sources` is a JSON array **stored as a String** in prefs. It is
     * exported as a real nested array so the file is human-readable, and that is
     * done by round-tripping through [RuleSource]'s own encoder rather than
     * hand-building the objects: `JSONArray(RuleSource.encode(...))` parses the
     * exact string the store holds, so the document and the store agree by
     * construction. Writing a second serializer here is the thing [RuleSource]'s
     * class comment exists to prevent.
     *
     * Built-ins are not in the encoded string — [RuleSource.encode] skips them
     * because they are defined in code, not storage — so they are not in the
     * document either, which is correct: their URLs must follow the installed
     * build, not the file.
     *
     * The import decodes the array back to a list and re-encodes it, so a
     * malformed entry is dropped by [RuleSource.decode]'s existing validation
     * rather than by a rule invented here.
     */
    private val ruleSources: Entry = Entry(
        "rule_sources",
        { o, p -> o.put("rule_sources", JSONArray(RuleSource.encode(p.ruleSources))) },
        { o, p ->
            if (o.has("rule_sources")) {
                val decoded = RuleSource.decode(o.getJSONArray("rule_sources").toString())
                // Replace rather than merge: the document is the user's complete
                // source list, so a merge would leave a source the export did not
                // contain. Built-ins are refused by `removeRuleSource` itself, so
                // iterating all of them is safe and keeps this loop from having to
                // know which ones are built in.
                p.ruleSources.toList().forEach { p.removeRuleSource(it.id) }
                decoded.forEach { p.addRuleSource(it) }
            }
        },
    )

    /**
     * Every portable setting, in the order they are applied on import.
     *
     * The order is load-bearing in exactly one place: `rule_sources` must be
     * applied **before** `rule_source_id`, because the id is validated against the
     * source list ([Prefs.updateRuleSource] ignores an id that names nothing
     * selectable), and an imported selection of an imported custom source would be
     * dropped if the list were not there yet.
     */
    private val ENTRIES: List<Entry> = listOf(
        // Connection.
        int("proxy_port", { it.proxyPort }, { p, v -> p.updateProxyPort(v) }),
        int("refresh_hours", { it.refreshHours }, { p, v -> p.updateRefreshHours(v) }),
        bool("offline", { it.offline }, { p, v -> p.updateOffline(v) }),
        bool("auto_connect", { it.autoConnect }, { p, v -> p.updateAutoConnect(v) }),
        // Logs.
        int("stats_interval", { it.statsIntervalSeconds }, { p, v -> p.updateStatsInterval(v) }),
        bool("log_archive", { it.logArchive }, { p, v -> p.updateLogArchive(v) }),
        bool("developer_view", { it.developerView }, { p, v -> p.updateDeveloperView(v) }),
        // Appearance.
        bool("dynamic_color", { it.dynamicColor }, { p, v -> p.updateDynamicColor(v) }),
        string("dark_mode", { it.darkMode }, { p, v -> p.updateDarkMode(v) }),
        string("theme_color", { it.themeColor }, { p, v -> p.updateThemeColor(v) }),
        string("corner_style", { it.cornerStyle }, { p, v -> p.updateCornerStyle(v) }),
        glassEnabled,
        // `wallpaper` is deliberately absent — see the class comment.
        int("wallpaper_scrim", { it.wallpaperScrim }, { p, v -> p.updateWallpaperScrim(v) }),
        int("glass_blur", { it.glassBlur }, { p, v -> p.updateGlassBlur(v) }),
        int("glass_tint", { it.glassTint }, { p, v -> p.updateGlassTint(v) }),
        int("glass_lens", { it.glassLens }, { p, v -> p.updateGlassLens(v) }),
        int("glass_highlight", { it.glassHighlight }, { p, v -> p.updateGlassHighlight(v) }),
        int("glass_border", { it.glassBorder }, { p, v -> p.updateGlassBorder(v) }),
        float("font_scale", { it.fontScale }, { p, v -> p.updateFontScale(v) }),
        bool("home_rate", { it.homeShowRate }, { p, v -> p.updateHomeShowRate(v) }),
        bool("confirm_disconnect", { it.confirmDisconnect }, { p, v -> p.updateConfirmDisconnect(v) }),
        // Advanced / kernel.
        int("max_candidates", { it.maxCandidates }, { p, v -> p.updateMaxCandidates(v) }),
        int("connect_stagger", { it.connectStaggerMillis }, { p, v -> p.updateConnectStagger(v) }),
        int("race_width", { it.raceWidth }, { p, v -> p.updateRaceWidth(v) }),
        int("race_launch", { it.raceLaunchMillis }, { p, v -> p.updateRaceLaunch(v) }),
        int("max_dialing", { it.maxDialing }, { p, v -> p.updateMaxDialing(v) }),
        int("failure_cooldown", { it.failureCooldownSeconds }, { p, v -> p.updateFailureCooldown(v) }),
        bool("dial_names", { it.dialNames }, { p, v -> p.updateDialNames(v) }),
        int("mtu", { it.mtu }, { p, v -> p.updateMtu(v) }),
        int("connect_timeout", { it.connectTimeoutSeconds }, { p, v -> p.updateConnectTimeout(v) }),
        int("tcp_idle", { it.tcpIdleSeconds }, { p, v -> p.updateTcpIdle(v) }),
        int("udp_idle", { it.udpIdleSeconds }, { p, v -> p.updateUdpIdle(v) }),
        int("max_tcp_flows", { it.maxTcpFlows }, { p, v -> p.updateMaxTcpFlows(v) }),
        int("max_udp_flows", { it.maxUdpFlows }, { p, v -> p.updateMaxUdpFlows(v) }),
        bool("answer_dns", { it.answerDnsFromRules }, { p, v -> p.updateAnswerDns(v) }),
        bool("observe_dns", { it.observeDns }, { p, v -> p.updateObserveDns(v) }),
        bool("certificate_check", { it.certificateCheck }, { p, v -> p.updateCertificateCheck(v) }),
        // The two non-scalars, which need their own shapes — see above.
        disabledRules,
        ruleSources,
        string("rule_source_id", { it.ruleSourceId }, { p, v -> p.updateRuleSource(v) }),
        string("update_url", { it.updateUrl }, { p, v -> p.updateUpdateUrl(v) }),
        bool("auto_check_update", { it.autoCheckUpdate }, { p, v -> p.updateAutoCheckUpdate(v) }),
        // `last_update_check_at` is deliberately absent, the second exception to
        // the membership rule stated in the class comment. It records when *this*
        // device last attempted a check, which is a fact about the device and not
        // a preference. Importing it would impose the exporting device's remaining
        // throttle window on the importing one, silencing its first startup check
        // for up to 24 hours for a reason the user cannot see — indistinguishable
        // from an auto-check that is switched on but never fires.
    )

    /**
     * Serialize the portable settings to a JSON document.
     *
     * `version` is [Prefs.CURRENT_VERSION] and the two `appVersion` fields come
     * from `BuildConfig`, so a file says which build wrote it without the reader
     * having to guess. `exportedAt` is an ISO-8601 timestamp with the writer's
     * offset, which is both sortable and unambiguous across time zones.
     */
    fun export(context: Context): String {
        val prefs = Prefs.of(context)
        val settings = JSONObject()
        for (entry in ENTRIES) entry.write(settings, prefs)

        return JSONObject()
            .put("format", FORMAT)
            .put("version", Prefs.CURRENT_VERSION)
            .put("appVersion", BuildConfig.VERSION_NAME)
            .put("appVersionCode", BuildConfig.VERSION_CODE)
            .put("exportedAt", OffsetDateTime.now().toString())
            .put("settings", settings)
            .toString()
    }

    /**
     * Apply a document produced by [export].
     *
     * Every write goes through the existing setters ([Prefs.updateGlassBlur],
     * [Prefs.replaceDisabledRules], …), **never** through
     * `SharedPreferences.edit()` on these keys. The setters are the
     * assign-and-persist pair: they update the in-memory `mutableStateOf` *and*
     * the store. Writing the store directly would leave every already-composed
     * screen reading the old value until the next process start, so the import
     * would appear to have done nothing — the same failure [Prefs.restoreDefaults]
     * documents for its own reason for using setters.
     *
     * Two deliberate absences:
     *
     * * **`prefs_version` is never written.** The store already holds the current
     *   version — the app is running this build — so writing the file's version
     *   would either be a no-op or, for an older file, a *downgrade* that makes
     *   [Prefs.migrate] re-run the whole v3→v7 chain on the next launch. A restore
     *   would then rewrite keys the migration exists to repair once, which is how a
     *   restore turns into a corruption. The version field is metadata about the
     *   writer, not something to copy into the store.
     * * **Old migrations are not replayed.** An older file is accepted and its
     *   recognised keys applied as-is, because this export format has never existed
     *   before: there is no older document whose keys meant something different, so
     *   there is nothing to migrate. Migration code for a format with no history
     *   would be dead code that has to be read and trusted forever.
     */
    fun import(context: Context, json: String): ImportResult {
        val root = try {
            JSONObject(json)
        } catch (_: JSONException) {
            return ImportResult(failure = ImportResult.Failure.NOT_JSON)
        }

        if (root.optString("format") != FORMAT) {
            return ImportResult(failure = ImportResult.Failure.NOT_OURS)
        }

        // A file from a *newer* app may use keys whose meaning this build does not
        // know, and may mean something different by a key it does. Applying the
        // subset it happens to recognise would be a partial restore the user
        // believes is complete, so the whole document is refused instead. `0`
        // catches a missing or non-numeric version as "older than us", which is
        // the permissive direction and is handled below.
        if (root.optInt("version", 0) > Prefs.CURRENT_VERSION) {
            return ImportResult(failure = ImportResult.Failure.TOO_NEW)
        }

        val settings = root.optJSONObject("settings")
            ?: return ImportResult(failure = ImportResult.Failure.NOT_JSON)

        val prefs = Prefs.of(context)
        var applied = 0
        for (entry in ENTRIES) {
            if (!settings.has(entry.key)) continue
            // A hand-edited file can put a string where an int belongs, and
            // `getInt` then throws. That is a malformed *document*, not a crash
            // the settings screen should take, so a single entry that cannot be
            // read is skipped and the rest of the file still applies.
            if (runCatching { entry.read(settings, prefs) }.isSuccess) applied++
        }

        // Anything in the file this build does not know is skipped, not fatal.
        // That is the whole reason a newer file's *extra* keys are harmless even
        // though a newer file itself is refused above: the refusal is about
        // semantics this build cannot honour, not about the key count.
        val known = ENTRIES.mapTo(HashSet()) { it.key }
        val ignored = settings.keys().asSequence().count { it !in known }

        return ImportResult(applied = applied, ignored = ignored)
    }

    /**
     * What an [import] did.
     *
     * [Failure] is a small enum of **stable codes**, not messages: this file has
     * no Android string resources (it is a plain object, so it can be exercised
     * without a `Context` for the format), and the UI is what maps a code to a
     * localized string.
     */
    data class ImportResult(
        val applied: Int = 0,
        val ignored: Int = 0,
        val failure: Failure? = null,
    ) {
        enum class Failure {
            /** The input was not JSON at all. */
            NOT_JSON,

            /** It parsed, but its `format` field is not [FORMAT]. */
            NOT_OURS,

            /** It is ours, but written by a newer build than this one. */
            TOO_NEW,
        }
    }
}
