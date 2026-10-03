package dev.detour.core

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The upstream exit's endpoint gate.
 *
 * **Why this is a unit test and not a device measurement.** `Prefs.isProxyEndpoint`
 * is pure — it parses a string and answers a question — so nothing about it needs
 * an emulator. It exists at all because the switch that turns the exit on is
 * drawn only when this returns true, and the whole point of that gate is that an
 * endpoint the kernel cannot dial never gets a control: the kernel reads the
 * endpoint once, when the engine is built, and would drop a name or a missing port
 * with nothing on screen to say so.
 *
 * So each case below is one of two bugs. A false negative hides the switch for an
 * endpoint that would have worked; a false positive shows a switch that does
 * nothing. The second is the worse of the two, which is why the malformed cases
 * outnumber the valid ones.
 *
 * The valid cases are written to match `SocketAddr::from_str`, which is what the
 * kernel actually calls — including its refusal of leading zeros, which is the
 * rule most likely to be got wrong by a hand-written parser.
 */
class PrefsEndpointTest {

    @Test
    fun aLiteralIpv4EndpointIsAccepted() {
        assertTrue(Prefs.isProxyEndpoint("203.0.113.7:1080"))
        assertTrue(Prefs.isProxyEndpoint("127.0.0.1:1"))
        assertTrue(Prefs.isProxyEndpoint("0.0.0.0:65535"))
    }

    @Test
    fun surroundingWhitespaceIsIgnored() {
        // The setter trims, but the gate is also reachable from a restored or
        // hand-edited value, so it trims too rather than reporting a trailing
        // space as a broken endpoint.
        assertTrue(Prefs.isProxyEndpoint("  203.0.113.7:1080  "))
    }

    @Test
    fun aBracketedIpv6EndpointIsAccepted() {
        assertTrue(Prefs.isProxyEndpoint("[::1]:1080"))
        assertTrue(Prefs.isProxyEndpoint("[2001:db8::1]:8080"))
    }

    @Test
    fun anIpv6ZoneIdentifierIsRefused() {
        // Recorded rather than recommended. `fe80::1%wlan0` is a real way to write
        // a link-local address, but the kernel's parser does not take it and this
        // one does not either — a gate that accepted it would offer a switch for an
        // endpoint the kernel drops, which is the failure this whole function is
        // here to prevent.
        assertFalse(Prefs.isProxyEndpoint("[fe80::1%wlan0]:1080"))
    }

    @Test
    fun aNameIsRefused() {
        // The reason the gate exists. The kernel dials a `SocketAddr` and has no
        // resolver for this endpoint, so a name would be silently dropped.
        assertFalse(Prefs.isProxyEndpoint("proxy.example.com:1080"))
        assertFalse(Prefs.isProxyEndpoint("localhost:1080"))
        assertFalse(Prefs.isProxyEndpoint("203.0.113.7.nip.io:1080"))
    }

    @Test
    fun aMissingPortIsRefused() {
        assertFalse(Prefs.isProxyEndpoint("203.0.113.7"))
        assertFalse(Prefs.isProxyEndpoint("203.0.113.7:"))
        assertFalse(Prefs.isProxyEndpoint("[::1]"))
        assertFalse(Prefs.isProxyEndpoint(""))
    }

    @Test
    fun anOutOfRangePortIsRefused() {
        assertFalse(Prefs.isProxyEndpoint("203.0.113.7:0"))
        assertFalse(Prefs.isProxyEndpoint("203.0.113.7:65536"))
        assertFalse(Prefs.isProxyEndpoint("203.0.113.7:99999"))
        assertFalse(Prefs.isProxyEndpoint("203.0.113.7:80a"))
    }

    @Test
    fun aLeadingZeroInThePortIsRefused() {
        // `SocketAddr::from_str` refuses `:080` — Rust's port parser does not
        // accept leading zeros — so accepting it here would be a gate looser than
        // the thing it gates, which is the one way this function can be wrong in
        // the direction that ships a dead switch.
        assertFalse(Prefs.isProxyEndpoint("203.0.113.7:080"))
    }

    @Test
    fun aLeadingZeroInAnOctetIsRefused() {
        assertFalse(Prefs.isProxyEndpoint("01.2.3.4:1080"))
        assertFalse(Prefs.isProxyEndpoint("203.0.113.007:1080"))
        // A single "0" is not a leading zero, and is a perfectly good octet.
        assertTrue(Prefs.isProxyEndpoint("0.0.0.0:1080"))
    }

    @Test
    fun anOctetOutOfRangeOrMissingIsRefused() {
        assertFalse(Prefs.isProxyEndpoint("203.0.113.256:1080"))
        assertFalse(Prefs.isProxyEndpoint("203.0.113:1080"))
        assertFalse(Prefs.isProxyEndpoint("203.0.113.7.8:1080"))
        assertFalse(Prefs.isProxyEndpoint("203.0.113.:1080"))
    }

    @Test
    fun anUnbracketedIpv6IsRefused() {
        // The kernel wants the brackets — an authority is `[addr]:port` — so
        // `::1:1080` is ambiguous and is not accepted by either side. Refusing it
        // here keeps the two parsers agreeing.
        assertFalse(Prefs.isProxyEndpoint("::1:1080"))
        assertFalse(Prefs.isProxyEndpoint("2001:db8::1:1080"))
    }

    @Test
    fun aBracketedNonAddressIsRefused() {
        assertFalse(Prefs.isProxyEndpoint("[not-an-address]:1080"))
        assertFalse(Prefs.isProxyEndpoint("[]:1080"))
        assertFalse(Prefs.isProxyEndpoint("[::1:1080"))
    }

    // --- the upstream resolver's endpoint gate ---------------------------------

    @Test
    fun everyShippedPresetIsOneTheKernelWillDial() {
        // The presets are code, not storage, so this is the only place that can
        // catch a typo in one of them. A preset the kernel would refuse is a chip
        // that looks like a working resolver and is not — the exact failure the
        // gate exists to prevent, arriving through the back door.
        assertTrue(Prefs.DNS_UPSTREAM_PRESETS.isNotEmpty())
        for (preset in Prefs.DNS_UPSTREAM_PRESETS) {
            assertTrue(
                "preset ${preset.id} (${preset.url} via ${preset.address}) does not parse",
                Prefs.isDohEndpoint(preset.url, preset.address),
            )
        }
    }

    @Test
    fun anAddressUrlIsDialableWithNoBootstrap() {
        // The certificate for `1.1.1.1` carries the address, so nothing has to be
        // resolved to reach it — which is why this is the one preset that needs
        // no bootstrap address.
        assertTrue(Prefs.isDohEndpoint("https://1.1.1.1/dns-query", ""))
        assertTrue(Prefs.isDohEndpoint("https://[2606:4700:4700::1111]/dns-query", ""))
    }

    @Test
    fun aNamedUrlNeedsABootstrapAddress() {
        // The circle this gate has to reproduce: `dns.alidns.com` cannot be
        // resolved *by* the resolver it names, so without an address the kernel
        // drops it and the chip would be a control that cannot take effect.
        assertFalse(Prefs.isDohEndpoint("https://dns.alidns.com/dns-query", ""))
        assertFalse(Prefs.isDohEndpoint("https://dns.alidns.com/dns-query", "not-an-ip"))
        assertTrue(Prefs.isDohEndpoint("https://dns.alidns.com/dns-query", "223.5.5.5"))
        // A bootstrap is an address, not an endpoint: no port is written on it.
        assertTrue(Prefs.isDohEndpoint("https://doh.pub/dns-query", "1.12.12.12"))
    }

    @Test
    fun aMissingPortAndAPathAreBothFine() {
        // Mirrors the Rust: 443 by default, `/dns-query` by default. Both are
        // what a person types when they paste a provider's page.
        assertTrue(Prefs.isDohEndpoint("https://dns.alidns.com", "223.5.5.5"))
        assertTrue(Prefs.isDohEndpoint("https://1.1.1.1", ""))
        assertTrue(Prefs.isDohEndpoint("https://1.1.1.1:8443/dns-query", ""))
    }

    @Test
    fun plaintextIsRefusedRatherThanDowngraded() {
        // A plaintext endpoint is reachable by exactly the interception a
        // resolver exists to avoid, so accepting one would ship a control that
        // cannot do its job.
        assertFalse(Prefs.isDohEndpoint("http://1.1.1.1/dns-query", ""))
        assertFalse(Prefs.isDohEndpoint("1.1.1.1/dns-query", ""))
        assertFalse(Prefs.isDohEndpoint("", ""))
        assertFalse(Prefs.isDohEndpoint("https://", ""))
        assertFalse(Prefs.isDohEndpoint("https:///dns-query", ""))
        assertFalse(Prefs.isDohEndpoint("https://1.1.1.1:notaport/dns-query", ""))
        assertFalse(Prefs.isDohEndpoint("https://1.1.1.1:0/dns-query", ""))
        assertFalse(Prefs.isDohEndpoint("https://1.1.1.1:65536/dns-query", ""))
    }

    @Test
    fun anUnbracketedIpv6AuthorityIsRefused() {
        // The Rust splits the authority at its last `:`, so an unbracketed IPv6
        // address reads its port out of the middle of the address and fails.
        // Refusing it here keeps the two parsers agreeing rather than letting the
        // gate be the looser of the pair.
        assertFalse(Prefs.isDohEndpoint("https://2606:4700:4700::1111/dns-query", ""))
    }

    @Test
    fun anIpLiteralIsJudgedTheWayTheKernelJudgesIt() {
        assertTrue(Prefs.isIpLiteral("223.5.5.5"))
        assertTrue(Prefs.isIpLiteral("::1"))
        assertTrue(Prefs.isIpLiteral("2606:4700:4700::1111"))
        // A bare `a.b.c` is not a literal, and must not be handed to a resolver
        // looking for one.
        assertFalse(Prefs.isIpLiteral("1.2.3"))
        assertFalse(Prefs.isIpLiteral("dns.alidns.com"))
        assertFalse(Prefs.isIpLiteral("01.2.3.4"))
        assertFalse(Prefs.isIpLiteral(""))
    }
}
