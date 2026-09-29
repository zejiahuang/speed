package dev.detour.core

import android.content.Context
import android.content.SharedPreferences
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * Settings, as observable properties over `SharedPreferences`.
 *
 * `SharedPreferences` rather than DataStore: every setting here is a scalar read
 * once per session or on a toggle, so the extra machinery would buy nothing. The
 * properties are Compose state so a screen can read them directly.
 *
 * The defaults are the ones a first run should have, not the ones that are
 * easiest: rules come from the built-in HelloGitHub source, dynamic colour is
 * on because the device's own palette is what Material You is for, and the
 * developer view is off because most people do not want a cooling counter on
 * their home screen.
 */
class Prefs private constructor(private val store: SharedPreferences) {

    var proxyPort by mutableStateOf(store.getInt(KEY_PROXY_PORT, DEFAULT_PROXY_PORT))
        private set

    /**
     * Every rule source the app offers, built-ins first.
     *
     * Built-ins are not stored — see [RuleSource]'s class comment — so this is
     * always `defaults() + whatever the user added`, never an empty list, which
     * is what lets [selectedSource] end in a non-null fallback.
     */
    var ruleSources by mutableStateOf(RuleSource.decode(store.getString(KEY_RULE_SOURCES, null)))
        private set

    /** Which source is selected, by id. */
    var ruleSourceId by mutableStateOf(store.getString(KEY_RULE_SOURCE_ID, RuleSource.HELLOGITHUB_HOSTS_ID) ?: RuleSource.HELLOGITHUB_HOSTS_ID)
        private set

    /**
     * The selected source, resolved against the current list.
     *
     * The stored id is a preference, not a guarantee: the user can delete the
     * source it names, and an update can retire one. So the fallback chain is
     * "the id if it is still selectable" → "the first selectable source" → "the
     * first source, selectable or not". The last step is what keeps this total;
     * it cannot be reached with the built-in list, but a total function is the
     * only kind that a settings screen can safely call.
     */
    val selectedSource: RuleSource
        get() = ruleSources.firstOrNull { it.id == ruleSourceId && it.usable }
            ?: ruleSources.firstOrNull { it.usable }
            ?: ruleSources.first()

    /**
     * The selected source's id, for callers that only need a name — the control
     * receiver's `rule_source` field and its `set rule_source <id>` handler.
     */
    val ruleSource: String get() = selectedSource.id

    /**
     * The cached rule document predates the identity-stamp fix and must be
     * refetched. Set once by [migrate] version 5, consumed and cleared by
     * `RulesRepository`.
     *
     * **Why a pref and not a file delete here.** The repair is "throw the cache
     * away", and the cache is a file — but `Prefs` deliberately has no file I/O:
     * it is constructed lazily from `of()` on whatever thread first touches a
     * setting, and a filesystem call there would make a settings read able to
     * block on storage. A flag carries the same decision to the one component that
     * already owns the cache file, and by the time `load()` runs the caller is
     * already doing file I/O so the delete is free.
     *
     * **Why set, not delete-a-key.** "Set this flag" is a value no previous build
     * could have written, so the repair cannot fire on a device that never needed
     * it; a *deleted* key is indistinguishable from one that was never present,
     * which is why that shape of repair cannot be gated on its own absence.
     */
    var rulesCacheDirty by mutableStateOf(store.getBoolean(KEY_RULES_DIRTY, false))
        private set

    /**
     * Consume the "the cache is from an older build" flag.
     *
     * Returns `true` at most once per flag lifetime: the read and the clear are
     * this one method so a caller cannot read it and forget to clear it, which
     * would refetch on every single load. Cleared before returning, so even a
     * caller that throws on the way to refetching does not leave the flag set.
     */
    fun consumeRulesCacheDirty(): Boolean {
        if (!rulesCacheDirty) return false
        rulesCacheDirty = false
        edit { putBoolean(KEY_RULES_DIRTY, false) }
        return true
    }

    /**
     * The settings document the running engine was built from, verbatim.
     *
     * Without it the app cannot tell "the user changed a row" from "the user changed
     * a row back": the banner that says a reconnect is needed would either never
     * appear, or appear when nothing had changed. It is stored as the exact string
     * [kernelSettingsJson] produced, so the comparison is against the document the
     * kernel was actually given rather than a reconstruction of it.
     */
    var appliedKernelSettings by mutableStateOf(store.getString(KEY_APPLIED_SETTINGS, "").orEmpty())
        private set

    /** Called by `DetourVpnService` immediately after the engine is built. */
    fun recordAppliedKernelSettings(settings: String) {
        appliedKernelSettings = settings
        edit { putString(KEY_APPLIED_SETTINGS, settings) }
    }

    /**
     * True when the document that would be sent on the next connect differs from
     * the one the running engine is using.
     *
     * `isNotEmpty` is load-bearing: a prefs file written before this key existed, or
     * by a run that never connected, has nothing applied, and a banner about nothing
     * is worse than no banner.
     */
    fun kernelSettingsPending(): Boolean =
        appliedKernelSettings.isNotEmpty() && appliedKernelSettings != kernelSettingsJson()

    /**
     * How long a downloaded rule document is reused before it is fetched again.
     *
     * Daily by default. It used to be 6 hours — four fetches a day — which is more
     * often than the sources change and is pure bandwidth for a document the user
     * is not editing. The row stays adjustable over 1..72; see [migrate] for why an
     * existing stored 6 is carried forward rather than left alone.
     */
    var refreshHours by mutableStateOf(store.getInt(KEY_REFRESH_HOURS, 24))
        private set

    var offline by mutableStateOf(store.getBoolean(KEY_OFFLINE, false))
        private set

    /**
     * Connect the tunnel as soon as the app starts. Off by default.
     *
     * Off rather than on because a tunnel that comes up on its own the first time
     * someone opens the app is a surprise, not a convenience: the switch is what
     * makes it a choice. The launch path additionally refuses to ask for consent
     * from an `Application` (see `DetourApp.maybeAutoConnect`), so this can only
     * ever connect on a device that has already granted it once.
     *
     * No migration bump for the same reason `applied_kernel_settings` needs none:
     * the default is already the correct behaviour for an upgraded install.
     */
    var autoConnect by mutableStateOf(store.getBoolean(KEY_AUTO_CONNECT, false))
        private set

    /**
     * Where the app fetches the update "version manifest" — a small JSON
     * document describing the newest published build.
     *
     * **A setting rather than a constant because the app has no server of its
     * own.** There is no release endpoint this project controls, so the manifest
     * lives wherever the owner puts it, and the owner has to be able to point the
     * app at it — or move it — without a rebuild. Baking a URL into the APK would
     * make every relocation a new release, which is the one thing an update
     * checker cannot depend on.
     *
     * **Defaults to empty, and empty means "do not check".** That is the honest
     * starting state: nothing has been configured, so nothing is fetched and the
     * UI can say so. A placeholder URL would be worse than useless — it would
     * produce a check that always fails, which reads as "your update source is
     * broken" on a device where no source was ever meant to exist. The reader
     * (`UpdateChecker`) already treats a blank URL as a first-class
     * "not configured" outcome for exactly this reason.
     */
    var updateUrl by mutableStateOf(store.getString(KEY_UPDATE_URL, "") ?: "")
        private set

    var statsIntervalSeconds by mutableStateOf(store.getInt(KEY_STATS_INTERVAL, 5))
        private set

    var logArchive by mutableStateOf(store.getBoolean(KEY_LOG_ARCHIVE, true))
        private set

    var dynamicColor by mutableStateOf(store.getBoolean(KEY_DYNAMIC_COLOR, true))
        private set

    var darkMode by mutableStateOf(store.getString(KEY_DARK_MODE, "follow") ?: "follow")
        private set

    var developerView by mutableStateOf(store.getBoolean(KEY_DEVELOPER_VIEW, false))
        private set

    /**
     * Which mode the tunnel runs in.
     *
     * Persisted, and that is the whole point: the mode used to live only in
     * `KernelState`, which is a process singleton. Switching to VPN, leaving the
     * app, and coming back put it silently back on the proxy — which reads as
     * "VPN never starts" and is impossible to diagnose from the outside.
     */
    var mode by mutableStateOf(store.getString(KEY_MODE, "proxy") ?: "proxy")
        private set

    /**
     * Which palette to use. `dynamic` follows the wallpaper on Android 12+;
     * anything else is a seed the theme builds a full scheme from, for devices
     * where Material You is unavailable or unwanted.
     */
    var themeColor by mutableStateOf(store.getString(KEY_THEME_COLOR, "dynamic") ?: "dynamic")
        private set

    /** How round the corners are. Feeds the Material shape scale. */
    var cornerStyle by mutableStateOf(store.getString(KEY_CORNER_STYLE, "medium") ?: "medium")
        private set

    /**
     * Whether the liquid-glass half of the material is on.
     *
     * The material has two independently switchable halves — see
     * `resolveDetourGlass` — and this is the refraction half: the lens, the
     * specular highlight and the lit edge. It is independent of [glassFrost] on
     * purpose; either can be on without the other, and each is a real material
     * on its own.
     *
     * **Both switches default off.** A fresh install draws flat opaque cards and
     * the user opts in, which is the owner's explicit choice. There is
     * deliberately no device inference any more: the old `auto` tier guessed
     * from RAM and core count, and a guess that silently changes the look per
     * device made the same build render differently on the emulator than on a
     * phone — harder to reason about, and impossible to verify against a stored
     * value, since nothing was stored.
     */
    var glassLiquid by mutableStateOf(store.getBoolean(KEY_GLASS_LIQUID, false))
        private set

    /**
     * Whether the frosted half of the material is on.
     *
     * The blur half: it samples and blurs the backdrop. Independent of
     * [glassLiquid]; on its own it is a frosted flat panel, with the refraction
     * off.
     *
     * Defaults off for the same reason [glassLiquid] does — see there.
     */
    var glassFrost by mutableStateOf(store.getBoolean(KEY_GLASS_FROST, false))
        private set

    /**
     * The identity of the chosen background photo, or `""` for none.
     *
     * An identity rather than a path: the bytes live in `filesDir` under a fixed
     * name and `WallpaperStore` stamps a SHA-256 of them into a sidecar. A read
     * requires this value to equal the sidecar's, so a stored string that names a
     * photo the disk no longer holds degrades to "no wallpaper" instead of
     * showing the wrong image — the same lesson `RulesRepository` records about a
     * cache that outlived what it described. `WallpaperStore` owns both this
     * value and the file, so the two are always written together.
     */
    var wallpaper by mutableStateOf(store.getString(KEY_WALLPAPER, "") ?: "")
        private set

    /**
     * How strongly the wallpaper is dimmed behind the UI, as a percent (0..100).
     *
     * A photo is arbitrary content and text has to stay readable over it, so the
     * scrim is a user setting rather than a fixed value; 58 is the owner's chosen
     * default. Bounded like the other numeric knobs here — the setter coerces.
     */
    var wallpaperScrim by mutableStateOf(store.getInt(KEY_WALLPAPER_SCRIM, DEFAULT_WALLPAPER_SCRIM))
        private set

    // The five numbers behind the two glass switches. They are stored
    // unconditionally — not only while a switch is on — so a user can set them
    // up, leave, and come back to find them intact, and so the control channel
    // can write them in any order relative to flipping a switch.
    //
    // **Their defaults are deliberately the old `high` preset's numbers, digit
    // for digit** (16 / 72 / 32 / 12 / 50). That is the whole reason for these
    // particular values: turning a switch on must reveal the material at a sane
    // setting, not also jump to an unrelated one. The numbers are the picture a
    // switch-on produces; changing them would make the act of enabling an
    // effect change something else as well. There is no version migration
    // because a missing key already yields the default, and a migration that
    // rewrote the same numbers would be ceremony — the same reasoning the
    // version note above `CURRENT_VERSION` records.
    //
    // Which switch reads which: the blur is the frost; the lens, highlight and
    // border are the liquid glass; the tint is shared, because both materials
    // draw a base fill and a card with no fill would let the wallpaper through
    // behind its text. See `resolveDetourGlass` for the assembly.

    /** Blur radius of the frosted material, in dp (0..100). */
    var glassBlur by mutableStateOf(store.getInt(KEY_GLASS_BLUR, DEFAULT_GLASS_BLUR))
        private set

    /** Opacity of the glass's base tint, as a percent (0..100). */
    var glassTint by mutableStateOf(store.getInt(KEY_GLASS_TINT, DEFAULT_GLASS_TINT))
        private set

    /** Refraction strength of the liquid glass, in dp (0..150). */
    var glassLens by mutableStateOf(store.getInt(KEY_GLASS_LENS, DEFAULT_GLASS_LENS))
        private set

    /** Specular highlight alpha of the liquid glass, as a percent (0..100). */
    var glassHighlight by mutableStateOf(store.getInt(KEY_GLASS_HIGHLIGHT, DEFAULT_GLASS_HIGHLIGHT))
        private set

    /** Border width of the liquid glass, as a percent of 2dp (0..100, so 50 → 1dp). */
    var glassBorder by mutableStateOf(store.getInt(KEY_GLASS_BORDER, DEFAULT_GLASS_BORDER))
        private set

    /** Multiplier on every text size. */
    var fontScale by mutableStateOf(store.getFloat(KEY_FONT_SCALE, 1f))
        private set

    /** Whether the home screen polls the rate twice a second. */
    var homeShowRate by mutableStateOf(store.getBoolean(KEY_HOME_RATE, true))
        private set

    /**
     * Ask before disconnecting.
     *
     * Off by default would be wrong for anyone who has ever fat-fingered the
     * power button mid-download, and on by default is wrong for anyone who
     * connects and disconnects all day. On, with a switch.
     */
    var confirmDisconnect by mutableStateOf(store.getBoolean(KEY_CONFIRM_DISCONNECT, true))
        private set

    // Advanced. These mirror the kernel's own knobs, and the defaults are the
    // kernel's, so changing nothing here changes nothing there.
    var failureCooldownSeconds by mutableStateOf(store.getInt(KEY_COOLDOWN, 60))
        private set

    var maxCandidates by mutableStateOf(store.getInt(KEY_MAX_CANDIDATES, 12))
        private set

    var connectStaggerMillis by mutableStateOf(store.getInt(KEY_STAGGER, 250))
        private set

    /**
     * How many candidate addresses a flow dials in parallel.
     *
     * One is the old serial dial and is the one-key rollback: with it, the kernel
     * behaves exactly as it did before racing existed. Three covers the measured
     * `github.com` shape (the first two candidates dead, the third live).
     */
    var raceWidth by mutableStateOf(store.getInt(KEY_RACE_WIDTH, 3))
        private set

    /**
     * Milliseconds between launching two candidates of the same race.
     *
     * 250, matching the kernel's own default for the same knob. The kernel takes
     * `max(race_launch_interval, connect_stagger)`, so a default of 150 here
     * would be sent to the kernel, folded up to 250 by that `max`, and leave the
     * setting showing a number the relay was not using.
     */
    var raceLaunchMillis by mutableStateOf(store.getInt(KEY_RACE_LAUNCH, 250))
        private set

    /** Upper bound on concurrent in-flight upstream dials across every flow. */
    var maxDialing by mutableStateOf(store.getInt(KEY_MAX_DIALING, 256))
        private set

    var dialNames by mutableStateOf(store.getBoolean(KEY_DIAL_NAMES, true))
        private set

    // --- kernel settings ------------------------------------------------------
    //
    // These reach `StackConfig` through the C ABI. The defaults are the kernel's
    // own, so a user who touches nothing gets exactly what the kernel was tuned
    // around — the point of exposing them is that the app's settings screen stops
    // being a list of switches that change nothing.

    /** Tunnel MTU. Lower costs throughput, higher risks fragmentation. */
    var mtu by mutableStateOf(store.getInt(KEY_MTU, 1500))
        private set

    /** How long a connect may take before the address is abandoned. */
    var connectTimeoutSeconds by mutableStateOf(store.getInt(KEY_CONNECT_TIMEOUT, 10))
        private set

    var tcpIdleSeconds by mutableStateOf(store.getInt(KEY_TCP_IDLE, 300))
        private set

    /** Also the floor for how long a silent UDP flow holds a descriptor. */
    var udpIdleSeconds by mutableStateOf(store.getInt(KEY_UDP_IDLE, 60))
        private set

    var maxTcpFlows by mutableStateOf(store.getInt(KEY_MAX_TCP, 512))
        private set

    var maxUdpFlows by mutableStateOf(store.getInt(KEY_MAX_UDP, 256))
        private set

    /** Answer DNS from the rule set instead of forwarding it. */
    var answerDnsFromRules by mutableStateOf(store.getBoolean(KEY_ANSWER_DNS, true))
        private set

    /** Learn address-to-domain mappings from forwarded answers. */
    var observeDns by mutableStateOf(store.getBoolean(KEY_OBSERVE_DNS, true))
        private set

    /**
     * Check a rule address's certificate against the domain before dialling it.
     *
     * **On by default.** Without it the address is chosen by an RTT race, so
     * whether a domain works is luck: measured on `github.com`, repeated runs
     * against the same rule set gave a working 200 and a TLS failure on
     * different attempts, because sometimes the fastest address was real GitHub
     * and sometimes it was an Azure edge serving someone else's certificate.
     *
     * With the check the choice is deterministic. The cost is one handshake per
     * (domain, address) pair, cached for ten minutes.
     */
    var certificateCheck by mutableStateOf(store.getBoolean(KEY_CERT_CHECK, true))
        private set

    /**
     * The rule switches the user has turned off.
     *
     * Flat keys rather than a tree, so that adding a level later does not change
     * the storage: `g:<group>`, `d:<domain>`, `a:<address>`. The prefixes are
     * parsed by the kernel's rule crate, which is also what applies them, so
     * there is one definition of what a key looks like.
     *
     * Stored as a `StringSet`, which `SharedPreferences` copies on read and write
     * — mutating the returned set would not persist, hence the explicit copy in
     * every mutator below.
     */
    var disabledRules by mutableStateOf(store.getStringSet(KEY_DISABLED, emptySet())?.toSet() ?: emptySet())
        private set

    fun disableRule(key: String) {
        disabledRules = disabledRules + key
        edit { putStringSet(KEY_DISABLED, disabledRules) }
    }

    fun enableRule(key: String) {
        disabledRules = disabledRules - key
        edit { putStringSet(KEY_DISABLED, disabledRules) }
    }

    fun setRuleEnabled(key: String, enabled: Boolean) {
        if (enabled) enableRule(key) else disableRule(key)
    }

    /** Switch a whole level back on, or all of it off. */
    fun replaceDisabledRules(keys: Set<String>) {
        disabledRules = keys
        edit { putStringSet(KEY_DISABLED, keys) }
    }

    /**
     * The kernel settings, as the flat JSON object the native side reads.
     *
     * Built here rather than passed field by field so that adding a knob is one
     * line in one place. Only the keys the kernel understands are emitted; the
     * rest of this screen is the shell's own business.
     *
     * [pinnedMtu] is not a convenience. The MTU is the only value here with a
     * second consumer outside the kernel — it also sizes the TUN interface — and
     * the tunnel start-up has to hand the *same number* to both. Reading the
     * property twice is not the same as reading it once: `updateMtu` can land
     * between the two reads, and that window is not hypothetical, because the
     * first read is immediately followed by the binder call that establishes the
     * tunnel. Passing the value in is how the caller pins it. The default is for
     * callers that only want a report and have no interface to agree with.
     */
    fun kernelSettingsJson(pinnedMtu: Int = mtu): String = buildString {
        append('{')
        append("\"mtu\":").append(pinnedMtu)
        append(",\"connect_timeout_seconds\":").append(connectTimeoutSeconds)
        append(",\"tcp_idle_seconds\":").append(tcpIdleSeconds)
        append(",\"udp_idle_seconds\":").append(udpIdleSeconds)
        append(",\"max_tcp_flows\":").append(maxTcpFlows)
        append(",\"max_udp_flows\":").append(maxUdpFlows)
        append(",\"answer_dns_from_rules\":").append(answerDnsFromRules)
        append(",\"observe_dns\":").append(observeDns)
        append(",\"certificate_check\":").append(certificateCheck)
        append(",\"max_candidates\":").append(maxCandidates)
        append(",\"connect_stagger_milliseconds\":").append(connectStaggerMillis)
        append(",\"race_width\":").append(raceWidth)
        append(",\"race_launch_milliseconds\":").append(raceLaunchMillis)
        append(",\"max_dialing\":").append(maxDialing)
        // The selector's failure cooldown. It does not reach `StackConfig`
        // directly; the kernel forwards it to `IpSelectorConfig` when it builds
        // the router, which is the only moment the selector can be configured.
        append(",\"failure_cooldown_seconds\":").append(failureCooldownSeconds)
        // Whether the kernel resolves the rules' dial names itself.
        append(",\"dial_names\":").append(dialNames)
        append('}')
    }

    /**
     * Bring stored settings forward.
     *
     * Each step describes "what the old build left behind" rather than "what the
     * user meant": a stored value equal to an old default is indistinguishable
     * from a deliberate choice, so only values that had no way to be chosen
     * deliberately are touched.
     *
     * The step that matters here is `certificate_check`. Before this round it had
     * no settings row at all, so a stored `false` could only ever be the old
     * (wrong) default — and that default is what made a correctly working tunnel
     * look broken, because it skipped the whole address-selection chain.
     *
     * Two invariants, both learned by getting this wrong once:
     *
     * * **The version counter records that code ran, not that the value is now
     *   right.** The first version wrote `CURRENT_VERSION` in the same pass it
     *   repaired, and gated the repair on `version < 2`. A device stamped 2 by
     *   that earlier build kept its stale `false` forever, because the guard
     *   that avoids repeating work is also the guard that prevents the repair.
     *   So the repair gets its own version bump rather than riding someone
     *   else's, and the bump happens only after the repair.
     * * **A setting that has since gained a UI must only be repaired once.**
     *   `certificate_check` now has a settings row, so from here on a stored
     *   `false` can be a real choice. Removing the key every launch would throw
     *   that choice away on every start, which is a worse bug than the one being
     *   fixed. Hence version 3: bring forward the devices the old code stranded,
     *   then leave the value alone forever.
     * * **A stored value naming something that no longer exists has to be
     *   renamed, not defaulted.** `rule_source` used to hold `hosts` / `json`,
     *   which named the *format* of a single document. The endpoints now publish
     *   two different rule sets, so `json` no longer names anything and the
     *   reader's `else` branch would quietly give that user "both documents"
     *   instead of "the smaller one" — a silent widening of what runs, which is
     *   the exact class of bug the migration exists to prevent. Version 4 renames
     *   it; an unknown value from some future build is left as-is rather than
     *   guessed at.
     * * **State that lives outside the prefs file still has to be migrated, and
     *   it is migrated by a flag.** The rule cache is a file, and versions 1–4
     *   wrote it with no record of which document it held, so on those devices it
     *   cannot be validated and must be discarded. `Prefs` does no file I/O (it
     *   is built lazily from `of()` on whatever thread first reads a setting), so
     *   version 5 sets `rules_cache_dirty` and lets `RulesRepository` — which
     *   already owns that file — do the delete. The flag is set rather than a key
     *   being removed precisely because "this flag is true" is a value no earlier
     *   build could have written, so the repair cannot misfire on a device that
     *   never needed it.
     * * **A changed default reaches nobody who already stored the old one, so it
     *   has to be carried forward — and that is the one repair that has to touch a
     *   value a user could have picked.** `refresh_hours` defaulted to 6; the owner
     *   wants daily, so the default is now 24. `SharedPreferences` returns a stored
     *   value over a changed default, so every existing install would keep pulling
     *   four times a day and the change would not reach the owner's own device.
     *   Version 6 therefore rewrites a stored 6 — but only 6, because 6 is the old
     *   default and any other number was chosen on purpose. The ambiguity is
     *   accepted rather than hidden: a stored 6 cannot be told apart from "never
     *   touched", the settings row stays adjustable so the choice is recoverable,
     *   and the version bump means the repair runs exactly once.
     * * **A repair that writes the store has to write the in-memory state too.**
     *   `of()` builds every property from the store and *then* calls this, so by
     *   the time a step here runs the old values are already loaded. An `edit {}`
     *   alone repairs the next launch and not this one — versions 5, 7 and 8 all
     *   have to assign their properties directly, or the upgrade process spends
     *   its whole life believing the stale value it just fixed on disk.
     * * **A stored value naming a model that no longer exists is removed, not
     *   translated.** Version 7 replaces `rule_source`'s three fixed option
     *   values with a list of sources ([RuleSource]). Two of the old values named
     *   the same document and the third named a retired endpoint, so none of them
     *   names anything the new model can fetch; the key is dropped and the default
     *   selected, rather than inventing a mapping that would select a dead URL.
     * * **An additive key whose default is already the correct behaviour needs no
     *   migration.** `applied_kernel_settings` defaults to `""`, which reads as
     *   "nothing applied" and therefore shows no banner — exactly what an upgraded
     *   install should do before its first connect — so there is no bump of
     *   `CURRENT_VERSION` here, because a step that changes nothing is ceremony
     *   that has to be read and trusted forever.
     */
    private fun migrate() {
        val version = store.getInt(KEY_VERSION, 1)

        if (version < 3) {
            // No row existed before, so any stored value is the old default.
            edit { remove(KEY_CERT_CHECK) }
        }

        if (version < 4) {
            // The key is spelled out rather than shared with a constant: it was
            // removed in version 7, and a constant for a key no current code reads
            // would outlive its only reason to exist.
            val source = store.getString("rule_source", "merged")
            if (source == "json") {
                edit { putString("rule_source", "s302") }
            }
        }

        if (version < 5) {
            // The cache written by every build up to now carries **no record of
            // which document it holds**, so it cannot be checked and has to be
            // thrown away rather than trusted. The device is the reason: a prefs
            // value of `hosts` next to a cached document that was actually the
            // `/2` s302 block — 862 entries, every address loopback — left the
            // tunnel relaying nothing for a whole refresh window while the UI
            // honestly reported `hosts`. `RulesRepository` now stamps the source
            // next to the document and treats a missing stamp as a miss, so
            // deleting is not strictly required for correctness; it is here so
            // the upgrade repairs the device on the **first** load instead of
            // serving the stale document until `refresh_hours` happens to elapse.
            edit { putBoolean(KEY_RULES_DIRTY, true) }
            // Writing the store is not enough for the process doing the upgrade.
            // `of()` constructs the properties from the store **first** and only
            // then calls `migrate()`, so `rulesCacheDirty` was already read as
            // `false` before this line ran; without the assignment the repair
            // would not take effect until the next cold start, which is exactly
            // the "fix that needs a restart nobody knows about" this migration
            // exists to avoid.
            rulesCacheDirty = true
        }

        if (version < 6) {
            // The refresh default moved from 6 hours (four fetches a day) to 24.
            //
            // This is the one repair here that deliberately rewrites a value a
            // user *could* have chosen, and that is a real trade-off rather than a
            // free one: 6 was both the old default and a value the settings row
            // could be set to by hand, so a stored 6 is genuinely ambiguous between
            // "never touched" and "chosen on purpose". It is rewritten anyway
            // because the owner asked for daily, `SharedPreferences` returns a
            // stored value over the new default, and without this every existing
            // install — the owner's own device included — would keep pulling four
            // times a day and the change would not take effect anywhere it already
            // ran. The blast radius is bounded by only touching the exact old
            // default: an 8, a 12, or anything else was chosen deliberately and is
            // left alone. The row stays adjustable, so anyone who wanted 6 can set
            // it back — and this runs once, so it will not fight them.
            if (store.getInt(KEY_REFRESH_HOURS, 6) == 6) {
                edit { putInt(KEY_REFRESH_HOURS, 24) }
            }
        }

        if (version < 7) {
            // `rule_source` used to hold one of three fixed option values, two of
            // which (`merged`, `hosts`) named the same document and the third of
            // which (`s302`) pointed at a retired endpoint. The model is a list of
            // sources now, and neither the old key nor its values mean anything to
            // it, so the old key is removed rather than translated.
            //
            // No value is carried forward: every old option resolved to a document
            // that is either the new default or gone, so "select the default" is
            // the only honest translation, and guessing that a stored `s302` meant
            // anything would select a source that cannot be fetched.
            //
            // The cache is marked dirty for the same reason v5 did: the document
            // on disk was fetched from an endpoint this build no longer knows, and
            // its sidecar identity will not match the new default's URL, so a load
            // without the flag would spend one refresh window failing to fetch
            // before repairing itself. The flag repairs it on the first load.
            edit {
                putString(KEY_RULE_SOURCES, RuleSource.encode(RuleSource.defaults()))
                putString(KEY_RULE_SOURCE_ID, RuleSource.HELLOGITHUB_HOSTS_ID)
                putBoolean(KEY_RULES_DIRTY, true)
                remove("rule_source")
            }
            // The in-memory half, for the same reason as v5: the properties were
            // read from the store before `migrate()` ran, so without this the
            // upgrade process would still be holding the old id and would stamp
            // the new cache with it.
            ruleSources = RuleSource.defaults()
            ruleSourceId = RuleSource.HELLOGITHUB_HOSTS_ID
            rulesCacheDirty = true
        }

        if (version < 8) {
            // The default source moved from `github-hosts` to HelloGitHub, and
            // this step exists because **a changed default does not reach anyone
            // who already has the old value stored**: `SharedPreferences` returns
            // the stored value over the new default, so without rewriting it every
            // existing install would keep fetching `github-hosts` and this change
            // would take effect nowhere it had already run. That is the same trap
            // v6's comment names for `refresh_hours`, and the same one v7 avoided
            // by writing the id rather than only changing the initialiser.
            //
            // The blast radius is bounded by rewriting **only the exact old
            // default**. Any other id was chosen deliberately and is left alone.
            // On an existing install, a stored `github-hosts` can only mean "never
            // changed it" or "picked it by hand" — and before this change
            // `github-hosts` was the *only* usable built-in, so "picked by hand"
            // and "got the default" are factually indistinguishable. Treating it
            // as the default is the only option that both makes the change take
            // effect and cannot misread a deliberate choice of something else.
            if (store.getString(KEY_RULE_SOURCE_ID, null) == RuleSource.GITHUB_HOSTS_ID) {
                edit { putString(KEY_RULE_SOURCE_ID, RuleSource.HELLOGITHUB_HOSTS_ID) }
                // The in-memory half again: `of()` reads the properties from the
                // store *before* calling `migrate()`, so without this the process
                // performing the upgrade would keep the old id and stamp the cache
                // with it (the same reason v5 and v7 assign it).
                ruleSourceId = RuleSource.HELLOGITHUB_HOSTS_ID
            }
            // The cache is deliberately **not** marked dirty here, unlike v5 and
            // v7. Both of those set the flag because the document on disk came
            // from an endpoint the new build no longer understood. v8 is a
            // different situation: the cached document is a perfectly usable
            // GitHub hosts file, merely from `maxiaof` rather than HelloGitHub,
            // and the two documents overlap heavily. The identity sidecar will
            // mismatch on its own (the source's URL changed, so
            // `RulesRepository` sees `identityMatches == false`, treats the cache
            // as absent, and refetches — falling back to the old cache only if
            // that fetch fails), so no flag is needed to make it refresh.
            //
            // Setting the flag would be actively harmful: `discardCache` deletes
            // the document, so a user who upgrades and is then offline would have
            // **no rules at all**. Leaving it alone is strictly better — try the
            // new source, and fall back to the old usable cache if that fails.
        }

        if (version < CURRENT_VERSION) {
            edit { putInt(KEY_VERSION, CURRENT_VERSION) }
        }
    }

    private fun edit(block: SharedPreferences.Editor.() -> Unit) {
        store.edit().apply(block).apply()
    }

    fun updateProxyPort(value: Int) { proxyPort = value.coerceIn(1024, 65535); edit { putInt(KEY_PROXY_PORT, proxyPort) } }

    /**
     * Select a source by id.
     *
     * A value that names nothing selectable is **ignored, not stored**. The old
     * version wrote whatever it was handed, so a shell could set a source that
     * did not exist and the tunnel would then fail at fetch time with the reason
     * buried in a log; refusing here keeps the stored id always resolvable.
     */
    fun updateRuleSource(id: String) {
        val source = ruleSources.firstOrNull { it.id == id && it.usable } ?: return
        ruleSourceId = source.id
        edit { putString(KEY_RULE_SOURCE_ID, source.id) }
    }

    /** Add a user-supplied source. Built-ins are already present and are not stored. */
    fun addRuleSource(source: RuleSource) {
        if (ruleSources.any { it.id == source.id }) return
        ruleSources = ruleSources + source
        edit { putString(KEY_RULE_SOURCES, RuleSource.encode(ruleSources)) }
    }

    /**
     * Remove a user-supplied source.
     *
     * Built-ins are refused: they are not in storage to begin with, so "removing"
     * one would only be undone by the next [RuleSource.decode]. If the deleted
     * source was selected, the selection falls back to the first selectable one —
     * otherwise the stored id would name something gone and [selectedSource]
     * would have to lean on its fallback chain for a state the user just caused.
     */
    fun removeRuleSource(id: String) {
        val target = ruleSources.firstOrNull { it.id == id } ?: return
        if (target.builtin) return
        ruleSources = ruleSources.filterNot { it.id == id }
        edit { putString(KEY_RULE_SOURCES, RuleSource.encode(ruleSources)) }
        if (ruleSourceId == id) {
            val fallback = ruleSources.firstOrNull { it.usable } ?: return
            ruleSourceId = fallback.id
            edit { putString(KEY_RULE_SOURCE_ID, fallback.id) }
        }
    }
    fun updateRefreshHours(value: Int) { refreshHours = value; edit { putInt(KEY_REFRESH_HOURS, value) } }
    fun updateOffline(value: Boolean) { offline = value; edit { putBoolean(KEY_OFFLINE, value) } }
    fun updateAutoConnect(value: Boolean) { autoConnect = value; edit { putBoolean(KEY_AUTO_CONNECT, value) } }
    fun updateUpdateUrl(value: String) { updateUrl = value.trim(); edit { putString(KEY_UPDATE_URL, updateUrl) } }
    fun updateStatsInterval(value: Int) { statsIntervalSeconds = value; edit { putInt(KEY_STATS_INTERVAL, value) } }
    fun updateLogArchive(value: Boolean) { logArchive = value; edit { putBoolean(KEY_LOG_ARCHIVE, value) } }
    fun updateDynamicColor(value: Boolean) { dynamicColor = value; edit { putBoolean(KEY_DYNAMIC_COLOR, value) } }
    fun updateDarkMode(value: String) { darkMode = value; edit { putString(KEY_DARK_MODE, value) } }
    fun updateDeveloperView(value: Boolean) { developerView = value; edit { putBoolean(KEY_DEVELOPER_VIEW, value) } }
    fun updateFailureCooldown(value: Int) { failureCooldownSeconds = value; edit { putInt(KEY_COOLDOWN, value) } }
    fun updateMaxCandidates(value: Int) { maxCandidates = value; edit { putInt(KEY_MAX_CANDIDATES, value) } }
    fun updateConnectStagger(value: Int) { connectStaggerMillis = value; edit { putInt(KEY_STAGGER, value) } }
    fun updateRaceWidth(value: Int) { raceWidth = value.coerceIn(1, 4); edit { putInt(KEY_RACE_WIDTH, raceWidth) } }
    fun updateRaceLaunch(value: Int) {
        raceLaunchMillis = value.coerceIn(0, 1000); edit { putInt(KEY_RACE_LAUNCH, raceLaunchMillis) }
    }
    fun updateMaxDialing(value: Int) {
        maxDialing = value.coerceIn(16, 1024); edit { putInt(KEY_MAX_DIALING, maxDialing) }
    }
    fun updateDialNames(value: Boolean) { dialNames = value; edit { putBoolean(KEY_DIAL_NAMES, value) } }

    fun updateMode(value: String) { mode = value; edit { putString(KEY_MODE, value) } }

    fun updateThemeColor(value: String) { themeColor = value; edit { putString(KEY_THEME_COLOR, value) } }
    fun updateCornerStyle(value: String) { cornerStyle = value; edit { putString(KEY_CORNER_STYLE, value) } }
    fun updateGlassLiquid(value: Boolean) { glassLiquid = value; edit { putBoolean(KEY_GLASS_LIQUID, value) } }
    fun updateGlassFrost(value: Boolean) { glassFrost = value; edit { putBoolean(KEY_GLASS_FROST, value) } }
    fun updateWallpaper(value: String) { wallpaper = value; edit { putString(KEY_WALLPAPER, value) } }
    fun updateWallpaperScrim(value: Int) {
        wallpaperScrim = value.coerceIn(0, 100); edit { putInt(KEY_WALLPAPER_SCRIM, wallpaperScrim) }
    }
    fun updateGlassBlur(value: Int) {
        glassBlur = value.coerceIn(0, 100); edit { putInt(KEY_GLASS_BLUR, glassBlur) }
    }
    fun updateGlassTint(value: Int) {
        glassTint = value.coerceIn(0, 100); edit { putInt(KEY_GLASS_TINT, glassTint) }
    }
    fun updateGlassLens(value: Int) {
        glassLens = value.coerceIn(0, 150); edit { putInt(KEY_GLASS_LENS, glassLens) }
    }
    fun updateGlassHighlight(value: Int) {
        glassHighlight = value.coerceIn(0, 100); edit { putInt(KEY_GLASS_HIGHLIGHT, glassHighlight) }
    }
    fun updateGlassBorder(value: Int) {
        glassBorder = value.coerceIn(0, 100); edit { putInt(KEY_GLASS_BORDER, glassBorder) }
    }
    fun updateFontScale(value: Float) {
        fontScale = value.coerceIn(0.85f, 1.3f); edit { putFloat(KEY_FONT_SCALE, fontScale) }
    }
    fun updateHomeShowRate(value: Boolean) { homeShowRate = value; edit { putBoolean(KEY_HOME_RATE, value) } }
    fun updateConfirmDisconnect(value: Boolean) {
        confirmDisconnect = value; edit { putBoolean(KEY_CONFIRM_DISCONNECT, value) }
    }

    fun updateMtu(value: Int) { mtu = value.coerceIn(576, 9000); edit { putInt(KEY_MTU, mtu) } }
    fun updateConnectTimeout(value: Int) {
        connectTimeoutSeconds = value.coerceIn(1, 120); edit { putInt(KEY_CONNECT_TIMEOUT, connectTimeoutSeconds) }
    }
    fun updateTcpIdle(value: Int) { tcpIdleSeconds = value.coerceIn(10, 3600); edit { putInt(KEY_TCP_IDLE, tcpIdleSeconds) } }
    fun updateUdpIdle(value: Int) { udpIdleSeconds = value.coerceIn(5, 600); edit { putInt(KEY_UDP_IDLE, udpIdleSeconds) } }
    fun updateMaxTcpFlows(value: Int) { maxTcpFlows = value.coerceIn(16, 8192); edit { putInt(KEY_MAX_TCP, maxTcpFlows) } }
    fun updateMaxUdpFlows(value: Int) { maxUdpFlows = value.coerceIn(16, 8192); edit { putInt(KEY_MAX_UDP, maxUdpFlows) } }
    fun updateAnswerDns(value: Boolean) { answerDnsFromRules = value; edit { putBoolean(KEY_ANSWER_DNS, value) } }
    fun updateObserveDns(value: Boolean) { observeDns = value; edit { putBoolean(KEY_OBSERVE_DNS, value) } }
    fun updateCertificateCheck(value: Boolean) {
        certificateCheck = value; edit { putBoolean(KEY_CERT_CHECK, value) }
    }

    /**
     * Put every setting back to the value a first run would have.
     *
     * **Why the setters and not `store.edit().clear()`.** `clear()` would also
     * drop two keys that are not "settings" at all, and each would be a bug:
     *
     * * `applied_kernel_settings` records what the *running* engine was built
     *   from, and it is the only thing that lets the "内核参数已修改 / 应用并重连"
     *   banner tell the truth. Wiping it would make a real pending change silently
     *   invisible after a restore — the user would move a row, see no banner, and
     *   believe the change had already been applied.
     * * `mode` is the tunnel kind, and under [BuildFlags.TUN_ONLY] the picker that
     *   would change it is hidden. A restore that silently switched the mode would
     *   be a surprise with no control left to undo it.
     *
     * So each property is set through its own setter, which is already the
     * assign-and-persist pair, keeping the in-memory singleton and the store in
     * step. The values are copied from the property declarations rather than the
     * sliders' ranges — the two are not the same (the MTU row steps by 100 while
     * `updateMtu` accepts anything in 576..9000, and the font-scale row clamps to
     * the same 0.85..1.3 the setter does, but nothing guarantees that stays true).
     *
     * The version is rewritten last so a restore does not look like an old install
     * and make [migrate] run again.
     */
    fun restoreDefaults() {
        // Connection.
        updateProxyPort(DEFAULT_PROXY_PORT)
        updateRefreshHours(24)
        updateOffline(false)
        updateAutoConnect(false)
        updateUpdateUrl("")
        // Logs.
        updateStatsInterval(5)
        updateLogArchive(true)
        updateDeveloperView(false)
        // Appearance.
        updateDynamicColor(true)
        updateDarkMode("follow")
        updateThemeColor("dynamic")
        updateCornerStyle("medium")
        updateGlassLiquid(false)
        updateGlassFrost(false)
        // Clears the selection only; `Prefs` has no file I/O by design, so the
        // bytes in `filesDir` are pruned by `WallpaperStore` on its next load.
        updateWallpaper("")
        updateWallpaperScrim(DEFAULT_WALLPAPER_SCRIM)
        // The glass numbers go back to the sane defaults with everything else.
        // They are reset even though both switches are reset off, so that turning
        // a switch back on later starts from the no-jump defaults rather than from
        // whatever the previous owner of the device left behind.
        updateGlassBlur(DEFAULT_GLASS_BLUR)
        updateGlassTint(DEFAULT_GLASS_TINT)
        updateGlassLens(DEFAULT_GLASS_LENS)
        updateGlassHighlight(DEFAULT_GLASS_HIGHLIGHT)
        updateGlassBorder(DEFAULT_GLASS_BORDER)
        updateFontScale(1f)
        updateHomeShowRate(true)
        updateConfirmDisconnect(true)
        // Advanced / kernel. These are the kernel's own defaults, so a restore
        // puts the engine back on the configuration it was tuned around.
        updateMaxCandidates(12)
        updateConnectStagger(250)
        updateRaceWidth(3)
        updateRaceLaunch(250)
        updateMaxDialing(256)
        updateFailureCooldown(60)
        updateDialNames(true)
        updateMtu(1500)
        updateConnectTimeout(10)
        updateTcpIdle(300)
        updateUdpIdle(60)
        updateMaxTcpFlows(512)
        updateMaxUdpFlows(256)
        updateAnswerDns(true)
        updateObserveDns(true)
        updateCertificateCheck(true)

        // The two settings that are not scalars. `replaceDisabledRules` is the
        // existing mutator and already writes the set through.
        replaceDisabledRules(emptySet())
        // Same shape as `migrate()`'s version-7 step: built-ins are never stored,
        // so `encode` writes the (empty) custom list, and the selection goes back
        // to the built-in default.
        ruleSources = RuleSource.defaults()
        ruleSourceId = RuleSource.HELLOGITHUB_HOSTS_ID
        edit {
            putString(KEY_RULE_SOURCES, RuleSource.encode(RuleSource.defaults()))
            putString(KEY_RULE_SOURCE_ID, RuleSource.HELLOGITHUB_HOSTS_ID)
        }

        // Stamping the current version here only stops `migrate()` from running
        // again; it is not a default change.
        edit { putInt(KEY_VERSION, CURRENT_VERSION) }
    }

    companion object {
        private const val STORE = "detour.prefs"

        const val DEFAULT_PROXY_PORT = 1080

        private const val KEY_PROXY_PORT = "proxy_port"

        /** The user-added rule sources, as JSON. Built-ins live in [RuleSource]. */
        private const val KEY_RULE_SOURCES = "rule_sources"

        /** Which source is selected, by id. */
        private const val KEY_RULE_SOURCE_ID = "rule_source_id"

        private const val KEY_RULES_DIRTY = "rules_cache_dirty"
        private const val KEY_REFRESH_HOURS = "refresh_hours"
        private const val KEY_OFFLINE = "offline"
        private const val KEY_STATS_INTERVAL = "stats_interval"
        private const val KEY_LOG_ARCHIVE = "log_archive"
        private const val KEY_DYNAMIC_COLOR = "dynamic_color"
        private const val KEY_DARK_MODE = "dark_mode"
        private const val KEY_DEVELOPER_VIEW = "developer_view"
        private const val KEY_COOLDOWN = "failure_cooldown"
        private const val KEY_MAX_CANDIDATES = "max_candidates"
        private const val KEY_STAGGER = "connect_stagger"
        private const val KEY_RACE_WIDTH = "race_width"
        private const val KEY_RACE_LAUNCH = "race_launch"
        private const val KEY_MAX_DIALING = "max_dialing"
        private const val KEY_DIAL_NAMES = "dial_names"
        private const val KEY_DISABLED = "disabled_rules"
        private const val KEY_MTU = "mtu"
        private const val KEY_CONNECT_TIMEOUT = "connect_timeout"
        private const val KEY_TCP_IDLE = "tcp_idle"
        private const val KEY_UDP_IDLE = "udp_idle"
        private const val KEY_MAX_TCP = "max_tcp_flows"
        private const val KEY_MAX_UDP = "max_udp_flows"
        private const val KEY_ANSWER_DNS = "answer_dns"
        private const val KEY_OBSERVE_DNS = "observe_dns"
        private const val KEY_CERT_CHECK = "certificate_check"
        private const val KEY_APPLIED_SETTINGS = "applied_kernel_settings"

        /**
         * The layout version of the stored settings.
         *
         * Bumped whenever a **default changes**, because a changed default does
         * not reach anyone who already has the old value stored.
         * `SharedPreferences` returns what is stored when a key exists, so
         * editing a default only affects installs that never wrote the key.
         */
        private const val KEY_VERSION = "prefs_version"

        /**
         * 1: everything before the certificate check. 2: the check is on. 3: the
         * check is on and repaired once. 4: `rule_source`'s dead `json` value is
         * renamed to `s302`. 5: the rule cache written by 1–4 carries no identity,
         * so it is marked for refetch. 6: `refresh_hours`' default moved from 6
         * hours to 24, and a stored 6 is carried forward. 7: `rule_source`'s fixed
         * option values are replaced by a list of [RuleSource]s, and the old key is
         * removed. 8: the default source moved from `github-hosts` to HelloGitHub,
         * and a stored `github-hosts` — the old default — is carried forward while
         * every other id is left alone.
         *
         * Public rather than private because it is also the `version` field of the
         * settings-export document: `SettingsBackup` stamps it on export and
         * refuses a document whose version is newer than this, so both directions
         * have to read the same number. A second copy over there would be a copy
         * that drifts the first time this one is bumped.
         */
        const val CURRENT_VERSION = 8
        private const val KEY_THEME_COLOR = "theme_color"
        private const val KEY_CORNER_STYLE = "corner_style"

        // The two glass switches. No version bump guards them: a missing boolean
        // key reads its default (`false`), so an install from before they existed
        // simply gets the both-off state, which is the correct starting point —
        // the same reasoning the numeric keys below record.
        private const val KEY_GLASS_LIQUID = "glass_liquid"
        private const val KEY_GLASS_FROST = "glass_frost"

        // The glass material's five numbers. No version bump guards these either:
        // a missing key reads its default.
        private const val KEY_GLASS_BLUR = "glass_blur"
        private const val KEY_GLASS_TINT = "glass_tint"
        private const val KEY_GLASS_LENS = "glass_lens"
        private const val KEY_GLASS_HIGHLIGHT = "glass_highlight"
        private const val KEY_GLASS_BORDER = "glass_border"

        // The glass defaults. Named rather than inlined because each is written
        // twice — the property initialiser and `restoreDefaults` — and a default
        // spelled out in two places is a default that drifts. The values are the
        // old `high` preset's numbers on purpose; see the property block for why.
        private const val DEFAULT_GLASS_BLUR = 16
        private const val DEFAULT_GLASS_TINT = 72
        private const val DEFAULT_GLASS_LENS = 32
        private const val DEFAULT_GLASS_HIGHLIGHT = 12
        private const val DEFAULT_GLASS_BORDER = 50

        /** Identity of the chosen background photo; `""` means none. */
        private const val KEY_WALLPAPER = "wallpaper"

        /** Scrim strength over the wallpaper, as a percent (0..100). */
        private const val KEY_WALLPAPER_SCRIM = "wallpaper_scrim"

        /**
         * The scrim's default, as a percent.
         *
         * Public and in one place on purpose. It is needed by [restoreDefaults],
         * by the property initialiser above, and by `WallpaperStore.clear` — and
         * a default written out three times is a default that will drift. It
         * already has a reason to be reset outside the settings screen: clearing
         * the wallpaper must reset the scrim too, or the next photo renders
         * behind a stale scrim and looks like it did not load (see
         * `WallpaperStore.clear`).
         */
        const val DEFAULT_WALLPAPER_SCRIM = 58

        private const val KEY_FONT_SCALE = "font_scale"
        private const val KEY_HOME_RATE = "home_rate"
        private const val KEY_CONFIRM_DISCONNECT = "confirm_disconnect"
    private const val KEY_MODE = "mode"
    private const val KEY_AUTO_CONNECT = "auto_connect"

    /** URL of the update version manifest; `""` means "do not check". */
    private const val KEY_UPDATE_URL = "update_url"

        @Volatile
        private var instance: Prefs? = null

        fun of(context: Context): Prefs = instance ?: synchronized(this) {
            instance ?: Prefs(
                context.applicationContext.getSharedPreferences(STORE, Context.MODE_PRIVATE),
            ).also {
                it.migrate()
                instance = it
            }
        }

        /** Read a setting without needing an instance, for the service. */
        fun ruleSource(context: Context): String =
            of(context).ruleSource

        fun offline(context: Context): Boolean = of(context).offline

        fun developerView(context: Context): Boolean = of(context).developerView
    }
}
