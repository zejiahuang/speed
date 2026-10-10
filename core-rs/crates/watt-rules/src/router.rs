//! Decision layer: turn a domain plus the rule set into a concrete connect plan.
//!
//! This is where the product's routing policy lives, kept separate from both the
//! rule indexes and the data plane so that it can be unit tested without any
//! sockets or TUN devices.
//!
//! Policy, in order of preference:
//!
//! 1. **Rule addresses.** The rule carries concrete addresses and at least one of
//!    them matches the family the client asked for. Candidates are ranked by the
//!    selector and tried in order, falling through to the next on failure.
//! 2. **Placeholder fallback.** The rule matched but only via `{Cloudflare}` /
//!    `{Cloudfront}`. The address is not ours to invent, so the caller resolves
//!    upstream normally while still knowing the domain is rule owned.
//! 3. **Family fallback.** The rule has addresses, but all of them are in the
//!    other address family than the client used. Inventing an IPv6 answer for an
//!    IPv4 client would simply fail, so we resolve upstream instead.
//! 4. **Direct.** No rule matched. The connection is left exactly as the client
//!    asked for it.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::Instant;

use crate::builtin::BUILTIN_RULES_JSON;
use crate::error::Result;
use crate::ruleset::{CompiledEntry, Placeholder, RuleSet, RuleSource};
use crate::selector::{IpSelector, IpSelectorConfig, Outcome};

/// Address family of the flow being planned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    V4,
    V6,
}

impl Family {
    /// Family of a concrete address.
    pub fn of(addr: IpAddr) -> Self {
        match addr {
            IpAddr::V4(_) => Family::V4,
            IpAddr::V6(_) => Family::V6,
        }
    }

    /// True when `addr` belongs to this family.
    pub fn admits(self, addr: IpAddr) -> bool {
        matches!(
            (self, addr),
            (Family::V4, IpAddr::V4(_)) | (Family::V6, IpAddr::V6(_))
        )
    }
}

/// How a plan was produced. Surfaced in logs so behaviour is explainable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Rule supplied concrete addresses usable by this client.
    RuleAddresses,
    /// Rule matched, but only a CDN placeholder is available.
    PlaceholderFallback,
    /// Rule matched with addresses, but none in the client's address family.
    FamilyFallback,
    /// No rule matched; leave the connection untouched.
    Direct,
}

impl Strategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Strategy::RuleAddresses => "rule-addresses",
            Strategy::PlaceholderFallback => "placeholder-fallback",
            Strategy::FamilyFallback => "family-fallback",
            Strategy::Direct => "direct",
        }
    }

    /// True when the rule set claimed the domain, regardless of the outcome.
    pub fn is_matched(&self) -> bool {
        !matches!(self, Strategy::Direct)
    }
}

/// The outcome of planning one connection.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Normalized domain the plan is for, when known.
    pub query: Option<String>,
    pub strategy: Strategy,
    /// Rule entry key, when a rule matched.
    pub entry_key: Option<u32>,
    pub entry_name: Option<String>,
    /// The rule domain that matched, which may be a parent of `query`.
    pub rule_domain: Option<String>,
    /// Ordered candidates. Empty means "resolve upstream as usual".
    pub addresses: Vec<IpAddr>,
    /// Names the rule named instead of an address, in rule order.
    ///
    /// These are *not* resolved here. Resolving is I/O, and this layer is pure so
    /// that policy can be tested without a network. The caller resolves them,
    /// because only the caller knows what a blocking lookup costs in its context:
    /// the local proxy already spends a thread per connection and can afford to
    /// resolve per request, whereas the TUN data path must not block and will
    /// want to resolve on a refresh tick instead.
    ///
    /// They are tried after `addresses`: a literal address is data the rule
    /// author already verified, while a name is a lookup that may fail.
    pub dial_names: Vec<String>,
    /// What each dial name resolved to on the last refresh tick, paired with the
    /// name it came from.
    ///
    /// A rule that names a host instead of an address is saying *which* host to
    /// talk to, and for a caller that re-originates TLS that is not only where to
    /// connect but also what to present as SNI — the origin answers to the rule's
    /// name, and the address it resolved to is an alias for it. Measured on the
    /// device: reaching a Steam Community address with the client's own name in
    /// SNI fails the handshake (`rc=35`), while the rule's name completes it and
    /// returns 200. The client's name and the origin's name are genuinely
    /// different questions here, and only this pairing answers the second.
    ///
    /// Paired rather than a bare list because one entry may name several hosts,
    /// and the host that answered is the one whose name has to be presented.
    ///
    /// It is a lookup table, not a parallel array to `addresses`: an address that
    /// is also listed literally appears in both, and an address no name produced
    /// appears in neither.
    ///
    /// Carried on the plan rather than looked up again after the connect, because
    /// the cache is refilled on a tick — a second read could describe a later
    /// tick than the addresses that were actually dialled.
    pub dial_addresses: Vec<(String, IpAddr)>,
    /// Candidates that must be dialled *without* SNI, when one of them answers.
    ///
    /// The third answer to "which name is the origin asked for". A rule's source
    /// sometimes has an address and no name for it, and asking such an origin for
    /// the client's name is measured to be refused — see
    /// [`CompiledEntry::no_sni`](crate::ruleset::CompiledEntry::no_sni).
    ///
    /// A subset of `addresses`, filtered to the same family for the same reason
    /// the pairing above is: a plan dials one family, and a flag on an address
    /// this plan will never dial would claim something that cannot happen.
    ///
    /// Only meaningful to a caller that re-originates TLS. A caller that relays
    /// ciphertext cannot change SNI and has no use for this — the client's own
    /// ClientHello already carries whatever it is going to carry.
    pub no_sni: Vec<IpAddr>,
    /// Preferred port from the rule, when it supplies one.
    pub port: Option<u16>,
    /// Certificate names from the rule, for diagnostics and future pinning.
    pub cert: Vec<String>,
    /// CDN placeholders declared by the rule.
    pub placeholders: Vec<Placeholder>,
}

impl Plan {
    /// A plan that leaves the connection alone.
    pub fn direct() -> Self {
        Self {
            query: None,
            strategy: Strategy::Direct,
            entry_key: None,
            entry_name: None,
            rule_domain: None,
            addresses: Vec::new(),
            dial_names: Vec::new(),
            dial_addresses: Vec::new(),
            no_sni: Vec::new(),
            port: None,
            cert: Vec::new(),
            placeholders: Vec::new(),
        }
    }

    /// True when the caller must resolve the name through the system resolver.
    ///
    /// Dial names do not count: when a rule names a host, the caller has a
    /// rule-supplied thing to resolve and must not quietly substitute the
    /// client's own name instead — that would discard the rule's whole point.
    pub fn needs_upstream_resolution(&self) -> bool {
        self.addresses.is_empty() && self.dial_names.is_empty()
    }
}

/// Rule set plus address history, the single entry point for routing decisions.
#[derive(Debug, Clone)]
pub struct Router {
    rules: RuleSet,
    selector: IpSelector,
    /// Addresses obtained by resolving the rules' dial names, keyed by entry.
    ///
    /// This is the equivalent of Caddy's `dynamic a <name>`: a name is re-resolved
    /// on a refresh tick rather than on every connection. Resolving per request
    /// would mean a DNS lookup per request to learn something that changes on the
    /// order of minutes, and it would put a blocking lookup in the engine's data
    /// path — which is why the engine had no dial-name support at all before this.
    ///
    /// Each address keeps the name that produced it. An entry may name several
    /// hosts, and a caller that re-originates TLS has to present the right one —
    /// see [`Plan::dial_addresses`].
    ///
    /// Keyed by entry because that is what a plan carries. Entry keys are indices
    /// into the current rule set, so the cache is dropped whenever the rules are
    /// replaced; stale keys would otherwise point at unrelated entries.
    resolved_dial: HashMap<u32, Vec<(String, IpAddr)>>,
}

impl Router {
    /// Build a router around an already compiled rule set.
    pub fn new(rules: RuleSet) -> Self {
        Self::with_selector(rules, IpSelectorConfig::default())
    }

    /// Build a router with the selector's own tuning.
    ///
    /// The selector is created here, with the router, so a caller that wants to
    /// change how address history is weighted has to do it at construction rather
    /// than afterwards. The shell exposes exactly one of these knobs — the failure
    /// cooldown — and hands it over through `StackConfig::failure_cooldown`; the
    /// rest of `IpSelectorConfig` stays at its default. Separate from [`Self::new`]
    /// so the many callers that want the defaults do not have to name them.
    pub fn with_selector(rules: RuleSet, selector: IpSelectorConfig) -> Self {
        Self {
            rules,
            selector: IpSelector::new(selector),
            resolved_dial: HashMap::new(),
        }
    }

    /// Build a router from the curated builtin rule set.
    pub fn builtin() -> Result<Self> {
        Ok(Self::new(RuleSet::from_str(
            BUILTIN_RULES_JSON,
            RuleSource::Builtin,
        )?))
    }

    pub fn rules(&self) -> &RuleSet {
        &self.rules
    }

    pub fn selector(&self) -> &IpSelector {
        &self.selector
    }

    /// Swap in a freshly downloaded rule set.
    ///
    /// Address history is intentionally preserved: it is keyed by address and
    /// remains valid across rule updates, which avoids re-learning the closest
    /// CDN node after every refresh.
    pub fn replace_rules(&mut self, rules: RuleSet) {
        self.rules = rules;
        // Entry keys index the rule set, so every cached resolution now points at
        // whatever entry happens to hold that index in the new document. Dropping
        // the cache is the only safe move; the next tick refills it.
        self.resolved_dial.clear();
    }

    /// Re-resolve every dial name the rule set names, caching the results.
    ///
    /// Called on a refresh tick, not per connection: the caller supplies the
    /// resolver, because resolving is I/O and this layer is pure so that policy
    /// can be tested without a network. Returns how many entries now have at
    /// least one address from a name.
    ///
    /// An entry whose names resolve to nothing **keeps its previous addresses**.
    /// Rebuilding the cache from scratch would mean a single transient lookup
    /// failure silently removes the entry's addresses until the next tick, and a
    /// DNS blip is exactly the moment the rule's own addresses are most valuable.
    /// Stale addresses are the safer failure: the selector will discover they are
    /// dead and demote them, which is a slower degradation than losing them.
    pub fn resolve_dial_names<F>(&mut self, mut resolve: F) -> usize
    where
        F: FnMut(&str) -> Vec<IpAddr>,
    {
        let mut resolved = 0usize;
        let mut cache: HashMap<u32, Vec<(String, IpAddr)>> = HashMap::new();
        for entry in self.rules.entries() {
            if entry.dial_names.is_empty() {
                continue;
            }
            // Deduplicated by address, keeping the first name that produced it.
            // Two names in one entry often resolve to the same edge (the puller's
            // upstream map has 2610 pairs over 75 distinct names), and the first
            // is the one the rule author ranked first.
            let mut addresses: Vec<(String, IpAddr)> = Vec::new();
            for name in &entry.dial_names {
                for addr in resolve(name) {
                    if !addr.is_unspecified() && !addresses.iter().any(|(_, seen)| *seen == addr) {
                        addresses.push((name.clone(), addr));
                    }
                }
            }
            if addresses.is_empty() {
                // Nothing came back this time. Keep what the last good tick found.
                if let Some(previous) = self.resolved_dial.get(&entry.key) {
                    cache.insert(entry.key, previous.clone());
                }
                continue;
            }
            cache.insert(entry.key, addresses);
            resolved += 1;
        }
        self.resolved_dial = cache;
        resolved
    }

    /// How many entries currently have addresses from a dial name.
    pub fn resolved_dial_entries(&self) -> usize {
        self.resolved_dial.len()
    }

    /// The cached resolutions for `key`, paired with their names and filtered to
    /// the family the caller asked for.
    ///
    /// Filtered by family here rather than at each call site because the cache is
    /// family-agnostic — a name is a lookup whose family is not known until it
    /// answers — while every plan is built for one family.
    fn named_addresses(&self, key: u32, family: Family) -> Vec<(String, IpAddr)> {
        self.resolved_dial
            .get(&key)
            .map(|pairs| {
                pairs
                    .iter()
                    .filter(|(_, addr)| family.admits(*addr))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Plan a connection for `domain` in the requested address `family`.
    pub fn plan(&self, now: Instant, domain: &str, family: Family) -> Plan {
        let Some(found) = self.rules.lookup(domain) else {
            return Plan::direct();
        };

        let entry = found.entry;
        let same_family: Vec<IpAddr> = entry
            .ips
            .iter()
            .copied()
            .filter(|addr| family.admits(*addr))
            .collect();

        // Dial names count as a usable plan: they are family-agnostic because the
        // address is not known until the lookup, so a v4 client can still be
        // served by a name that resolves to v4.
        let has_dial = !entry.dial_names.is_empty();
        let strategy = if !same_family.is_empty() || has_dial {
            Strategy::RuleAddresses
        } else if entry.ips.is_empty() {
            Strategy::PlaceholderFallback
        } else {
            Strategy::FamilyFallback
        };

        // Literal addresses first, then whatever the dial names resolved to on the
        // last tick. The rule's own order is a preference the author stated; a
        // name is a lookup that may have gone stale, so it follows.
        //
        // Membership is tested against a set built once rather than by scanning
        // the candidate list per name: the list can hold hundreds of addresses and
        // `Vec::contains` in that loop would make planning quadratic again.
        let (addresses, dial_addresses) = if strategy == Strategy::RuleAddresses {
            let mut candidates = self.selector.rank(now, &same_family);
            let named = self.named_addresses(entry.key, family);
            let mut present: HashSet<IpAddr> = candidates.iter().copied().collect();
            for (_, addr) in &named {
                if present.insert(*addr) {
                    candidates.push(*addr);
                }
            }
            (candidates, named)
        } else {
            (Vec::new(), Vec::new())
        };
        let dial_names = if strategy == Strategy::RuleAddresses {
            entry.dial_names.clone()
        } else {
            Vec::new()
        };
        // Gated on the same condition as the candidates, and filtered to the same
        // family: an address this plan will not dial must not be described as one
        // it would reach without a name.
        let no_sni = if strategy == Strategy::RuleAddresses {
            entry
                .no_sni
                .iter()
                .copied()
                .filter(|addr| family.admits(*addr))
                .collect()
        } else {
            Vec::new()
        };

        Plan {
            query: Some(found.query.clone()),
            strategy,
            entry_key: Some(entry.key),
            entry_name: Some(entry.name.clone()),
            rule_domain: Some(found.rule_domain.to_string()),
            addresses,
            dial_names,
            dial_addresses,
            no_sni,
            port: entry.port,
            cert: entry.cert.clone(),
            placeholders: entry.placeholders.clone(),
        }
    }

    /// Plan a connection when only the destination address is known.
    ///
    /// Without a domain there is nothing to match on, so this reports whether the
    /// address is claimed by the rule set and otherwise leaves the flow alone.
    ///
    /// This matters for clients that never let their DNS be observed — anything
    /// using DNS-over-HTTPS or a private resolver. The rule's own address list is
    /// still a usable signal: a client dialling one of those addresses is
    /// unambiguously talking to the service the entry describes.
    ///
    /// The dialled address is kept first. It is the one the client's own
    /// resolution produced, so it is known to be reachable from this network;
    /// putting a possibly stale rule address ahead of it would break a working
    /// connection. The remaining candidates follow in selector order, which is
    /// what makes a failed connect retry elsewhere instead of giving up.
    pub fn plan_for_ip(&self, now: Instant, addr: IpAddr) -> Plan {
        let keys = self.rules.entries_for_ip(addr);
        let Some(&key) = keys.first() else {
            return Plan::direct();
        };
        let Some(entry) = self.rules.entry(key) else {
            return Plan::direct();
        };

        let family = Family::of(addr);
        let same_family: Vec<IpAddr> = entry
            .ips
            .iter()
            .copied()
            .filter(|candidate| family.admits(*candidate))
            .collect();
        let mut addresses = self.selector.rank(now, &same_family);
        if let Some(position) = addresses.iter().position(|candidate| *candidate == addr) {
            addresses.swap(0, position);
        }
        if addresses.is_empty() {
            addresses.push(addr);
        }

        Plan {
            query: None,
            strategy: Strategy::RuleAddresses,
            entry_key: Some(entry.key),
            entry_name: Some(entry.name.clone()),
            rule_domain: entry.domains.first().cloned(),
            addresses,
            dial_names: entry.dial_names.clone(),
            dial_addresses: self.named_addresses(entry.key, family),
            no_sni: entry
                .no_sni
                .iter()
                .copied()
                .filter(|candidate| family.admits(*candidate))
                .collect(),
            port: entry.port,
            cert: entry.cert.clone(),
            placeholders: entry.placeholders.clone(),
        }
    }

    /// Record a successful connect against a candidate address.
    pub fn report_success(&mut self, addr: IpAddr, rtt: std::time::Duration, now: Instant) {
        self.selector.report_success(addr, rtt, now);
    }

    /// Record a failed connect against a candidate address.
    pub fn report_failure(&mut self, addr: IpAddr, now: Instant) {
        self.selector.report_failure(addr, now);
    }

    /// Record how a session ended, once its bytes have stopped flowing.
    ///
    /// [`Self::report_success`] fires when the connection opens; this fires when
    /// it closes. The gap between them is where "connected but nothing came back"
    /// lives, and that is the case a connect-only signal cannot see.
    pub fn report_outcome(&mut self, addr: IpAddr, outcome: Outcome, now: Instant) {
        self.selector.report_outcome(addr, outcome, now);
    }

    /// Whether this address is currently being avoided after a recent failure.
    ///
    /// A caller with several candidates does not need this — [`Self::plan`]
    /// already ranks them. A caller with one candidate does, because there is
    /// nothing to rank and the only remaining lever is the order of its own
    /// stages.
    pub fn is_cooled(&self, addr: IpAddr, now: Instant) -> bool {
        self.selector.is_cooled(addr, now)
    }

    /// Drop stale address history.
    pub fn prune(&mut self, now: Instant) {
        self.selector.prune(now);
    }

    /// Whether the rule set claims `domain` at all.
    pub fn is_rule_owned(&self, domain: &str) -> bool {
        self.rules.lookup(domain).is_some()
    }

    /// Look up the entry behind a domain without building a full plan.
    pub fn entry_for_domain(&self, domain: &str) -> Option<&CompiledEntry> {
        self.rules.lookup(domain).map(|found| found.entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::time::Duration;

    const DOC: &str = r#"{
      "meta": { "version": "t", "update_time": "t" },
      "groups": [
        { "group": "g", "entries": [
          { "id": "1", "name": "Concrete", "domains": ["concrete.example"], "ips": ["203.0.113.10", "203.0.113.20"], "port": "8443", "cert": "concrete.example,*.concrete.example", "isPlaceholder": false },
          { "id": "2", "name": "Placeholder", "domains": ["placeholder.example"], "ips": ["{Cloudflare}"], "port": "443", "isPlaceholder": true },
          { "id": "3", "name": "V6 only", "domains": ["v6.example"], "ips": ["2001:db8::1"], "port": "443", "isPlaceholder": false },
          { "id": "4", "name": "Dialled", "domains": ["dialled.example"], "ips": ["steamstore-a.akamaihd.net.edgesuite.net"], "port": "443", "isPlaceholder": false },
          { "id": "5", "name": "Both", "domains": ["both.example"], "ips": ["203.0.113.30", "edge.example.net", "other.example.net"], "port": "443", "isPlaceholder": false },
          { "id": "6", "name": "Unnamed", "domains": ["unnamed.example"], "ips": ["!203.0.113.40", "203.0.113.41"], "port": "443", "isPlaceholder": false },
          { "id": "7", "name": "Unnamed v6", "domains": ["unnamed6.example"], "ips": ["!2001:db8::7"], "port": "443", "isPlaceholder": false }
        ] }
      ]
    }"#;

    fn router() -> Router {
        Router::new(RuleSet::from_str(DOC, RuleSource::Provided).unwrap())
    }

    fn v4(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, last))
    }

    #[test]
    fn unmatched_domain_is_direct() {
        let plan = router().plan(Instant::now(), "unknown.example", Family::V4);
        assert_eq!(plan.strategy, Strategy::Direct);
        assert!(plan.needs_upstream_resolution());
        assert!(plan.entry_key.is_none());
    }

    #[test]
    fn concrete_rule_yields_ranked_addresses() {
        let r = router();
        let plan = r.plan(Instant::now(), "concrete.example", Family::V4);
        assert_eq!(plan.strategy, Strategy::RuleAddresses);
        assert_eq!(plan.addresses, vec![v4(10), v4(20)]);
        assert_eq!(plan.port, Some(8443));
        // `*.concrete.example` normalizes to the same name as `concrete.example`,
        // so the duplicate is collapsed during compilation.
        assert_eq!(plan.cert, vec!["concrete.example"]);
        assert!(!plan.needs_upstream_resolution());
    }

    #[test]
    fn a_flagged_address_rides_the_plan_as_one_to_reach_without_a_name() {
        // The rule named an address and no host, so the caller has to present
        // nothing. The unflagged address beside it is untouched — the flag is per
        // address, not per entry, because the two lines make different claims.
        let plan = router().plan(Instant::now(), "unnamed.example", Family::V4);
        assert_eq!(plan.strategy, Strategy::RuleAddresses);
        assert_eq!(plan.addresses, vec![v4(40), v4(41)]);
        assert_eq!(plan.no_sni, vec![v4(40)]);
        assert!(plan.dial_addresses.is_empty(), "no name was resolved here");
    }

    #[test]
    fn a_rule_that_names_a_host_has_nothing_to_leave_unnamed() {
        // The complement of the case above. A rule that named a host wants that
        // name presented, so the two lists must never both fill up for one entry
        // — that would be two contradictory answers to one question.
        let plan = router().plan(Instant::now(), "dialled.example", Family::V4);
        assert!(plan.no_sni.is_empty());
    }

    #[test]
    fn a_flag_on_an_address_this_plan_cannot_dial_is_dropped() {
        // The rule flags a v6 address and the caller asked for v4. This plan will
        // dial nothing, so saying it would reach something without a name would
        // describe a connection that cannot happen.
        let plan = router().plan(Instant::now(), "unnamed6.example", Family::V4);
        assert!(plan.addresses.is_empty());
        assert!(plan.no_sni.is_empty());

        let plan = router().plan(Instant::now(), "unnamed6.example", Family::V6);
        assert_eq!(plan.no_sni.len(), 1, "v6 asked, so v6 is answered");
    }

    #[test]
    fn a_plan_that_dials_nothing_flags_nothing() {
        // A placeholder-only entry yields no candidates at all: there is nothing
        // to reach and therefore nothing to leave unnamed.
        let plan = router().plan(Instant::now(), "placeholder.example", Family::V4);
        assert_eq!(plan.strategy, Strategy::PlaceholderFallback);
        assert!(plan.no_sni.is_empty());
    }

    #[test]
    fn planning_by_address_keeps_the_flag() {
        // The address-only path — what a client whose DNS was never observed
        // takes — builds its own candidate list, with the dialled address moved
        // first. The flag has to survive that, or the two paths would disagree
        // about the same rule.
        let plan = router().plan_for_ip(Instant::now(), v4(41));
        assert_eq!(plan.addresses, vec![v4(41), v4(40)]);
        assert_eq!(plan.no_sni, vec![v4(40)]);
    }

    #[test]
    fn a_dial_name_is_a_usable_plan_without_addresses() {
        // The rule knows a name but no address. That is still a plan: the caller
        // resolves it. Reporting `needs_upstream_resolution` here would make the
        // caller fall back to the *client's* name and discard the rule entirely.
        let plan = router().plan(Instant::now(), "dialled.example", Family::V4);
        assert_eq!(plan.strategy, Strategy::RuleAddresses);
        assert!(plan.addresses.is_empty());
        assert_eq!(
            plan.dial_names,
            vec!["steamstore-a.akamaihd.net.edgesuite.net"]
        );
        assert!(!plan.needs_upstream_resolution());
    }

    #[test]
    fn addresses_come_before_dial_names() {
        let plan = router().plan(Instant::now(), "both.example", Family::V4);
        assert_eq!(plan.strategy, Strategy::RuleAddresses);
        assert_eq!(plan.addresses, vec![v4(30)]);
        assert_eq!(
            plan.dial_names,
            vec!["edge.example.net", "other.example.net"]
        );
        assert!(!plan.needs_upstream_resolution());
    }

    #[test]
    fn a_dial_name_serves_a_client_in_either_family() {
        // The address is unknown until the lookup, so a name is not tied to a
        // family the way a literal address is.
        let r = router();
        for family in [Family::V4, Family::V6] {
            let plan = r.plan(Instant::now(), "dialled.example", family);
            assert_eq!(plan.strategy, Strategy::RuleAddresses);
            assert_eq!(plan.dial_names.len(), 1);
        }
    }

    #[test]
    fn an_entry_with_neither_address_nor_name_still_falls_back() {
        let plan = router().plan(Instant::now(), "placeholder.example", Family::V4);
        assert_eq!(plan.strategy, Strategy::PlaceholderFallback);
        assert!(plan.dial_names.is_empty());
        assert!(plan.needs_upstream_resolution());
    }

    #[test]
    fn resolving_a_dial_name_feeds_it_into_the_address_list() {
        // Before the tick runs there is nothing to dial; after it, the name's
        // addresses appear as ordinary candidates. This is what lets the engine —
        // which cannot block to resolve — use dial names at all.
        let mut r = router();
        let before = r.plan(Instant::now(), "dialled.example", Family::V4);
        assert!(before.addresses.is_empty());

        // Both dial-name entries in the fixture resolve, so the count is 2.
        let resolved = r.resolve_dial_names(|_| vec![v4(77)]);
        assert_eq!(resolved, 2);

        let after = r.plan(Instant::now(), "dialled.example", Family::V4);
        assert_eq!(after.addresses, vec![v4(77)]);
        assert!(!after.needs_upstream_resolution());
    }

    #[test]
    fn literal_addresses_still_come_before_resolved_names() {
        let mut r = router();
        r.resolve_dial_names(|_| vec![v4(88)]);
        let plan = r.plan(Instant::now(), "both.example", Family::V4);
        assert_eq!(plan.addresses, vec![v4(30), v4(88)]);
    }

    #[test]
    fn a_name_that_does_not_resolve_contributes_nothing() {
        let mut r = router();
        let resolved = r.resolve_dial_names(|_| Vec::new());
        assert_eq!(resolved, 0);
        assert_eq!(r.resolved_dial_entries(), 0);
        // The entry keeps its dial name but gains no address, so the caller still
        // has to resolve something — it just is not a rule address.
        let plan = r.plan(Instant::now(), "dialled.example", Family::V4);
        assert!(plan.addresses.is_empty());
    }

    #[test]
    fn a_resolved_name_travels_with_the_address_it_produced() {
        // The pairing is what lets a caller that re-originates TLS present the
        // *rule's* name: the origin answers to that name, not to the client's.
        // Without it the caller only knows an address and has to guess.
        let mut r = router();
        r.resolve_dial_names(|name| {
            if name == "edge.example.net" {
                vec![v4(88)]
            } else {
                vec![v4(77)]
            }
        });

        let plan = r.plan(Instant::now(), "dialled.example", Family::V4);
        assert_eq!(
            plan.dial_addresses,
            vec![(
                "steamstore-a.akamaihd.net.edgesuite.net".to_string(),
                v4(77)
            )]
        );
        assert_eq!(
            plan.addresses,
            vec![v4(77)],
            "the address is still dialable"
        );
    }

    #[test]
    fn each_name_keeps_its_own_address_in_the_pairing() {
        // One entry, several names: the pair is what says which name to present
        // for the address that actually answered. Pairing by position instead
        // would be wrong as soon as two names resolve to different addresses,
        // because the caller dials a set, not a list of names.
        let mut r = router();
        r.resolve_dial_names(|name| {
            if name == "edge.example.net" {
                vec![v4(88)]
            } else {
                vec![v4(77)]
            }
        });

        let plan = r.plan(Instant::now(), "both.example", Family::V4);
        assert_eq!(
            plan.dial_addresses,
            vec![
                ("edge.example.net".to_string(), v4(88)),
                ("other.example.net".to_string(), v4(77)),
            ]
        );
        // Literal first, then the names in document order.
        assert_eq!(plan.addresses, vec![v4(30), v4(88), v4(77)]);
    }

    #[test]
    fn a_name_that_produced_no_address_is_absent_from_the_pairing() {
        // The entry's first name fails, the second answers. The pairing must
        // claim the answer for the name that produced it and say nothing about
        // the one that did not — a caller reading a name off the wrong address
        // would present a name that origin never answered to.
        let mut r = router();
        r.resolve_dial_names(|name| {
            if name == "edge.example.net" {
                Vec::new()
            } else {
                vec![v4(77)]
            }
        });

        let plan = r.plan(Instant::now(), "both.example", Family::V4);
        assert_eq!(plan.addresses, vec![v4(30), v4(77)]);
        assert_eq!(
            plan.dial_addresses,
            vec![("other.example.net".to_string(), v4(77))]
        );
        // The literal address is nobody's answer, so no name may be attached.
        assert!(
            !plan.dial_addresses.iter().any(|(_, addr)| *addr == v4(30)),
            "no name produced that address"
        );
    }

    #[test]
    fn an_entry_whose_names_all_fail_keeps_its_literal_addresses() {
        // Every name fails but the rule still lists a literal address. That
        // address stays dialable and the pairing stays empty: there is no name
        // to present for it, so the caller falls back to the client's.
        let mut r = router();
        r.resolve_dial_names(|_| Vec::new());

        let plan = r.plan(Instant::now(), "both.example", Family::V4);
        assert_eq!(plan.addresses, vec![v4(30)]);
        assert!(plan.dial_addresses.is_empty());
        assert_eq!(plan.dial_names.len(), 2, "the names are still reported");
    }

    #[test]
    fn a_pairing_is_dropped_when_its_family_does_not_match() {
        // The cache is family-agnostic — a name's family is unknown until it
        // answers — but a plan is not. Keeping a pair for an address the plan
        // will never dial would make the pairing claim something false.
        let mut r = router();
        r.resolve_dial_names(|_| vec!["2001:db8::1".parse::<IpAddr>().unwrap()]);

        let plan = r.plan(Instant::now(), "dialled.example", Family::V4);
        assert!(plan.addresses.is_empty());
        assert!(plan.dial_addresses.is_empty());
        // The name itself is still reported: the rule did name a host.
        assert_eq!(
            plan.dial_names,
            vec!["steamstore-a.akamaihd.net.edgesuite.net"]
        );
    }

    #[test]
    fn a_rule_with_only_literal_addresses_pairs_nothing() {
        // Nothing resolved here, so there is no rule name to present. A caller
        // must fall back to the name the client asked for.
        let plan = router().plan(Instant::now(), "concrete.example", Family::V4);
        assert!(!plan.addresses.is_empty());
        assert!(plan.dial_addresses.is_empty());
    }

    #[test]
    fn a_transient_lookup_failure_keeps_the_last_good_addresses() {
        // A DNS blip must not silently strip a rule of its addresses until the
        // next tick — that is the moment the rule's own addresses matter most.
        let mut r = router();
        assert_eq!(r.resolve_dial_names(|_| vec![v4(77)]), 2);

        // The resolver now fails for everything, as a DNS outage would.
        assert_eq!(r.resolve_dial_names(|_| Vec::new()), 0);
        assert_eq!(r.resolved_dial_entries(), 2, "the good result is kept");

        let plan = r.plan(Instant::now(), "dialled.example", Family::V4);
        assert_eq!(plan.addresses, vec![v4(77)], "still dialable");

        // And a later successful tick replaces it.
        assert_eq!(r.resolve_dial_names(|_| vec![v4(78)]), 2);
        let plan = r.plan(Instant::now(), "dialled.example", Family::V4);
        assert_eq!(plan.addresses, vec![v4(78)]);
    }

    #[test]
    fn replacing_the_rules_drops_stale_resolutions() {
        // Entry keys index the rule set, so a cached resolution would otherwise
        // point at whatever entry happens to hold that index in the new document.
        let mut r = router();
        r.resolve_dial_names(|_| vec![v4(99)]);
        assert_eq!(r.resolved_dial_entries(), 2, "both dial-name entries resolved");
        r.replace_rules(RuleSet::from_str(DOC, RuleSource::Provided).unwrap());
        assert_eq!(r.resolved_dial_entries(), 0);
    }

    #[test]
    fn subdomain_inherits_the_parent_rule() {        let plan = router().plan(Instant::now(), "cdn.concrete.example", Family::V4);
        assert_eq!(plan.strategy, Strategy::RuleAddresses);
        assert_eq!(plan.rule_domain.as_deref(), Some("concrete.example"));
        assert_eq!(plan.query.as_deref(), Some("cdn.concrete.example"));
    }

    #[test]
    fn placeholder_rule_asks_for_upstream_resolution() {
        let plan = router().plan(Instant::now(), "placeholder.example", Family::V4);
        assert_eq!(plan.strategy, Strategy::PlaceholderFallback);
        assert!(plan.needs_upstream_resolution());
        assert_eq!(plan.placeholders, vec![Placeholder::Cloudflare]);
    }

    #[test]
    fn family_mismatch_falls_back_instead_of_inventing_an_answer() {
        let plan = router().plan(Instant::now(), "v6.example", Family::V4);
        assert_eq!(plan.strategy, Strategy::FamilyFallback);
        assert!(plan.needs_upstream_resolution());
    }

    #[test]
    fn failures_reorder_candidates_for_subsequent_plans() {
        let mut r = router();
        let now = Instant::now();
        r.report_success(v4(20), Duration::from_millis(8), now);
        r.report_failure(v4(10), now);
        let plan = r.plan(now, "concrete.example", Family::V4);
        assert_eq!(plan.addresses, vec![v4(20), v4(10)]);
    }

    #[test]
    fn reverse_ip_lookup_reports_ownership() {
        let r = router();
        let now = Instant::now();
        assert!(r.rules().owns_ip(v4(10)));
        assert_eq!(r.plan_for_ip(now, v4(10)).entry_key, Some(0));
        assert_eq!(r.plan_for_ip(now, v4(99)).strategy, Strategy::Direct);
    }

    #[test]
    fn a_dialled_rule_address_keeps_its_place_ahead_of_stale_candidates() {
        let mut r = router();
        let now = Instant::now();
        // Address .20 has a bad history and .10 is the one the client resolved
        // and dialled. The dialled address must stay first: it demonstrably works
        // from this network, while the rule's own list may be stale.
        r.report_failure(v4(20), now);

        let plan = r.plan_for_ip(now, v4(10));
        assert_eq!(plan.strategy, Strategy::RuleAddresses);
        assert_eq!(plan.addresses, vec![v4(10), v4(20)]);
        assert_eq!(plan.entry_name.as_deref(), Some("Concrete"));

        // Dialling the other address puts that one first instead.
        let plan = r.plan_for_ip(now, v4(20));
        assert_eq!(plan.addresses.first().copied(), Some(v4(20)));
    }

    #[test]
    fn a_dialled_address_without_rule_ownership_stays_direct() {
        let r = router();
        let now = Instant::now();
        let plan = r.plan_for_ip(now, v4(200));
        assert_eq!(plan.strategy, Strategy::Direct);
        assert!(plan.addresses.is_empty());
    }

    #[test]
    fn builtin_rule_set_compiles_and_matches() {
        let r = Router::builtin().expect("builtin rules must compile");
        assert!(r.is_rule_owned("raw.githubusercontent.com"));
        assert!(r.is_rule_owned("sub.github.com"), "subdomains must be covered");
        assert!(!r.is_rule_owned("example.com"));
        assert!(r.rules().stats().entries >= 25);
    }
}
