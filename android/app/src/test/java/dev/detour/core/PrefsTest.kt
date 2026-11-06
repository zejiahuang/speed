package dev.detour.core

import android.content.SharedPreferences
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Settings behaviour that does not need a device: defaults, the clamps every
 * numeric row goes through, persistence, and the version-9 migration.
 *
 * **Why a fake store instead of Robolectric.** Everything exercised here is
 * `SharedPreferences` semantics — get, put, remove — and a fake implements that
 * contract directly. The alternative, a real `org.json`, is rejected elsewhere in
 * this test suite for the same reason it is rejected here: it would assert against
 * a different implementation than the one the app runs.
 *
 * **What is deliberately absent.** Anything that reaches `org.json` is not tested
 * from the JVM: `kernelSettingsJson` quotes strings through `JSONObject`, so a
 * unit test would hit AGP's stub and throw. That path is exercised on a device.
 * The migration test starts at version 8 for the same reason — versions below 7
 * write `RuleSource.encode`, which builds a `JSONArray`.
 */
class PrefsTest {

    private fun prefs(vararg seeded: Pair<String, Any>) = Prefs(FakeSharedPreferences(seeded.toMap()))

    @Test
    fun aFreshInstallGetsTheDocumentedDefaults() {
        val prefs = prefs()

        assertEquals(Prefs.DEFAULT_PROXY_PORT, prefs.proxyPort)
        assertEquals("proxy", prefs.mode)
        assertEquals(24, prefs.refreshHours)
        assertEquals(12, prefs.maxCandidates)
        assertEquals(3, prefs.raceWidth)
        assertEquals(1500, prefs.mtu)
        assertFalse(prefs.glassEnabled)
        assertTrue(prefs.dynamicColor)
        assertFalse(prefs.developerView)
        assertTrue(prefs.disabledRules.isEmpty())
        // The default resolver is a preset with an address, so it is in use
        // before the user touches anything.
        assertTrue(prefs.dnsUpstreamActive)
    }

    @Test
    fun theNumericRowsClampToTheirDeclaredRanges() {
        val prefs = prefs()

        prefs.updateProxyPort(80)
        assertEquals(1024, prefs.proxyPort)
        prefs.updateProxyPort(70000)
        assertEquals(65535, prefs.proxyPort)

        prefs.updateRaceWidth(0)
        assertEquals(1, prefs.raceWidth)
        prefs.updateRaceWidth(9)
        assertEquals(4, prefs.raceWidth)

        prefs.updateRaceLaunch(-5)
        assertEquals(0, prefs.raceLaunchMillis)
        prefs.updateRaceLaunch(5000)
        assertEquals(1000, prefs.raceLaunchMillis)

        prefs.updateMaxDialing(1)
        assertEquals(16, prefs.maxDialing)
        prefs.updateMaxDialing(5000)
        assertEquals(1024, prefs.maxDialing)

        prefs.updateMtu(100)
        assertEquals(576, prefs.mtu)
        prefs.updateMtu(20000)
        assertEquals(9000, prefs.mtu)

        prefs.updateConnectTimeout(0)
        assertEquals(1, prefs.connectTimeoutSeconds)
        prefs.updateConnectTimeout(500)
        assertEquals(120, prefs.connectTimeoutSeconds)

        prefs.updateTcpIdle(0)
        assertEquals(10, prefs.tcpIdleSeconds)
        prefs.updateTcpIdle(9999)
        assertEquals(3600, prefs.tcpIdleSeconds)

        prefs.updateUdpIdle(0)
        assertEquals(5, prefs.udpIdleSeconds)
        prefs.updateUdpIdle(999)
        assertEquals(600, prefs.udpIdleSeconds)

        prefs.updateMaxTcpFlows(1)
        assertEquals(16, prefs.maxTcpFlows)
        prefs.updateMaxTcpFlows(99999)
        assertEquals(8192, prefs.maxTcpFlows)

        prefs.updateMaxUdpFlows(1)
        assertEquals(16, prefs.maxUdpFlows)
        prefs.updateMaxUdpFlows(99999)
        assertEquals(8192, prefs.maxUdpFlows)
    }

    @Test
    fun theAppearanceRowsClampAndSurviveADaemonRestart() {
        val store = FakeSharedPreferences()
        val prefs = Prefs(store)

        prefs.updateWallpaperScrim(-1)
        assertEquals(0, prefs.wallpaperScrim)
        prefs.updateWallpaperScrim(101)
        assertEquals(100, prefs.wallpaperScrim)

        prefs.updateGlassBlur(101)
        assertEquals(100, prefs.glassBlur)
        prefs.updateGlassTint(-1)
        assertEquals(0, prefs.glassTint)
        prefs.updateGlassLens(999)
        assertEquals(150, prefs.glassLens)
        prefs.updateGlassHighlight(-1)
        assertEquals(0, prefs.glassHighlight)
        prefs.updateGlassBorder(101)
        assertEquals(100, prefs.glassBorder)

        prefs.updateFontScale(0.1f)
        assertEquals(0.85f, prefs.fontScale, 0.0001f)
        prefs.updateFontScale(3f)
        assertEquals(1.3f, prefs.fontScale, 0.0001f)

        // The clamp is not only in memory: a new instance reads the stored value,
        // which is what the next launch will see.
        val reopened = Prefs(store)
        assertEquals(150, reopened.glassLens)
        assertEquals(1.3f, reopened.fontScale, 0.0001f)
    }

    @Test
    fun theUpstreamExitIsOnlyActiveWhenItIsOnAndDialable() {
        val prefs = prefs()

        assertFalse("nothing switched on yet", prefs.upstreamProxyActive)
        prefs.updateUpstreamProxyAddress("proxy.example.com:1080")
        assertFalse("a name cannot be dialled", prefs.upstreamProxyConfigured)
        prefs.updateUpstreamProxyEnabled(true)
        assertFalse("an undialable endpoint is not an exit", prefs.upstreamProxyActive)

        prefs.updateUpstreamProxyAddress("  203.0.113.7:1080  ")
        assertEquals("the address is stored trimmed", "203.0.113.7:1080", prefs.upstreamProxyAddress)
        assertTrue(prefs.upstreamProxyConfigured)
        assertTrue(prefs.upstreamProxyActive)

        prefs.updateUpstreamProxyEnabled(false)
        assertFalse("switched off means off", prefs.upstreamProxyActive)
    }

    @Test
    fun theUpstreamResolverNeedsAnAddressForANamedUrl() {
        val prefs = prefs()

        prefs.updateDnsUpstream("https://dns.alidns.com/dns-query", "")
        assertFalse(prefs.dnsUpstreamConfigured)
        assertFalse(prefs.dnsUpstreamActive)

        prefs.updateDnsUpstream("https://dns.alidns.com/dns-query", "223.5.5.5")
        assertTrue(prefs.dnsUpstreamConfigured)
        assertTrue(prefs.dnsUpstreamActive)

        // An address-shaped URL needs no bootstrap, and both halves are cleared
        // in one write.
        prefs.updateDnsUpstream("https://1.1.1.1/dns-query", "")
        assertTrue(prefs.dnsUpstreamActive)
        prefs.updateDnsUpstream("", "")
        assertFalse(prefs.dnsUpstreamActive)
    }

    @Test
    fun disabledRulesAreAdditiveAndPersisted() {
        val store = FakeSharedPreferences()
        val prefs = Prefs(store)

        prefs.disableRule("a.example")
        prefs.disableRule("b.example")
        assertEquals(setOf("a.example", "b.example"), prefs.disabledRules)

        prefs.enableRule("a.example")
        assertEquals(setOf("b.example"), prefs.disabledRules)

        // `setRuleEnabled(key, enabled)` speaks in the positive sense: true means
        // "not disabled", so it removes the key from the disabled set.
        prefs.setRuleEnabled("b.example", true)
        assertTrue(prefs.disabledRules.isEmpty())
        prefs.setRuleEnabled("c.example", false)
        assertEquals(setOf("c.example"), prefs.disabledRules)

        // Replacing is a replacement, not a union — a whole level switched back
        // on has to clear what the previous level disabled.
        prefs.replaceDisabledRules(setOf("d.example"))
        assertEquals(setOf("d.example"), prefs.disabledRules)

        assertEquals(setOf("d.example"), Prefs(store).disabledRules)
    }

    @Test
    fun theCacheDirtyFlagIsConsumedExactlyOnce() {
        val store = FakeSharedPreferences(mapOf("rules_cache_dirty" to true))
        val prefs = Prefs(store)

        assertTrue(prefs.consumeRulesCacheDirty())
        assertFalse(prefs.consumeRulesCacheDirty())
        assertFalse(store.getBoolean("rules_cache_dirty", true))
    }

    @Test
    fun anAppliedDocumentIsOnlyPendingAfterAChange() {
        val prefs = prefs()
        // Clear the resolver first: the default preset is active, and quoting it
        // goes through `JSONObject`, which a JVM unit test cannot call.
        prefs.updateDnsUpstream("", "")

        assertFalse("nothing applied yet", prefs.kernelSettingsPending())
        prefs.recordAppliedKernelSettings(prefs.kernelSettingsJson())
        assertFalse(prefs.kernelSettingsPending())

        prefs.updateMtu(1400)
        assertTrue(prefs.kernelSettingsPending())
    }

    @Test
    fun sourceSelectionOnlyAcceptsASelectableId() {
        val prefs = prefs()
        val builtin = RuleSource.defaults().first()

        prefs.updateRuleSource(builtin.id)
        assertEquals(builtin.id, prefs.ruleSourceId)

        prefs.updateRuleSource("does-not-exist")
        assertEquals("an unknown id must not be stored", builtin.id, prefs.ruleSourceId)
    }

    @Test
    fun theUpdateChannelCollapsesEverythingButBeta() {
        val prefs = prefs()

        assertEquals("stable", prefs.updateChannel)
        prefs.updateUpdateChannel("beta")
        assertEquals("beta", prefs.updateChannel)
        prefs.updateUpdateChannel("nightly")
        assertEquals("stable", prefs.updateChannel)
    }

    @Test
    fun migrationMergesTheTwoGlassSwitchesIntoOne() {
        val store = FakeSharedPreferences(
            mapOf(
                "prefs_version" to 8,
                "glass_liquid" to true,
                "glass_frost" to false,
            ),
        )
        val prefs = Prefs(store)
        prefs.migrate()

        assertTrue("either half on means on", prefs.glassEnabled)
        assertFalse(store.contains("glass_liquid"))
        assertFalse(store.contains("glass_frost"))
        assertEquals(Prefs.CURRENT_VERSION, store.getInt("prefs_version", 0))
    }

    @Test
    fun migrationKeepsGlassOffWhenNeitherHalfWasOn() {
        val store = FakeSharedPreferences(
            mapOf(
                "prefs_version" to 8,
                "glass_liquid" to false,
                "glass_frost" to false,
            ),
        )
        val prefs = Prefs(store)
        prefs.migrate()

        assertFalse(prefs.glassEnabled)
        assertFalse(store.contains("glass_liquid"))
        assertFalse(store.contains("glass_frost"))
        assertEquals(Prefs.CURRENT_VERSION, store.getInt("prefs_version", 0))
    }
}

/**
 * An in-memory `SharedPreferences`.
 *
 * Only the parts `Prefs` uses are implemented for real; every method is the
 * contract the app relies on, which is what makes the tests above test `Prefs`
 * rather than the fake.
 */
private class FakeSharedPreferences(
    initial: Map<String, Any> = emptyMap(),
) : SharedPreferences {

    private val values = HashMap<String, Any>(initial)

    override fun getAll(): MutableMap<String, *> = HashMap(values)

    override fun getString(key: String, defValue: String?): String? =
        values[key] as? String ?: defValue

    override fun getStringSet(key: String, defValues: MutableSet<String>?): MutableSet<String>? =
        (values[key] as? Set<*>)?.filterIsInstance<String>()?.toMutableSet() ?: defValues

    override fun getInt(key: String, defValue: Int): Int = values[key] as? Int ?: defValue

    override fun getLong(key: String, defValue: Long): Long = values[key] as? Long ?: defValue

    override fun getFloat(key: String, defValue: Float): Float = values[key] as? Float ?: defValue

    override fun getBoolean(key: String, defValue: Boolean): Boolean =
        values[key] as? Boolean ?: defValue

    override fun contains(key: String): Boolean = values.containsKey(key)

    override fun edit(): SharedPreferences.Editor = FakeEditor()

    override fun registerOnSharedPreferenceChangeListener(
        listener: SharedPreferences.OnSharedPreferenceChangeListener,
    ) = Unit

    override fun unregisterOnSharedPreferenceChangeListener(
        listener: SharedPreferences.OnSharedPreferenceChangeListener,
    ) = Unit

    private inner class FakeEditor : SharedPreferences.Editor {
        private val pending = HashMap<String, Any>()
        private val removals = HashSet<String>()
        private var clearAll = false

        override fun putString(key: String, value: String?): SharedPreferences.Editor {
            if (value == null) removals += key else pending[key] = value
            return this
        }

        override fun putStringSet(key: String, values: MutableSet<String>?): SharedPreferences.Editor {
            if (values == null) removals += key else pending[key] = values.toSet()
            return this
        }

        override fun putInt(key: String, value: Int): SharedPreferences.Editor {
            pending[key] = value
            return this
        }

        override fun putLong(key: String, value: Long): SharedPreferences.Editor {
            pending[key] = value
            return this
        }

        override fun putFloat(key: String, value: Float): SharedPreferences.Editor {
            pending[key] = value
            return this
        }

        override fun putBoolean(key: String, value: Boolean): SharedPreferences.Editor {
            pending[key] = value
            return this
        }

        override fun remove(key: String): SharedPreferences.Editor {
            removals += key
            return this
        }

        override fun clear(): SharedPreferences.Editor {
            clearAll = true
            return this
        }

        override fun commit(): Boolean {
            write()
            return true
        }

        override fun apply() = write()

        private fun write() {
            if (clearAll) values.clear()
            removals.forEach { values.remove(it) }
            values.putAll(pending)
        }
    }
}
