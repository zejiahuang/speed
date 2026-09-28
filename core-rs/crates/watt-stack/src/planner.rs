//! Turns "the client is connecting to X" into "connect to Y instead".
//!
//! The data plane only knows addresses; the rule set is keyed by domain. This
//! module is the join between them, and it is where the product's routing policy
//! is actually expressed:
//!
//! 1. **Static rewrites** win outright. They are explicit operator intent.
//! 2. **Rule addresses.** The destination address is looked up in the DNS
//!    observation cache to recover the domain, the domain is matched against the
//!    rule set, and the rule's ranked candidate list becomes the target list.
//! 3. **Rule ownership by address.** DNS was never observed, but the rule set
//!    claims the address the client dialled. The rule's other addresses become
//!    fallbacks, with the dialled address kept first.
//! 4. **Direct.** No rule owns the destination, so the kernel connects exactly
//!    where the client asked. Relaying rather than short-circuiting is still
//!    necessary: the packet is already inside the tunnel, so "direct" means
//!    "connect to the original destination from a socket", not "do nothing".
//!
//! The client's destination port is always preserved. A rule's `port` field
//! describes the service, not a redirect, so honouring it would break a client
//! that deliberately used a different port.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use watt_rules::{Family, Outcome, Plan, Router, Strategy};

use crate::config::StackConfig;
use crate::flow::ObservationCache;

/// What the kernel decided to do with a flow.
#[derive(Debug, Clone)]
pub struct Decision {
    /// Where to actually connect.
    pub target: SocketAddr,
    /// Where the client asked to connect.
    pub requested: SocketAddr,
    /// The routing plan behind the decision.
    pub plan: Plan,
    /// Remaining rule candidates, in preference order, excluding `target`.
    ///
    /// A failed connect advances to the next candidate, which is what makes a
    /// hundred-address CDN entry useful rather than a liability.
    pub alternatives: Vec<IpAddr>,
    /// True when a static rewrite produced the target.
    pub overridden: bool,
}

impl Decision {
    /// True when the decision came from the rule set or a rewrite.
    pub fn is_steered(&self) -> bool {
        self.overridden || self.plan.strategy.is_matched()
    }

    /// Human readable reason, for logs.
    pub fn reason(&self) -> &'static str {
        if self.overridden {
            "override"
        } else {
            self.plan.strategy.as_str()
        }
    }
}

/// Combines the rule set, the observation cache and the override table.
#[derive(Debug)]
pub struct Planner {
    router: Router,
    observations: ObservationCache,
    overrides: Vec<crate::config::DestinationOverride>,
    excluded: Vec<IpAddr>,
    /// Address assigned to the tunnel, which must never be a target.
    tunnel_address: IpAddr,
}

impl Planner {
    /// Build a planner from a router and the engine configuration.
    pub fn new(router: Router, config: &StackConfig) -> Self {
        Self {
            router,
            observations: ObservationCache::new(
                config.max_observed_addresses,
                4,
            ),
            overrides: config.overrides.clone(),
            excluded: config.excluded_destinations.clone(),
            tunnel_address: config.address,
        }
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    pub fn router_mut(&mut self) -> &mut Router {
        &mut self.router
    }

    pub fn observations(&self) -> &ObservationCache {
        &self.observations
    }

    /// Replace the rule set, keeping the observation cache and address history.
    pub fn replace_rules(&mut self, rules: watt_rules::RuleSet) {
        self.router.replace_rules(rules);
    }

    /// Re-resolve the rules' dial names, caching the addresses in the router.
    ///
    /// Driven from a refresh tick, not from the data path: resolving blocks, and
    /// the engine's step must not. Returns how many entries gained an address.
    pub fn resolve_dial_names<F>(&mut self, resolve: F) -> usize
    where
        F: FnMut(&str) -> Vec<IpAddr>,
    {
        self.router.resolve_dial_names(resolve)
    }

    /// Record that `domain` resolved to `address` for `ttl`.
    pub fn observe(&mut self, address: IpAddr, domain: &str, ttl: Duration, now: Instant) {
        if address.is_unspecified() || address == self.tunnel_address {
            return;
        }
        self.observations.record(address, domain, ttl, now);
    }

    /// Domains still associated with `address`.
    pub fn domains_for(&self, address: IpAddr, now: Instant) -> Vec<&str> {
        self.observations.domains(address, now)
    }

    /// Decide where to connect for a flow whose destination is `requested`.
    pub fn decide(&self, now: Instant, requested: SocketAddr) -> Decision {
        let requested_ip = requested.ip();
        let family = Family::of(requested_ip);

        // 1. Static rewrites.
        if let Some((addr, port)) = self
            .overrides
            .iter()
            .find_map(|entry| entry.apply(requested_ip, requested.port()))
        {
            return Decision {
                target: SocketAddr::new(addr, port),
                requested,
                plan: Plan::direct(),
                alternatives: Vec::new(),
                overridden: true,
            };
        }

        // 2. Rule addresses, via the domain recovered from DNS observation.
        //
        // A rule can match and still produce no usable address — a placeholder
        // entry, or one whose addresses are all in the other family. The plan is
        // kept so the decision can still be explained in a log, but the loop keeps
        // looking, because another observed domain for the same address may have
        // real addresses to offer.
        let mut matched_plan: Option<Plan> = None;
        for domain in self.observations.domains(requested_ip, now) {
            let plan = self.router.plan(now, domain, family);
            if plan.strategy == Strategy::RuleAddresses && !plan.addresses.is_empty() {
                let mut addresses = plan.addresses.clone();
                let first = addresses.remove(0);
                return Decision {
                    target: SocketAddr::new(first, requested.port()),
                    requested,
                    plan,
                    alternatives: addresses,
                    overridden: false,
                };
            }
            if plan.strategy.is_matched() && matched_plan.is_none() {
                matched_plan = Some(plan);
            }
        }

        // 3. The client dialled an address the rule set claims, but no DNS was
        // observed for it. Clients using DNS-over-HTTPS or a private resolver
        // never expose their lookups, and without this step every one of their
        // flows would be relayed blindly.
        let plan = self.router.plan_for_ip(now, requested_ip);
        if plan.strategy == Strategy::RuleAddresses && !plan.addresses.is_empty() {
            let mut addresses = plan.addresses.clone();
            let first = addresses.remove(0);
            return Decision {
                target: SocketAddr::new(first, requested.port()),
                requested,
                plan,
                alternatives: addresses,
                overridden: false,
            };
        }

        // 4. Direct: the client's own destination, from a real socket.
        Decision {
            target: requested,
            requested,
            plan: matched_plan.unwrap_or_else(Plan::direct),
            alternatives: Vec::new(),
            overridden: false,
        }
    }

    /// True when `addr` must not be used as a rule candidate.
    ///
    /// Used for the fallback addresses a rule supplies. A rule listing a loopback
    /// or multicast address is a rule authoring mistake, and retrying against it
    /// would only waste time.
    ///
    /// Private and link-local ranges are refused for a stronger reason than
    /// tidiness: the rule set is fetched over the network and is not signed, so a
    /// LAN address in it would aim this device at its own network — every other
    /// host on the Wi-Fi becomes something a remote document can point a domain
    /// at. `is_blocked_target` is the boundary that keeps a public rule set from
    /// describing private destinations.
    pub fn is_blocked_target(&self, addr: IpAddr) -> bool {
        addr == self.tunnel_address
            || addr.is_loopback()
            || addr.is_unspecified()
            || addr.is_multicast()
            || is_lan(addr)
            || self.excluded.contains(&addr)
    }

    /// True when the decision's target can actually be connected to.
    ///
    /// This is deliberately weaker than [`Planner::is_blocked_target`] for one
    /// case: a static rewrite. An override is explicit operator intent, and the
    /// most common reason to write one is to point a flow at a service that only
    /// listens on loopback — a local stub, or a test server standing in for a
    /// remote one. Refusing it would make the override table useless for the job
    /// it exists to do.
    ///
    /// The tunnel's own address stays off limits either way: connecting to it
    /// would hand the flow straight back to the kernel and spin forever.
    ///
    /// An override may reach loopback or a private address — the operator wrote it
    /// down, so a local stub or a service on the same LAN is a legitimate target.
    /// A rule may not, which is the difference between `is_blocked_target` and
    /// this function.
    pub fn can_relay(&self, decision: &Decision) -> bool {
        let addr = decision.target.ip();
        if addr == self.tunnel_address {
            return false;
        }
        if decision.overridden {
            return true;
        }
        !(addr.is_loopback()
            || addr.is_unspecified()
            || addr.is_multicast()
            || is_lan(addr)
            || self.excluded.contains(&addr))
    }

    /// Record a successful connect against `addr`.
    pub fn report_success(&mut self, addr: IpAddr, rtt: Duration, now: Instant) {
        self.router.report_success(addr, rtt, now);
    }

    /// Record a failed connect against `addr`.
    pub fn report_failure(&mut self, addr: IpAddr, now: Instant) {
        self.router.report_failure(addr, now);
    }

    /// Record how a session ended, once its bytes have stopped flowing.
    ///
    /// [`Self::report_success`] fires when the connection opens; this fires when
    /// the flow is reaped. The gap between them is where "connected but nothing
    /// came back" lives, and that is the case a connect-only signal cannot see.
    pub fn report_outcome(&mut self, addr: IpAddr, outcome: Outcome, now: Instant) {
        self.router.report_outcome(addr, outcome, now);
    }

    /// Drop stale observation entries and address history.
    pub fn prune(&mut self, now: Instant) {
        self.observations.prune(now);
        self.router.prune(now);
    }
}

/// True for addresses that belong to the local network rather than the internet.
///
/// Spelled out rather than using the standard library's predicates because the
/// IPv6 ones (`is_unique_local`, `is_unicast_link_local`) are not stable, and a
/// rule set that reaches this check deserves an answer that does not change with
/// the toolchain.
fn is_lan(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let octets = v6.octets();
            // fc00::/7 unique local — the top seven bits are 1111110.
            (octets[0] & 0xfe) == 0xfc
                // fe80::/10 link local.
                || (octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use watt_rules::{RuleSet, RuleSource};

    const DOC: &str = r#"{
      "meta": { "version": "t", "update_time": "t" },
      "groups": [
        { "group": "g", "entries": [
          { "id": "1", "name": "CDN", "domains": ["cdn.example"], "ips": ["203.0.113.10", "203.0.113.20"], "port": "443", "isPlaceholder": false },
          { "id": "2", "name": "Placeholder", "domains": ["ph.example"], "ips": ["{Cloudflare}"], "port": "443", "isPlaceholder": true }
        ] }
      ]
    }"#;

    fn planner() -> Planner {
        let rules = RuleSet::from_str(DOC, RuleSource::Provided).unwrap();
        let config = StackConfig::default();
        Planner::new(Router::new(rules), &config)
    }

    fn v4(last: u8, port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, last)), port)
    }

    #[test]
    fn an_unknown_address_is_relayed_directly() {
        let planner = planner();
        let decision = planner.decide(Instant::now(), v4(99, 443));
        assert!(!decision.is_steered());
        assert_eq!(decision.target, v4(99, 443));
        assert_eq!(decision.reason(), "direct");
    }

    #[test]
    fn an_observed_domain_pulls_in_the_rule_addresses() {
        let mut planner = planner();
        let now = Instant::now();
        // The client resolved cdn.example and got .10, but the selector prefers
        // the first ranked candidate.
        planner.observe(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)), "cdn.example", Duration::from_secs(60), now);

        let decision = planner.decide(now, v4(10, 443));
        assert!(decision.is_steered());
        assert_eq!(decision.target, v4(10, 443));
        assert_eq!(decision.alternatives, vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 20))]);
        assert_eq!(decision.plan.entry_name.as_deref(), Some("CDN"));
    }

    #[test]
    fn the_client_port_is_preserved_even_when_the_rule_names_another() {
        let mut planner = planner();
        let now = Instant::now();
        planner.observe(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)), "cdn.example", Duration::from_secs(60), now);

        // The rule says 443; the client used 8443 and must get 8443.
        let decision = planner.decide(now, v4(10, 8443));
        assert_eq!(decision.target.port(), 8443);
        assert_eq!(decision.plan.port, Some(443));
    }

    #[test]
    fn a_placeholder_rule_does_not_redirect() {
        let mut planner = planner();
        let now = Instant::now();
        planner.observe(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 77)), "ph.example", Duration::from_secs(60), now);

        let decision = planner.decide(now, v4(77, 443));
        assert_eq!(decision.target, v4(77, 443));
        assert_eq!(decision.reason(), "placeholder-fallback");
    }

    #[test]
    fn expired_observations_stop_steering() {
        let mut planner = planner();
        let now = Instant::now();
        // 203.0.113.77 belongs to no rule, so once the observation expires nothing
        // is left to steer the flow with.
        planner.observe(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 77)), "cdn.example", Duration::from_secs(10), now);

        let later = now + Duration::from_secs(120);
        let decision = planner.decide(later, v4(77, 443));
        assert!(!decision.is_steered());
        assert_eq!(decision.target, v4(77, 443));
    }

    #[test]
    fn a_rule_owned_address_is_steered_even_without_a_dns_observation() {
        let planner = planner();
        let now = Instant::now();
        // Nothing was observed: this is the DNS-over-HTTPS case, where the client
        // resolves on its own and the kernel only ever sees addresses.
        let decision = planner.decide(now, v4(10, 443));
        assert!(decision.is_steered());
        assert_eq!(decision.target, v4(10, 443), "the dialled address must stay first");
        assert_eq!(decision.reason(), "rule-addresses");
        assert_eq!(
            decision.alternatives,
            vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 20))],
            "the rule's other addresses become fallbacks"
        );
    }


    #[test]
    fn overrides_take_precedence_over_rules() {
        let rules = RuleSet::from_str(DOC, RuleSource::Provided).unwrap();
        let config = StackConfig::default().with_override(
            crate::config::DestinationOverride::endpoint(
                IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)),
                443,
                IpAddr::V4(Ipv4Addr::new(10, 99, 99, 1)),
                8443,
            ),
        );
        let mut planner = Planner::new(Router::new(rules), &config);
        let now = Instant::now();
        planner.observe(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)), "cdn.example", Duration::from_secs(60), now);

        let decision = planner.decide(now, v4(10, 443));
        assert!(decision.overridden);
        assert_eq!(decision.reason(), "override");
        assert_eq!(
            decision.target,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 99, 99, 1)), 8443)
        );
    }

    #[test]
    fn failures_reorder_the_candidates_a_later_flow_receives() {
        let mut planner = planner();
        let now = Instant::now();
        planner.observe(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)), "cdn.example", Duration::from_secs(60), now);

        planner.report_success(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 20)), Duration::from_millis(5), now);
        planner.report_failure(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)), now);

        let decision = planner.decide(now, v4(10, 443));
        assert_eq!(
            decision.target,
            v4(20, 443),
            "the healthy candidate must be preferred"
        );
        assert_eq!(decision.alternatives, vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10))]);
    }

    #[test]
    fn the_tunnel_address_is_never_a_target() {
        let planner = planner();
        let config = StackConfig::default();
        assert!(planner.is_blocked_target(config.address));
        assert!(planner.is_blocked_target(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!planner.is_blocked_target(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10))));
    }

    #[test]
    fn a_rule_may_not_name_a_private_address() {
        // The rule set is fetched over the network and is not signed. A LAN
        // address in it would aim this device at its own network, so every host
        // on the Wi-Fi would become something a remote document can point a
        // domain at. This is the boundary that refuses it.
        let planner = planner();
        for addr in [
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.1",
            "169.254.1.1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
        ] {
            let addr: IpAddr = addr.parse().expect("test address parses");
            assert!(planner.is_blocked_target(addr), "{addr} must be refused");
        }
    }

    #[test]
    fn a_public_address_is_still_allowed() {
        // The guard must not become a filter that breaks ordinary rules.
        let planner = planner();
        for addr in ["203.0.113.10", "8.8.8.8", "2001:4860:4860::8888", "2606:4700::1111"] {
            let addr: IpAddr = addr.parse().expect("test address parses");
            assert!(!planner.is_blocked_target(addr), "{addr} must be allowed");
        }
    }

    #[test]
    fn a_rewrite_to_loopback_is_relayable_but_a_plain_loopback_target_is_not() {
        let config = StackConfig::default().with_override(
            crate::config::DestinationOverride::address(
                IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
            ),
        );
        let rules = RuleSet::from_str(DOC, RuleSource::Provided).unwrap();
        let planner = Planner::new(Router::new(rules), &config);
        let now = Instant::now();

        // Rewritten: allowed, because that is what the override is for.
        let rewritten = planner.decide(now, v4(10, 443));
        assert!(rewritten.overridden);
        assert!(planner.can_relay(&rewritten));

        // Not rewritten: a loopback destination is still refused.
        let plain = planner.decide(
            now,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443),
        );
        assert!(!plain.overridden);
        assert!(!planner.can_relay(&plain));
    }

    #[test]
    fn even_a_rewrite_may_not_point_at_the_tunnel() {
        let tunnel = StackConfig::default().address;
        let config = StackConfig::default()
            .with_override(crate::config::DestinationOverride::address(v4(10, 443).ip(), tunnel));
        let rules = RuleSet::from_str(DOC, RuleSource::Provided).unwrap();
        let planner = Planner::new(Router::new(rules), &config);

        let decision = planner.decide(Instant::now(), v4(10, 443));
        assert!(decision.overridden);
        assert!(!planner.can_relay(&decision), "the tunnel would loop back on itself");
    }

    #[test]
    fn observing_the_tunnel_address_is_ignored() {
        let mut planner = planner();
        let now = Instant::now();
        let tunnel = StackConfig::default().address;
        planner.observe(tunnel, "self.example", Duration::from_secs(60), now);
        assert!(planner.domains_for(tunnel, now).is_empty());
    }
}
