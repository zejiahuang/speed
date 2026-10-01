package dev.detour.control

import android.content.Context
import androidx.core.content.ContextCompat
import dev.detour.core.DetourVpnService
import dev.detour.core.Kernel
import dev.detour.core.KernelState
import dev.detour.core.LogArchive
import dev.detour.core.Prefs
import dev.detour.core.RuleIndex
import dev.detour.core.RuleSource
import dev.detour.core.RulesRepository
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * The command set, in one place, for every surface that can drive the app.
 *
 * There are two ways in: the exported adb receiver ([ControlReceiver]) and the
 * in-app console row on the settings screen. They must agree on semantics, on
 * validation and on error strings — a console that accepted `set mtu 1400` while
 * the shell rejected it would make the phone and the laptop disagree about the
 * same app. So the command bodies live here exactly once and both surfaces call
 * them; the receiver keeps only its transport (an intent in, logcat out) and the
 * console keeps only its rendering.
 *
 * The console is deliberately *not* gated the way the receiver is: an exported
 * broadcast receiver is a remote-control surface — any app on the device could
 * tell it to connect — which is why the receiver is `BuildConfig.DEBUG`-only.
 * The console can only be typed into by whoever is holding the phone, so it is
 * gated on the `开发者视图` setting instead. Two different questions, two
 * different gates.
 */
object ControlConsole {

    /** The last `dump`, for `adb pull`. */
    private const val DUMP_NAME = "control-dump.json"

    /**
     * Run one command.
     *
     * **Blocking** — it may fetch rules or read files, so call it from
     * `Dispatchers.IO`, never from the main thread.
     */
    fun run(context: Context, command: String, value: String?, key: String?): JSONObject =
        dispatch(context, command, value, key)

    /**
     * One command line, split into the triple [run] takes.
     *
     * [value] and [key] are the two slots the adb receiver fills from the
     * `--es value` / `--es key` extras; a console line has to decide which word
     * goes in which, which is what [parse] does.
     */
    data class CommandLine(val command: String, val value: String?, val key: String?)

    /**
     * Split one typed line into [run]'s arguments.
     *
     * `null` for a blank line or a `#` comment, both of which the caller ignores.
     * Only the command token is lowercased; everything else keeps its case,
     * because a URL or a group key is not a command word.
     *
     * Validation is deliberately *not* done here — it stays in [run], so the
     * console and the shell produce the same `set needs key`, `mode expects vpn
     * or proxy` and `unknown command: x` messages rather than two sets of them.
     */
    fun parse(line: String): CommandLine? {
        val trimmed = line.trim()
        if (trimmed.isEmpty() || trimmed.startsWith("#")) return null
        val tokens = trimmed.split(Regex("\\s+"))
        val command = tokens[0].lowercase()
        val rest = tokens.drop(1)

        return when (command) {
            // `set <key> <value>`: the value is the remainder joined, so a value
            // containing a space survives; the key is the single token before it.
            "set" -> CommandLine(
                command = "set",
                value = rest.drop(1).joinToString(" ").ifEmpty { null },
                key = rest.firstOrNull(),
            )

            "rules" -> {
                val sub = rest.firstOrNull()?.lowercase()
                when {
                    sub == null -> CommandLine("rules", null, null)
                    sub == "refresh" -> CommandLine("rules", "refresh", null)
                    // `disable` / `enable` take a group key: the remainder joined.
                    sub == "disable" || sub == "enable" -> CommandLine(
                        command = "rules",
                        value = sub,
                        key = rest.drop(1).joinToString(" ").ifEmpty { null },
                    )
                    // `add:<url> [name]`: the value is the whole `add:` token with
                    // its URL untouched, and the remainder is the display name.
                    sub.startsWith("add:") -> CommandLine(
                        command = "rules",
                        value = rest[0],
                        key = rest.drop(1).joinToString(" ").ifEmpty { null },
                    )
                    // `remove:<id>`: no key; the id rides inside the value.
                    sub.startsWith("remove:") -> CommandLine("rules", rest[0], null)
                    // An unknown sub-command is passed through so `run` produces
                    // its own `rules expects ...` message.
                    else -> CommandLine("rules", rest[0], null)
                }
            }

            // `log [clear|archive|clear-archive]`: the sub-command is the value.
            "log" -> CommandLine("log", rest.firstOrNull()?.lowercase(), null)

            // `mode vpn` / `mode proxy`: the only command outside `set`/`rules`/
            // `log` that reads a value.
            "mode" -> CommandLine("mode", rest.joinToString(" ").ifEmpty { null }, null)

            // status / connect / disconnect / dump / help, and anything
            // unrecognised — no value, and `run` owns the error for an unknown
            // word.
            else -> CommandLine(command, null, null)
        }
    }

    /**
     * The status snapshot on its own, for callers that need it without a command.
     */
    fun statusJson(): JSONObject {
        val status = KernelState.status.value
        val stats = KernelState.stats.value
        return JSONObject()
            .put("phase", status.phase.name.lowercase())
            .put("mode", status.mode.name.lowercase())
            .put("running", status.isRunning)
            .put("message", status.message ?: JSONObject.NULL)
            .put("proxy_port", status.proxyPort)
            .put("uptime_ms", if (status.isRunning) System.currentTimeMillis() - status.sinceMillis else 0)
            .put("kernel_loaded", Kernel.loadError == null)
            .put("kernel_error", Kernel.loadError ?: JSONObject.NULL)
            .put("bytes_to_upstream", stats.bytesToUpstream)
            .put("bytes_to_client", stats.bytesToClient)
            .put("tcp_opened", stats.tcpOpened)
            .put("tcp_closed", stats.tcpClosed)
            .put("tcp_failures", stats.tcpConnectFailures)
            .put("udp_opened", stats.udpOpened)
            .put("dns_queries", stats.dnsQueries)
            .put("dns_answered_locally", stats.dnsAnsweredLocally)
            .put("live_flows", stats.liveFlows)
            .put("flows_matched_rules", stats.flowsMatchedRules)
            .put("flows_direct", stats.flowsDirect)
            // The count of TCP flows the kernel refused to relay and answered
            // with a RST. It is the one counter that explains "the tunnel is up
            // but this site does not load": the flow never reached an upstream,
            // so no connect failure was recorded either. Kept next to the other
            // flow attributions so a single `status` dump answers where a flow
            // went -- matched a rule, went direct, or was rejected outright.
            .put("tcp_flows_rejected", stats.tcpRejected)
    }

    private fun dispatch(
        context: Context,
        command: String,
        value: String?,
        key: String?,
    ): JSONObject = when (command) {
        "status" -> statusJson()

        "connect" -> {
            val mode = KernelState.status.value.mode
            if (mode == KernelState.Mode.VPN && android.net.VpnService.prepare(context) != null) {
                // A broadcast cannot show the consent dialog, so the tunnel only
                // starts when it has already been granted. Reported rather than
                // silently doing nothing.
                JSONObject().put("error", "vpn consent not granted; open the app once")
            } else {
                ContextCompat.startForegroundService(
                    context, DetourVpnService.startIntent(context, mode),
                )
                JSONObject().put("started", mode.name.lowercase())
            }
        }

        "disconnect" -> {
            context.startService(DetourVpnService.stopIntent(context))
            JSONObject().put("stopped", true)
        }

        "mode" -> {
            val mode = when (value?.lowercase()) {
                "vpn" -> KernelState.Mode.VPN
                "proxy", "connect" -> KernelState.Mode.PROXY
                else -> null
            }
            if (mode == null) {
                JSONObject().put("error", "mode expects vpn or proxy")
            } else {
                // Stored as well as shown. Setting only the holder looked right
                // and meant a mode chosen here was forgotten on the next cold
                // start, which is indistinguishable from "the setting does not
                // work".
                Prefs.of(context).updateMode(mode.name.lowercase())
                KernelState.setStatus { it.copy(mode = mode) }
                JSONObject().put("mode", mode.name.lowercase())
            }
        }

        "rules" -> {
            val raw = value
            if (raw != null && raw.startsWith("add:")) {
                addRuleSource(context, raw.removePrefix("add:"), key)
            } else if (raw != null && raw.startsWith("remove:")) {
                removeRuleSource(context, raw.removePrefix("remove:"))
            } else when (raw?.lowercase()) {
            "refresh", null -> {
                RulesRepository.invalidate(context)
                val document = RulesRepository.load(context, forceRefresh = true)
                JSONObject()
                    .put("bytes", document.size)
                    .put("cached", RulesRepository.cacheFile(context).absolutePath)
            }
            "count" -> {
                // The filtered size, not the cached one. What the kernel will get
                // is the only number that answers "did my switch do anything".
                val document = RulesRepository.load(context)
                val index = runCatching { RuleIndex.parse(context) }.getOrNull()
                JSONObject()
                    .put("bytes", document.size)
                    .put("cached_bytes", RulesRepository.cacheFile(context).length())
                    .put("switches", Prefs.of(context).disabledRules.size)
                    // The size alone cannot tell "21 entries fewer" from "one entry
                    // with 21 addresses fewer". Counting makes a switch auditable.
                    .put("groups", index?.groups?.size ?: -1)
                    .put(
                        "domains",
                        index?.groups?.sumOf { it.domainCount } ?: -1,
                    )
                    .put(
                        "addresses",
                        index?.groups?.sumOf { it.addressCount } ?: -1,
                    )
            }
            "clear" -> {
                RulesRepository.invalidate(context)
                JSONObject().put("cleared", true)
            }
            // The rule switches, driven from a shell so the three levels can be
            // tested without tapping through 21 groups.
            //
            // Each one ends by handing the freshly filtered document to a tunnel
            // that is already up. Writing the store is only half of it: a running
            // tunnel is holding the document it started with, so without the reload
            // the switch is decoration — which is exactly what the first attempt at
            // this looked like from the outside.
            "disable" -> {
                if (key == null) return JSONObject().put("error", "disable needs key")
                Prefs.of(context).disableRule(key)
                DetourVpnService.reloadRulesIfRunning()
                JSONObject()
                    .put("disabled", key)
                    .put("bytes", RulesRepository.load(context).size)
            }
            "enable" -> {
                if (key == null) return JSONObject().put("error", "enable needs key")
                Prefs.of(context).enableRule(key)
                DetourVpnService.reloadRulesIfRunning()
                JSONObject()
                    .put("enabled", key)
                    .put("bytes", RulesRepository.load(context).size)
            }
            "enable-all" -> {
                Prefs.of(context).replaceDisabledRules(emptySet())
                DetourVpnService.reloadRulesIfRunning()
                JSONObject()
                    .put("enabled_all", true)
                    .put("bytes", RulesRepository.load(context).size)
            }
            "disabled" -> {
                val keys = JSONArray()
                Prefs.of(context).disabledRules.forEach { keys.put(it) }
                JSONObject().put("disabled", keys)
            }
            else -> JSONObject().put(
                "error",
                "rules expects refresh, count, clear, disable, enable, enable-all, disabled, add:<url> or remove:<id>",
            )
            }
        }

        "log" -> when (value?.lowercase()) {
            "clear" -> {
                KernelState.clearLogs()
                JSONObject().put("cleared", true)
            }
            // The archive is a file, not a flow, so it cannot be read back through
            // `entries`. Reporting its path and size is what makes "按天归档" auditable
            // from a shell: the size moves when the setting is on and stands still
            // when it is off.
            "archive" -> JSONObject()
                .put("path", LogArchive.file(context).absolutePath)
                .put("bytes", LogArchive.sizeBytes(context))
            "clear-archive" -> {
                LogArchive.clear(context)
                JSONObject().put("archive_cleared", true)
            }
            else -> {
                val entries = JSONArray()
                KernelState.logs.value.take(50).forEach { entry ->
                    entries.put(
                        JSONObject()
                            .put("at", entry.atMillis)
                            .put("level", entry.level.name)
                            .put("tag", entry.tag)
                            .put("message", entry.message),
                    )
                }
                JSONObject().put("entries", entries)
            }
        }

        "set" -> {
            if (key == null) return JSONObject().put("error", "set needs key")
            setSetting(context, key, value)
        }

        "dump" -> {
            val snapshot = statusJson()
                .put("settings", settingsJson(context))
                .put("rules", JSONObject().put("bytes", runCatching {
                    RulesRepository.load(context).size
                }.getOrDefault(0)))
                .put("logs", KernelState.logs.value.size)
                // The archive's size, so a `dump` answers "is the archive switch
                // doing anything" without a second command.
                .put(
                    "archive",
                    JSONObject()
                        .put("path", LogArchive.file(context).absolutePath)
                        .put("bytes", LogArchive.sizeBytes(context)),
                )
            File(context.filesDir, DUMP_NAME).writeText(snapshot.toString())
            snapshot.put("written", DUMP_NAME)
        }

        "help" -> JSONObject().put(
            "commands",
            JSONArray(
                listOf(
                    "status", "connect", "disconnect", "mode <vpn|proxy>",
                    "rules <refresh|count|clear|disable|enable|enable-all|disabled|add:<url>|remove:<id>>",
                    "log <clear|archive|clear-archive>",
                    "set <key> <value>", "dump", "help",
                ),
            ),
        )

        else -> JSONObject().put("error", "unknown command: $command")
    }

    /**
     * `rules add:<url> [--es key <name>]` — add a custom source and select it.
     *
     * The URL goes through [RuleSource.normalizeUrl] so a GitHub page URL pasted
     * from a browser becomes the raw URL that actually serves the file. The name
     * rides in `key`, which is otherwise unused by this sub-command, rather than
     * being packed into the value next to the URL.
     */
    private fun addRuleSource(context: Context, rawUrl: String, name: String?): JSONObject {
        val url = RuleSource.normalizeUrl(rawUrl)
        if (!url.startsWith("http://") && !url.startsWith("https://")) {
            return JSONObject().put("error", "add expects an http(s) URL")
        }
        val source = RuleSource(
            id = "custom:$url",
            label = name?.trim().orEmpty().ifEmpty { url },
            url = url,
        )
        val prefs = Prefs.of(context)
        prefs.addRuleSource(source)
        // Selected as well as stored: a source that is added but not selected
        // changes nothing, and the caller named it, so it is what they want.
        prefs.updateRuleSource(source.id)
        RulesRepository.invalidate(context)
        DetourVpnService.reloadRulesIfRunning()
        return JSONObject().put("added", source.id).put("url", source.url)
    }

    /** `rules remove:<id>` — drop a custom source and drop the stale cache. */
    private fun removeRuleSource(context: Context, id: String): JSONObject {
        Prefs.of(context).removeRuleSource(id)
        RulesRepository.invalidate(context)
        DetourVpnService.reloadRulesIfRunning()
        return JSONObject().put("removed", id)
    }

    private fun settingsJson(context: Context): JSONObject {
        val prefs = Prefs.of(context)
        val sources = JSONArray()
        prefs.ruleSources.forEach { source ->
            sources.put(
                JSONObject()
                    .put("id", source.id)
                    .put("label", source.labelRes?.let { context.getString(it) } ?: source.label)
                    .put("url", source.url)
                    .put("builtin", source.builtin)
                    .put("usable", source.usable),
            )
        }
        return JSONObject()
            .put("proxy_port", prefs.proxyPort)
            // `rule_source` is the selected id, which is what a shell passes back
            // to `set rule_source`. The label is the human name — for a built-in it
            // comes from a string resource the shell cannot read.
            .put("rule_source", prefs.ruleSource)
            .put(
                "rule_source_label",
                prefs.selectedSource.labelRes?.let { context.getString(it) }
                    ?: prefs.selectedSource.label,
            )
            .put("rule_sources", sources)
            .put("refresh_hours", prefs.refreshHours)
            .put("offline", prefs.offline)
            .put("stats_interval", prefs.statsIntervalSeconds)
            .put("log_archive", prefs.logArchive)
            .put("dynamic_color", prefs.dynamicColor)
            .put("dark_mode", prefs.darkMode)
            .put("developer_view", prefs.developerView)
            .put("failure_cooldown", prefs.failureCooldownSeconds)
            .put("max_candidates", prefs.maxCandidates)
            .put("connect_stagger", prefs.connectStaggerMillis)
            .put("dial_names", prefs.dialNames)
            .put("race_width", prefs.raceWidth)
            .put("race_launch", prefs.raceLaunchMillis)
            .put("max_dialing", prefs.maxDialing)
            .put("glass_enabled", prefs.glassEnabled)
            // The five numbers behind the glass switch. They are reported
            // unconditionally — not only while the switch is on — because the whole
            // point of reporting a setting is that `set` can be read back, and a
            // value that disappears from the dump depending on a switch would be
            // unverifiable: a script that sets `glass_blur` and then reads it back
            // would find nothing whenever the switch happened to be off. The rule
            // this project learned the hard way is that a `set` must be confirmed
            // by reading the pref back, never by trusting the command's own
            // success reply; these five are what makes that possible.
            .put("glass_blur", prefs.glassBlur)
            .put("glass_tint", prefs.glassTint)
            .put("glass_lens", prefs.glassLens)
            .put("glass_highlight", prefs.glassHighlight)
            .put("glass_border", prefs.glassBorder)
            .put("font_scale", prefs.fontScale)
            .put("mode", prefs.mode)
            .put("theme_color", prefs.themeColor)
            .put("corner_style", prefs.cornerStyle)
            .put("mtu", prefs.mtu)
            .put("connect_timeout", prefs.connectTimeoutSeconds)
            .put("tcp_idle", prefs.tcpIdleSeconds)
            .put("udp_idle", prefs.udpIdleSeconds)
            .put("max_tcp_flows", prefs.maxTcpFlows)
            .put("max_udp_flows", prefs.maxUdpFlows)
            .put("answer_dns", prefs.answerDnsFromRules)
            .put("observe_dns", prefs.observeDns)
            .put("certificate_check", prefs.certificateCheck)
            .put("home_rate", prefs.homeShowRate)
            .put("confirm_disconnect", prefs.confirmDisconnect)
            .put("auto_connect", prefs.autoConnect)
            // Reported like any other string setting so `set update_url <url>`
            // can be read back and confirmed. An empty value is the real
            // "not configured" state, not a missing key, which is why it is
            // dumped unconditionally rather than omitted when blank.
            //
            // It is also the only way left to change the address: the About
            // page's 版本清单地址 row was removed, so this `set` case is what
            // keeps "point the app at your own mirror" possible without a
            // rebuild. See `Prefs.updateUrl`.
            .put("update_url", prefs.updateUrl)
            // The release channel, reported so `set update_channel beta` can be
            // read back and confirmed. It is here for the same reason
            // `update_url` is: without it the only way to reach the beta channel
            // is a tap on a segmented row, and the channel decides which
            // *endpoint* gets asked — a difference no other setting can stand in
            // for.
            .put("update_channel", prefs.updateChannel)
            // The auto-check switch is a preference and is reported like any
            // other, so `set auto_check_update false` can be read back and
            // confirmed.
            .put("auto_check_update", prefs.autoCheckUpdate)
            // Reported although it is not settable — there is no `set` case for
            // it. It is runtime state rather than a preference, like
            // `applied_kernel_settings` below, and reading this number is the
            // only way to tell whether the 24-hour throttle is what is
            // suppressing a startup check.
            .put("last_update_check_at", prefs.lastUpdateCheckAt)
            .put("disabled_rules", prefs.disabledRules.size)
            .put("kernel_settings", prefs.kernelSettingsJson())
            // Two different questions, and the pair is what makes each unambiguous.
            // `kernel_settings` is what the *next* connect would send; this one is what
            // the *running* engine was actually built from. Equal means the tunnel
            // matches the settings screen; different means a reconnect is pending. On
            // its own, `kernel_settings` reads like kernel state and is not — the FFI
            // exposes no way to read the engine's config back.
            .put("applied_kernel_settings", prefs.appliedKernelSettings)
    }

    private fun setSetting(context: Context, key: String, value: String?): JSONObject {
        if (value == null) return JSONObject().put("error", "set needs value")
        val prefs = Prefs.of(context)
        val asBool = value.toBooleanStrictOrNull()
        val asInt = value.toIntOrNull()

        return when (key) {
            "proxy_port" -> asInt?.let { prefs.updateProxyPort(it) } ?: return bad("int")
            "rule_source" -> prefs.updateRuleSource(value)
            "refresh_hours" -> asInt?.let { prefs.updateRefreshHours(it) } ?: return bad("int")
            "offline" -> asBool?.let { prefs.updateOffline(it) } ?: return bad("bool")
            "stats_interval" -> asInt?.let { prefs.updateStatsInterval(it) } ?: return bad("int")
            "log_archive" -> asBool?.let { prefs.updateLogArchive(it) } ?: return bad("bool")
            "dynamic_color" -> asBool?.let { prefs.updateDynamicColor(it) } ?: return bad("bool")
            "dark_mode" -> prefs.updateDarkMode(value)
            "mode" -> prefs.updateMode(value)
            "theme_color" -> prefs.updateThemeColor(value)
            "corner_style" -> prefs.updateCornerStyle(value)
            "mtu" -> asInt?.let { prefs.updateMtu(it) } ?: return bad("int")
            "connect_timeout" -> asInt?.let { prefs.updateConnectTimeout(it) } ?: return bad("int")
            "tcp_idle" -> asInt?.let { prefs.updateTcpIdle(it) } ?: return bad("int")
            "udp_idle" -> asInt?.let { prefs.updateUdpIdle(it) } ?: return bad("int")
            "max_tcp_flows" -> asInt?.let { prefs.updateMaxTcpFlows(it) } ?: return bad("int")
            "max_udp_flows" -> asInt?.let { prefs.updateMaxUdpFlows(it) } ?: return bad("int")
            "answer_dns" -> asBool?.let { prefs.updateAnswerDns(it) } ?: return bad("bool")
            "observe_dns" -> asBool?.let { prefs.updateObserveDns(it) } ?: return bad("bool")
            "certificate_check" -> asBool?.let { prefs.updateCertificateCheck(it) } ?: return bad("bool")
            "home_rate" -> asBool?.let { prefs.updateHomeShowRate(it) } ?: return bad("bool")
            "confirm_disconnect" -> asBool?.let { prefs.updateConfirmDisconnect(it) } ?: return bad("bool")
            "auto_connect" -> asBool?.let { prefs.updateAutoConnect(it) } ?: return bad("bool")
            "developer_view" -> asBool?.let { prefs.updateDeveloperView(it) } ?: return bad("bool")
            "failure_cooldown" -> asInt?.let { prefs.updateFailureCooldown(it) } ?: return bad("int")
            "max_candidates" -> asInt?.let { prefs.updateMaxCandidates(it) } ?: return bad("int")
            "connect_stagger" -> asInt?.let { prefs.updateConnectStagger(it) } ?: return bad("int")
            "dial_names" -> asBool?.let { prefs.updateDialNames(it) } ?: return bad("bool")
            // The three racing knobs. They were reachable only by dragging a
            // stepper on a phone screen, which made the one thing worth scripting
            // — the serial-vs-racing rollback — impossible to test from a shell.
            // `race_launch_milliseconds` is accepted alongside `race_launch`
            // because that is the name the kernel's own settings document uses.
            "race_width" -> asInt?.let { prefs.updateRaceWidth(it) } ?: return bad("int")
            "race_launch", "race_launch_milliseconds" ->
                asInt?.let { prefs.updateRaceLaunch(it) } ?: return bad("int")
            "max_dialing" -> asInt?.let { prefs.updateMaxDialing(it) } ?: return bad("int")
            "glass_enabled" -> asBool?.let { prefs.updateGlassEnabled(it) } ?: return bad("bool")
            // The glass material's five knobs. They are settable regardless of the
            // switch's state, and that is deliberate rather than an oversight: a
            // script may set the numbers *first* and flip the switch afterwards,
            // and if the setters rejected them while the switch was off that
            // ordering would look like the settings were silently dropped. Both
            // orderings — set then switch on, or switch on then tune — have to
            // work, so a value is accepted whenever it is sent. Whether it
            // currently reaches the pixels is a separate question, answered by the
            // switch in the dump, not by refusing the write.
            "glass_blur" -> asInt?.let { prefs.updateGlassBlur(it) } ?: return bad("int")
            "glass_tint" -> asInt?.let { prefs.updateGlassTint(it) } ?: return bad("int")
            "glass_lens" -> asInt?.let { prefs.updateGlassLens(it) } ?: return bad("int")
            "glass_highlight" -> asInt?.let { prefs.updateGlassHighlight(it) } ?: return bad("int")
            "glass_border" -> asInt?.let { prefs.updateGlassBorder(it) } ?: return bad("int")
            "font_scale" -> value.toFloatOrNull()?.let { prefs.updateFontScale(it) }
                ?: return bad("float")
            // The update-manifest URL. A plain string, so it takes the same shape
            // as `dark_mode` above: the value is handed to its setter untouched
            // (the setter trims it), with no parsing that could reject a URL.
            "update_url" -> prefs.updateUpdateUrl(value)
            // The release channel, a plain string like `dark_mode` above. An
            // unrecognised value is not rejected here — the setter collapses it
            // to stable — so a `set` of a misspelled channel is a silent no-op
            // that the read-back catches, which is why the dump reports this
            // key. See `Prefs.normalizeUpdateChannel`.
            "update_channel" -> prefs.updateUpdateChannel(value)
            // The auto-check switch, a plain boolean like `auto_connect` above.
            // `last_update_check_at` has no case here on purpose: it is runtime
            // state, not a preference, so `dump` reports it and `set` leaves it
            // alone — the same split as `applied_kernel_settings`.
            "auto_check_update" -> asBool?.let { prefs.updateAutoCheckUpdate(it) } ?: return bad("bool")
            else -> return JSONObject().put("error", "unknown setting: $key")
        }.let { JSONObject().put(key, value) }
    }

    private fun bad(expected: String) = JSONObject().put("error", "expected a $expected")
}
