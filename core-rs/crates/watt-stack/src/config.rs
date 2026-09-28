//! Engine configuration and runtime counters.

use std::net::IpAddr;
use std::time::Duration;

use crate::DEFAULT_MTU;

/// A static destination rewrite.
///
/// Applied when a flow's destination address matches `match_addr` (and
/// `match_port`, when given). Two situations need this:
///
/// * **Reaching an address that only exists behind the tunnel.** An integration
///   test routes `198.51.100.0/24` into the TUN and rewrites it to a real local
///   server, which is the only way to exercise the full packet path without
///   either a second machine or a routing loop.
/// * **Pinning a CDN address.** When a rule's address is unreachable from a
///   particular network, an operator can point it at a reachable one without
///   editing the upstream rule document.
///
/// Rewrites are matched before rules, so they also act as an explicit override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationOverride {
    /// Destination address to match.
    pub match_addr: IpAddr,
    /// Destination port to match; `None` matches every port.
    pub match_port: Option<u16>,
    /// Address to connect to instead.
    pub target_addr: IpAddr,
    /// Port to connect to instead; `None` keeps the original port.
    pub target_port: Option<u16>,
}

impl DestinationOverride {
    /// Rewrite every port on `match_addr` to `target`.
    pub fn address(match_addr: IpAddr, target_addr: IpAddr) -> Self {
        Self {
            match_addr,
            match_port: None,
            target_addr,
            target_port: None,
        }
    }

    /// Rewrite a single address and port pair.
    pub fn endpoint(
        match_addr: IpAddr,
        match_port: u16,
        target_addr: IpAddr,
        target_port: u16,
    ) -> Self {
        Self {
            match_addr,
            match_port: Some(match_port),
            target_addr,
            target_port: Some(target_port),
        }
    }

    /// Resolve a destination through this override, if it applies.
    pub fn apply(&self, addr: IpAddr, port: u16) -> Option<(IpAddr, u16)> {
        if addr != self.match_addr {
            return None;
        }
        if let Some(expected) = self.match_port {
            if expected != port {
                return None;
            }
        }
        Some((self.target_addr, self.target_port.unwrap_or(port)))
    }
}

/// Tunables for the full-traffic kernel.
#[derive(Debug, Clone)]
pub struct StackConfig {
    /// Interface name requested from the kernel. Empty means "let the kernel choose".
    pub tun_name: String,
    /// Interface MTU.
    pub mtu: usize,
    /// The address assigned to the TUN interface.
    ///
    /// The stack claims this address and accepts packets for every other unicast
    /// destination, which is what makes it a transparent relay rather than a host
    /// with one identity.
    pub address: IpAddr,
    /// Prefix length reported to the kernel for `address`.
    pub prefix_len: u8,

    /// Receive buffer per TCP socket, in bytes.
    pub tcp_rx_buffer: usize,
    /// Send buffer per TCP socket, in bytes.
    pub tcp_tx_buffer: usize,
    /// How many idle listening sockets to keep per destination endpoint.
    ///
    /// smoltcp consumes a listening socket when it accepts a connection, so the
    /// kernel keeps spares ready. Too few and a client opening several parallel
    /// connections to one host sees a stall; too many and memory is wasted on
    /// buffers that are never used.
    ///
    /// This is the *resting* number. See
    /// [`StackConfig::max_listeners_per_endpoint`] for how far a burst may go.
    pub listener_pool: usize,
    /// How many listening sockets one destination endpoint may hold at once.
    ///
    /// This is the burst capacity, and it has to be larger than the pool. A
    /// client that opens N connections to one host at the same moment needs N
    /// listeners, because smoltcp answers a SYN that finds no free listener with
    /// a reset — which the client reports as "connection refused", the exact
    /// symptom of a server that is down. The ceiling exists because every
    /// listener allocates both socket buffers eagerly, so it cannot be unbounded.
    pub max_listeners_per_endpoint: usize,
    /// How many listening sockets the kernel may hold across every endpoint.
    ///
    /// The backstop for a flood that spreads over many destinations, where a
    /// per-endpoint ceiling never triggers.
    pub max_listeners: usize,
    /// Hard cap on simultaneously open TCP flows.
    pub max_tcp_flows: usize,
    /// Hard cap on simultaneously open UDP flows.
    pub max_udp_flows: usize,
    /// Idle timeout for an established TCP flow.
    pub tcp_idle_timeout: Duration,
    /// Idle timeout for a UDP flow.
    pub udp_idle_timeout: Duration,
    /// How long to wait for an upstream TCP connection to complete.
    pub connect_timeout: Duration,
    /// How long a single candidate may hold a race slot before it is rotated out.
    ///
    /// With parallel racing this no longer decides how fast the *client* sees a
    /// failure over — that is now the window width. It is the metronome of the
    /// race window: a candidate that is still dialling after this long loses its
    /// slot to a fresh candidate, **but only while there is somewhere else to
    /// go**. The moment a candidate becomes the flow's only hope, the full
    /// `connect_timeout` applies instead, so a slow but honest server still gets
    /// its whole budget.
    ///
    /// Measured on the tunnel path: `github.com` arrives with eight candidates,
    /// the head is sometimes dead, and the old serial failure path paid ten
    /// seconds to learn that. A `curl` with a twelve second budget then had
    /// nothing left for the candidate that would have worked, so a domain with
    /// eight addresses failed for the same reason a domain with one would. Three
    /// seconds is enough for a live server on a mobile link (the working connects
    /// measure ~500 ms) and short enough that a rotated-out candidate does not
    /// stall the window.
    pub first_connect_timeout: Duration,
    /// Upper bound on bytes buffered per direction per flow.
    pub flow_buffer_limit: usize,
    /// How many of a rule's addresses a single flow will try.
    ///
    /// The rule set gives some domains dozens of addresses (`github.com` has
    /// thirty-nine) and the relay tries them one at a time, so an unbounded list
    /// turns "this domain is unreachable" into "this domain costs three hundred
    /// seconds to fail". Bounded, the same failure costs a few seconds.
    pub max_candidates: usize,
    /// Milliseconds to wait between starting two upstream connects of one flow.
    ///
    /// Purely a courtesy to the network: fifty simultaneous SYNs from one device
    /// to one CDN edge is what a flood looks like, and some middleboxes respond
    /// by dropping all of them. Zero disables the pause.
    ///
    /// Under racing this is **degraded** to "the gap between the first batch's
    /// candidates": the same field still spaces the SYNs of one race, but the
    /// window is what bounds how many go out at once. It is folded into
    /// [`StackConfig::race_launch_interval`] by taking the larger of the two, so
    /// an operator who set a long stagger keeps it.
    pub connect_stagger: Duration,
    /// How many candidates a flow dials in parallel (the happy-eyeballs window).
    ///
    /// One at a time is the old serial behaviour and is kept as a one-key
    /// rollback: a value of `1` reproduces it exactly. The default of `3` covers
    /// the measured `github.com` shape — eight candidates, the first two dead,
    /// the third live — so the first live address is found in the time of one
    /// stagger instead of two connect timeouts. Clamped to `1..=4` where it is
    /// consumed: past four the extra SYNs and descriptors buy nothing on a
    /// mobile link.
    pub race_width: usize,
    /// Milliseconds between launching two candidates of the same race.
    ///
    /// The window fills one candidate per tick and no faster than this, so a
    /// burst of flows does not turn into a burst of SYNs. Zero lets the first
    /// batch go out together, which is not recommended.
    ///
    /// The effective spacing is `max(this, connect_stagger)`. The default is set
    /// equal to `connect_stagger`'s default on purpose: with the two aligned,
    /// `max` is a no-op until someone deliberately raises one of them, and the
    /// value this field reports is the value the race actually uses. They were
    /// previously 150 ms and 250 ms, so the field said 150 ms while the window
    /// ran at 250 ms — a number that was wrong on its face.
    pub race_launch_interval: Duration,
    /// Upper bound on concurrent in-flight upstream dials across every flow.
    ///
    /// Racing raises a single flow's in-flight descriptors from one to the
    /// window width, so a SYN storm could otherwise open thousands of sockets at
    /// once and exhaust `RLIMIT_NOFILE`. This caps the handshake window globally;
    /// a race that would exceed it simply waits for a slot.
    pub max_dialing: usize,
    /// Longest the engine blocks waiting for a descriptor to become ready.
    ///
    /// This doubles as the timer granularity: smoltcp retransmits and idle flow
    /// reaping only happen when the loop wakes, so a large value makes a stalled
    /// connection feel sluggish while a small one burns power. Twenty
    /// milliseconds is below the threshold where a user notices a delay and well
    /// above the cost of a `poll` that finds nothing.
    pub poll_timeout: Duration,
    /// How often expired observations and address history are swept.
    pub prune_interval: Duration,

    /// Answer DNS queries for rule owned domains locally instead of forwarding.
    pub answer_dns_from_rules: bool,
    /// TTL used for locally answered records.
    pub dns_answer_ttl: Duration,
    /// Record address to domain mappings from forwarded answers.
    pub observe_dns: bool,
    /// Whether to check a rule address's certificate before dialling it.
    ///
    /// On by default, because without it the choice of address is an RTT race
    /// and whether a domain works is luck. See `Prefs.certificateCheck` on the
    /// shell side for the measurements.
    pub certificate_check: bool,
    /// Upper bound on addresses tracked in the observation cache.
    pub max_observed_addresses: usize,

    /// How long a rule address is deprioritized after a failed connect.
    ///
    /// Owned by the rule selector, not the data path: a failed address is not
    /// written off, only pushed to the back of the candidate list for this long,
    /// so a domain whose fastest address was briefly down still recovers. It is
    /// forwarded to `watt_rules::IpSelectorConfig::failure_cooldown` when the
    /// engine is built — the selector is created with the router and the router
    /// with the engine, so this is the one point at which it can be handed over.
    pub failure_cooldown: Duration,

    /// Resolve the rules' dial names before the tunnel starts serving.
    ///
    /// A rule may name a host (`edge.example.net`) rather than list addresses,
    /// and the kernel has to resolve those names itself: substituting the
    /// client's own resolution would discard the rule's whole point. Resolving is
    /// blocking I/O, which is why it happens once, before the engine is handed to
    /// the run loop, rather than inside a step.
    pub dial_names: bool,

    /// Static destination rewrites, matched before rules.
    pub overrides: Vec<DestinationOverride>,
    /// Addresses that must never be relayed, to stop the kernel feeding itself.
    pub excluded_destinations: Vec<IpAddr>,
}

impl Default for StackConfig {
    fn default() -> Self {
        Self {
            tun_name: String::new(),
            mtu: DEFAULT_MTU,
            // 198.18.0.0/15 is reserved for benchmarking by RFC 2544 and is not
            // routable, which makes it a safe private identity for the tunnel.
            address: "198.18.0.1".parse().expect("valid literal"),
            prefix_len: 15,
            tcp_rx_buffer: 16 * 1024,
            tcp_tx_buffer: 16 * 1024,
            listener_pool: 4,
            // Sixty four listeners is two megabytes of buffers at the default
            // socket sizes, and comfortably above what a browser or a torrent
            // client opens to a single host.
            max_listeners_per_endpoint: 64,
            max_listeners: 256,
            max_tcp_flows: 2048,
            max_udp_flows: 1024,
            tcp_idle_timeout: Duration::from_secs(300),
            udp_idle_timeout: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(20),
            first_connect_timeout: Duration::from_secs(3),
            flow_buffer_limit: 512 * 1024,
            // Twelve candidates is a balance: high enough that a CDN entry with a
            // handful of dead addresses still finds a live one, low enough that a
            // wholly unreachable domain fails in about two connect timeouts rather
            // than in minutes. The certificate check normally shortens the list
            // further, so this only binds when there is nothing to go on.
            max_candidates: 12,
            connect_stagger: Duration::from_millis(250),
            // Three covers the measured `github.com` shape (two dead candidates
            // ahead of a live one) without paying for more SYNs than that needs.
            race_width: 3,
            // Equal to `connect_stagger`'s default so the field and the spacing
            // the window actually uses agree; see the field's own docs.
            race_launch_interval: Duration::from_millis(250),
            // A backstop for the handshake window, not for established flows:
            // 2048 flows × a window of 3 would otherwise approach 6000 sockets.
            max_dialing: 256,
            poll_timeout: Duration::from_millis(20),
            prune_interval: Duration::from_secs(30),
            answer_dns_from_rules: true,
            dns_answer_ttl: Duration::from_secs(60),
            observe_dns: true,
            certificate_check: true,
            max_observed_addresses: 8192,
            failure_cooldown: Duration::from_secs(60),
            dial_names: true,
            overrides: Vec::new(),
            excluded_destinations: Vec::new(),
        }
    }
}

impl StackConfig {
    /// Set the TUN identity.
    pub fn with_address(mut self, address: IpAddr, prefix_len: u8) -> Self {
        self.address = address;
        self.prefix_len = prefix_len;
        self
    }

    /// Add a static destination rewrite.
    pub fn with_override(mut self, entry: DestinationOverride) -> Self {
        self.overrides.push(entry);
        self
    }

    /// Resolve a destination through the override table.
    pub fn resolve_override(&self, addr: IpAddr, port: u16) -> Option<(IpAddr, u16)> {
        self.overrides.iter().find_map(|entry| entry.apply(addr, port))
    }

    /// True when `addr` must not be relayed.
    ///
    /// Without this the kernel can connect to an address that its own route table
    /// points back at the tunnel, producing a loop that eats a file descriptor and
    /// a thread per packet.
    pub fn is_excluded(&self, addr: IpAddr) -> bool {
        addr.is_loopback()
            || addr.is_unspecified()
            || addr.is_multicast()
            || self.excluded_destinations.contains(&addr)
    }
}

/// Counters describing what the kernel has done.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stats {
    pub packets_in: u64,
    pub packets_out: u64,
    pub packets_unparsable: u64,
    pub packets_ignored: u64,

    pub tcp_flows_opened: u64,
    pub tcp_flows_closed: u64,
    pub tcp_connect_failures: u64,
    pub tcp_resets_sent: u64,
    pub tcp_flows_rejected: u64,
    /// Initial SYNs for which a *new* listening socket was refused because a
    /// ceiling was reached.
    ///
    /// Not proof that a client was refused: a socket already in the pool may
    /// serve the SYN. It is a signal that demand has reached the ceiling, and the
    /// authoritative check on whether a client was refused is the client.
    pub tcp_listener_ceiling_hits: u64,

    pub udp_flows_opened: u64,
    pub udp_flows_closed: u64,
    /// Datagrams carried by a flow that already existed.
    ///
    /// A client that reuses a source port lands on the flow the previous socket
    /// left behind, which is what a NAT is supposed to do. Without this counter
    /// the datagram is invisible: it opened nothing, closed nothing and dropped
    /// nothing, so `opened` alone under-reports the traffic that was carried.
    pub udp_datagrams_reused: u64,
    /// Datagrams dropped because no flow could be created for them.
    ///
    /// Every one of these is a client that asked for something and got nothing
    /// back, so a non-zero value on a quiet network is a signal worth chasing.
    pub udp_flows_rejected: u64,
    /// Flows forgotten to make room for a new one when the table was full.
    ///
    /// The table's pressure valve. Evicting is what a NAT does when it runs out
    /// of room: forgetting a finished conversation costs nothing, whereas
    /// refusing a new one is indistinguishable from the destination being down.
    pub udp_flows_evicted: u64,
    /// Datagrams that reached the client path but did not fit the tunnel MTU and
    /// were not a DNS message that could be truncated.
    pub udp_datagrams_dropped: u64,

    pub dns_queries: u64,
    /// Queries answered from the rule set, with at least one address delivered.
    ///
    /// A query the answer could not be built for at all is not counted here: the
    /// client asked and got nothing, which is what `udp_datagrams_dropped` is
    /// for.
    pub dns_answered_locally: u64,
    /// Answers that carried fewer addresses than the rule listed.
    ///
    /// A rule with hundreds of addresses cannot put them all in one datagram, so
    /// the answer is trimmed to the ones that fit. Non-zero is not a fault, but
    /// it does mean the client is seeing a subset, ranked by the selector.
    pub dns_answers_trimmed: u64,
    pub dns_forwarded: u64,
    pub dns_unparsable: u64,
    pub dns_addresses_observed: u64,

    /// Payload bytes carried from a client to an upstream socket.
    ///
    /// A DNS query the kernel answers from the rule set never leaves the device,
    /// so it is counted by [`Stats::dns_answered_locally`] instead of here. The
    /// two counters therefore describe relayed traffic only.
    pub bytes_client_to_upstream: u64,
    /// Payload bytes carried from an upstream socket back to a client.
    pub bytes_upstream_to_client: u64,

    pub flows_matched_rules: u64,
    pub flows_direct: u64,
}

impl Stats {
    /// Total flows currently open.
    pub fn open_flows(&self) -> u64 {
        self.tcp_flows_opened
            .saturating_add(self.udp_flows_opened)
            .saturating_sub(self.tcp_flows_closed)
            .saturating_sub(self.udp_flows_closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn overrides_match_by_address_and_optional_port() {
        let entry = DestinationOverride::endpoint(ip(198, 51, 100, 10), 8080, ip(10, 99, 99, 1), 80);
        assert_eq!(entry.apply(ip(198, 51, 100, 10), 8080), Some((ip(10, 99, 99, 1), 80)));
        assert_eq!(entry.apply(ip(198, 51, 100, 10), 8081), None);
        assert_eq!(entry.apply(ip(198, 51, 100, 11), 8080), None);
    }

    #[test]
    fn address_only_override_keeps_the_port() {
        let entry = DestinationOverride::address(ip(198, 51, 100, 10), ip(10, 0, 0, 5));
        assert_eq!(entry.apply(ip(198, 51, 100, 10), 443), Some((ip(10, 0, 0, 5), 443)));
    }

    #[test]
    fn config_resolves_overrides_in_order() {
        let config = StackConfig::default()
            .with_override(DestinationOverride::endpoint(
                ip(198, 51, 100, 10),
                80,
                ip(10, 0, 0, 1),
                80,
            ))
            .with_override(DestinationOverride::address(
                ip(198, 51, 100, 10),
                ip(10, 0, 0, 2),
            ));

        // The first matching entry wins, so the port specific rule applies to 80.
        assert_eq!(config.resolve_override(ip(198, 51, 100, 10), 80), Some((ip(10, 0, 0, 1), 80)));
        assert_eq!(config.resolve_override(ip(198, 51, 100, 10), 443), Some((ip(10, 0, 0, 2), 443)));
        assert_eq!(config.resolve_override(ip(198, 51, 100, 11), 80), None);
    }

    #[test]
    fn excluded_destinations_cover_loopback_and_multicast() {
        let config = StackConfig::default();
        assert!(config.is_excluded(ip(127, 0, 0, 1)));
        assert!(config.is_excluded(ip(0, 0, 0, 0)));
        assert!(config.is_excluded(ip(224, 0, 0, 1)));
        assert!(!config.is_excluded(ip(198, 51, 100, 1)));

        let mut config = StackConfig::default();
        config.excluded_destinations.push(ip(198, 51, 100, 1));
        assert!(config.is_excluded(ip(198, 51, 100, 1)));
    }

    #[test]
    fn stats_track_open_flows() {
        let stats = Stats {
            tcp_flows_opened: 10,
            tcp_flows_closed: 3,
            udp_flows_opened: 5,
            udp_flows_closed: 1,
            ..Stats::default()
        };
        assert_eq!(stats.open_flows(), 11);
    }

    #[test]
    fn race_defaults_are_the_ones_the_design_settled_on() {
        // The window width is the knob the whole change hangs on; pinning it here
        // means a later refactor cannot quietly turn racing back off.
        let config = StackConfig::default();
        assert_eq!(config.race_width, 3);
        // Equal to `connect_stagger`'s default on purpose: the effective spacing
        // is `max(race_launch_interval, connect_stagger)`, so if the two disagree
        // the field reports a number the race does not use.
        assert_eq!(config.race_launch_interval, Duration::from_millis(250));
        assert_eq!(config.race_launch_interval, config.connect_stagger);
        assert_eq!(config.max_dialing, 256);
        // The two knobs the shell exposes through `apply_settings`. Pinned so a
        // refactor cannot silently change what "leave it alone" means.
        assert_eq!(config.failure_cooldown, Duration::from_secs(60));
        assert!(config.dial_names, "dial names are resolved by default");
    }
}
