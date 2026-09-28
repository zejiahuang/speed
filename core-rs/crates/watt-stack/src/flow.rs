//! Flow identity and the address-to-domain observation cache.
//!
//! The data plane usually only sees addresses. A rule set, on the other hand, is
//! keyed by domain. The bridge between the two is DNS: every answer the kernel
//! forwards is recorded here, so when a connection to `203.0.113.10:443` starts a
//! moment later, the kernel can recover that it was `cdn.example.com` and apply
//! the matching rule.
//!
//! The cache is deliberately small and lossy. CDN addresses are shared by many
//! names, a name resolves to many addresses, and entries expire quickly, so the
//! structure is bounded on both axes and evicts in insertion order.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Identifies one direction-independent flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FlowKey {
    pub protocol: u8,
    pub src: IpAddr,
    pub dst: IpAddr,
    pub src_port: u16,
    pub dst_port: u16,
}

impl FlowKey {
    pub fn new(protocol: u8, src: IpAddr, dst: IpAddr, src_port: u16, dst_port: u16) -> Self {
        Self {
            protocol,
            src,
            dst,
            src_port,
            dst_port,
        }
    }
}

impl std::fmt::Display for FlowKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}:{} -> {}:{}",
            self.protocol, self.src, self.src_port, self.dst, self.dst_port
        )
    }
}

/// A domain that resolved to some address, with the time it stops being relevant.
#[derive(Debug, Clone)]
struct Observation {
    domain: String,
    expires_at: Instant,
}

/// Bounded map from a resolved address back to the names that produced it.
#[derive(Debug)]
pub struct ObservationCache {
    by_address: HashMap<IpAddr, Vec<Observation>>,
    /// Insertion order of addresses, for eviction.
    order: VecDeque<IpAddr>,
    max_addresses: usize,
    max_domains_per_address: usize,
}

impl ObservationCache {
    /// Create a cache bounded by `max_addresses` and `max_domains_per_address`.
    pub fn new(max_addresses: usize, max_domains_per_address: usize) -> Self {
        Self {
            by_address: HashMap::new(),
            order: VecDeque::new(),
            max_addresses: max_addresses.max(16),
            max_domains_per_address: max_domains_per_address.clamp(1, 32),
        }
    }

    /// Number of addresses currently tracked.
    pub fn len(&self) -> usize {
        self.by_address.len()
    }

    /// True when nothing is tracked.
    pub fn is_empty(&self) -> bool {
        self.by_address.is_empty()
    }

    /// Record that `domain` resolved to `address` for the next `ttl`.
    ///
    /// A zero or very small TTL is clamped upward: an observation that expires
    /// before the connection it describes is useless, and some resolvers return
    /// single-digit TTLs for load balanced names.
    pub fn record(&mut self, address: IpAddr, domain: &str, ttl: Duration, now: Instant) {
        let ttl = ttl.max(Duration::from_secs(5)).min(Duration::from_secs(3600));
        let expires_at = now + ttl;

        let bucket = self.by_address.entry(address).or_default();
        if bucket.is_empty() {
            self.order.push_back(address);
        }
        // Refresh an existing entry rather than duplicating it, so a repeated
        // answer extends the lifetime instead of filling the bucket.
        if let Some(existing) = bucket.iter_mut().find(|obs| obs.domain == domain) {
            existing.expires_at = expires_at;
        } else {
            if bucket.len() >= self.max_domains_per_address {
                bucket.remove(0);
            }
            bucket.push(Observation {
                domain: domain.to_string(),
                expires_at,
            });
        }

        while self.order.len() > self.max_addresses {
            if let Some(evicted) = self.order.pop_front() {
                self.by_address.remove(&evicted);
            }
        }
    }

    /// Domains still associated with `address`, most recently recorded first.
    pub fn domains(&self, address: IpAddr, now: Instant) -> Vec<&str> {
        let Some(bucket) = self.by_address.get(&address) else {
            return Vec::new();
        };
        bucket
            .iter()
            .rev()
            .filter(|obs| obs.expires_at > now)
            .map(|obs| obs.domain.as_str())
            .collect()
    }

    /// The single most likely domain for `address`, if any is still valid.
    pub fn primary_domain(&self, address: IpAddr, now: Instant) -> Option<&str> {
        self.domains(address, now).into_iter().next()
    }

    /// Drop expired observations. Cheap enough to call on a timer.
    pub fn prune(&mut self, now: Instant) {
        let before = self.by_address.len();
        self.by_address.retain(|_, bucket| {
            bucket.retain(|obs| obs.expires_at > now);
            !bucket.is_empty()
        });
        if self.by_address.len() != before {
            let live: std::collections::HashSet<IpAddr> =
                self.by_address.keys().copied().collect();
            self.order.retain(|address| live.contains(address));
        }
    }

    /// Forget everything, used when the rule set is replaced.
    pub fn clear(&mut self) {
        self.by_address.clear();
        self.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, last))
    }

    #[test]
    fn records_and_reads_back() {
        let mut cache = ObservationCache::new(64, 4);
        let now = Instant::now();
        cache.record(ip(1), "a.example", Duration::from_secs(60), now);
        cache.record(ip(1), "b.example", Duration::from_secs(60), now);

        let domains = cache.domains(ip(1), now);
        assert_eq!(domains.len(), 2);
        // The most recent observation comes first.
        assert_eq!(domains[0], "b.example");
        assert_eq!(cache.primary_domain(ip(1), now), Some("b.example"));
        assert!(cache.domains(ip(2), now).is_empty());
    }

    #[test]
    fn repeated_answers_refresh_rather_than_duplicate() {
        let mut cache = ObservationCache::new(64, 4);
        let now = Instant::now();
        cache.record(ip(1), "a.example", Duration::from_secs(30), now);
        cache.record(ip(1), "a.example", Duration::from_secs(30), now + Duration::from_secs(20));

        assert_eq!(cache.domains(ip(1), now).len(), 1);
        assert_eq!(cache.domains(ip(1), now + Duration::from_secs(20)).len(), 1);
    }

    #[test]
    fn entries_expire() {
        let mut cache = ObservationCache::new(64, 4);
        let now = Instant::now();
        cache.record(ip(1), "a.example", Duration::from_secs(10), now);
        assert_eq!(cache.domains(ip(1), now).len(), 1);
        assert!(cache.domains(ip(1), now + Duration::from_secs(11)).is_empty());
    }

    #[test]
    fn tiny_ttls_are_clamped_so_the_mapping_survives_the_connection() {
        let mut cache = ObservationCache::new(64, 4);
        let now = Instant::now();
        cache.record(ip(1), "a.example", Duration::from_millis(1), now);
        assert_eq!(cache.domains(ip(1), now + Duration::from_secs(1)).len(), 1);
    }

    #[test]
    fn per_address_domain_list_is_bounded() {
        let mut cache = ObservationCache::new(64, 2);
        let now = Instant::now();
        for name in ["a.example", "b.example", "c.example"] {
            cache.record(ip(1), name, Duration::from_secs(60), now);
        }
        let domains = cache.domains(ip(1), now);
        assert_eq!(domains.len(), 2);
        assert_eq!(domains[0], "c.example");
        assert_eq!(domains[1], "b.example");
    }

    #[test]
    fn address_count_is_bounded_and_evicts_oldest() {
        let mut cache = ObservationCache::new(16, 2);
        let now = Instant::now();
        for index in 0..40u8 {
            cache.record(ip(index), "x.example", Duration::from_secs(60), now);
        }
        assert!(cache.len() <= 16, "cache grew to {}", cache.len());
        assert!(cache.domains(ip(39), now).len() == 1, "newest entry must survive");
        assert!(cache.domains(ip(0), now).is_empty(), "oldest entry must be evicted");
    }

    #[test]
    fn prune_drops_expired_entries() {
        let mut cache = ObservationCache::new(64, 4);
        let now = Instant::now();
        cache.record(ip(1), "a.example", Duration::from_secs(10), now);
        cache.record(ip(2), "b.example", Duration::from_secs(600), now);
        cache.prune(now + Duration::from_secs(60));
        assert_eq!(cache.len(), 1);
        assert!(cache.domains(ip(1), now + Duration::from_secs(60)).is_empty());
        assert_eq!(cache.domains(ip(2), now + Duration::from_secs(60)).len(), 1);
    }

    #[test]
    fn flow_keys_are_direction_sensitive_and_hashable() {
        use std::collections::HashSet;
        let forward = FlowKey::new(6, ip(1), ip(2), 1234, 443);
        let backward = FlowKey::new(6, ip(2), ip(1), 443, 1234);
        assert_ne!(forward, backward);
        let mut set = HashSet::new();
        set.insert(forward);
        set.insert(backward);
        set.insert(forward);
        assert_eq!(set.len(), 2);
        assert_eq!(forward.to_string(), "6 203.0.113.1:1234 -> 203.0.113.2:443");
    }
}
