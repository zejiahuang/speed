//! Candidate address ranking.
//!
//! A rule entry such as `code.jquery.com` lists more than a hundred Fastly
//! addresses. Trying them in file order is wasteful and picking one at random is
//! worse, because the closest anycast node is stable for a given network.
//!
//! The selector therefore keeps a tiny amount of per-address history and sorts
//! candidates so that:
//!
//! 1. addresses inside a failure cooldown sink to the bottom,
//! 2. addresses with a measured round trip time come before unmeasured ones,
//! 3. otherwise the original document order is preserved.
//!
//! Rule 3 matters on first use: with no history at all the sort is stable, so the
//! upstream author's ordering is respected rather than being shuffled by an
//! arbitrary constant.
//!
//! ## There is real ranking room now, and what that changed
//!
//! An earlier version of this comment justified the "single candidate" special
//! case with "the rule source averages barely one address per domain". That was
//! measured against a source that no longer exists. Re-measured 2026-09-26
//! against the live `/1` (UsbEAm host records, host text, `#` comments and
//! loopback addresses skipped):
//!
//! | metric | old claim | measured 2026-09-26 |
//! |---|---:|---:|
//! | domains | 2879 | **5225** |
//! | addresses | ~2900 | **15952** |
//! | average addresses per domain | 1.03 | **3.05** |
//! | domains with more than one address | 79 (2.7%) | **5225 (100%)** |
//!
//! Every domain has more than one address, so ranking is no longer the
//! bottleneck: the selector has room to sort. Roughly 3 in 10 sampled domains
//! (25 sampled; 18/25 = 72% usable on one run, 17/25 = 68% on an independent
//! re-sample) still have **no usable address at all**, and the failures split
//! into three kinds that must not be conflated — a certificate mismatch (the
//! address connects but its chain does not cover the domain), a refusal
//! (connection refused / reset / EOF) and a timeout. Ranking can only reorder
//! addresses that exist; it cannot turn a wrong address into a working one.
//!
//! To re-measure: fetch `https://abhuang.dpdns.org/1`, skip `#` lines and
//! loopback rows, and count distinct final fields and rows; for the usability
//! figure, sample 25 domains and test each address with
//! `connect(ip, 443)` + `ssl.wrap_socket(server_hostname = domain)`.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// What a finished upstream session looked like.
///
/// The kernel never sees an HTTP status code — it forwards encrypted bytes and
/// terminates nothing. But it does see the *shape* of a session, and the shape is
/// enough to separate two failures:
///
///   * the connection never opened, and
///   * the connection opened and the far side never said anything.
///
/// The second case is the one the old connect/no-connect signal could not name: a
/// middlebox that completes the TCP handshake and then blackholes the flow used to
/// be recorded as a success.
///
/// **What this deliberately does not claim.** A client rejecting the certificate
/// is *not* detectable here, and an earlier version of this comment said it was.
/// Measured against a real Akamai edge, a rejected certificate looks like
/// `up=469 down=3105`: the server sends its certificate before the client can
/// refuse it, so the bytes flow normally and the session ends as `Healthy`. The
/// kernel cannot tell that rejection from a success, because telling them apart
/// requires knowing what the bytes meant, which is exactly what not terminating
/// TLS gives up.
///
/// This is a *transport* judgement, not an application one. A 404 is a perfectly
/// healthy exchange; the kernel must not learn to avoid an address because a page
/// did not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The connection was never established.
    Refused,
    /// The connection was established and the upstream sent something back.
    Healthy,
    /// The connection was established, and not one byte came back before it closed.
    Silent,
}

/// Per-address history kept by the selector.
#[derive(Debug, Clone, Default)]
pub struct IpStat {
    pub successes: u32,
    pub failures: u32,
    pub consecutive_failures: u32,
    /// Sessions that connected but returned nothing.
    ///
    /// Counted apart from `failures` because the two mean different things: one
    /// is "the road is closed", the other is "the road is open and nobody
    /// answers". Folding them together would lose the distinction that makes this
    /// signal worth collecting.
    pub silent: u32,
    /// Exponentially weighted moving average of the connect time, in milliseconds.
    pub ewma_rtt_ms: Option<f64>,
    pub last_success: Option<Instant>,
    pub last_failure: Option<Instant>,
}

impl IpStat {
    /// True when the address failed recently enough to be avoided for now.
    pub fn in_cooldown(&self, now: Instant, cooldown: Duration) -> bool {
        match self.last_failure {
            Some(at) => now.saturating_duration_since(at) < cooldown,
            None => false,
        }
    }

    /// Simple heuristic score used for reporting; lower is better.
    ///
    /// Silent sessions count against the ratio. They were recorded as successes by
    /// [`IpSelector::report_success`] — the connect did succeed — but a session
    /// that returned nothing did not serve, and a health figure that ignored that
    /// would flatter exactly the addresses this module exists to demote.
    pub fn health(&self) -> f64 {
        let total = self.successes + self.failures + self.silent;
        if total == 0 {
            return 1.0;
        }
        self.successes as f64 / total as f64
    }
}

/// Tuning knobs for [`IpSelector`].
#[derive(Debug, Clone, Copy)]
pub struct IpSelectorConfig {
    /// How long an address is deprioritized after a failure.
    pub failure_cooldown: Duration,
    /// Weight of the newest sample in the moving average, in `(0, 1]`.
    pub ewma_alpha: f64,
    /// Entries untouched for longer than this are dropped during [`IpSelector::prune`].
    pub retention: Duration,
}

impl Default for IpSelectorConfig {
    fn default() -> Self {
        Self {
            failure_cooldown: Duration::from_secs(60),
            ewma_alpha: 0.3,
            retention: Duration::from_secs(60 * 60 * 6),
        }
    }
}

/// Ranks candidate addresses using observed connect outcomes.
#[derive(Debug, Clone)]
pub struct IpSelector {
    config: IpSelectorConfig,
    stats: HashMap<IpAddr, IpStat>,
}

impl Default for IpSelector {
    fn default() -> Self {
        Self::new(IpSelectorConfig::default())
    }
}

impl IpSelector {
    pub fn new(config: IpSelectorConfig) -> Self {
        Self {
            config,
            stats: HashMap::new(),
        }
    }

    pub fn config(&self) -> IpSelectorConfig {
        self.config
    }

    /// Addresses currently tracked, useful for diagnostics.
    pub fn tracked(&self) -> usize {
        self.stats.len()
    }

    /// Return `candidates` ordered by preference. Duplicates are removed.
    pub fn rank(&self, now: Instant, candidates: &[IpAddr]) -> Vec<IpAddr> {
        // Sort key, in priority order:
        //   1. `cooled`        — recently failed addresses sink to the bottom,
        //   2. `silent`        — addresses that connect but never answer sink next.
        //                        Ahead of `rtt` because "nobody is there" is a
        //                        worse property than "slow": a slow address still
        //                        works, a silent one wastes the whole session,
        //   3. `rtt`           — measured addresses beat unmeasured ones,
        //   4. `consecutive`   — an address that just failed gives way to one that
        //                        has not, so a dead address is not retried on
        //                        every cooldown expiry,
        //   5. `index`         — document order, so first use is predictable.
        //
        // Cumulative failure count is deliberately *not* part of the key: an
        // address that succeeded a thousand times and failed once must not lose
        // its place to an address that has never been tried.
        // A `HashSet`, not a scan of the results so far. This is a public entry
        // point, so it stays correct for a caller that hands over duplicates —
        // but checking with `iter().any(...)` made it quadratic in the address
        // count, and an entry may list nearly a thousand addresses. The compiler
        // already stores each address once, so the common case pays one set
        // insertion per candidate and nothing more.
        let mut scored: Vec<(u8, u32, f64, u32, usize, IpAddr)> =
            Vec::with_capacity(candidates.len());
        let mut seen: HashSet<IpAddr> = HashSet::with_capacity(candidates.len());
        for (index, addr) in candidates.iter().enumerate() {
            if !seen.insert(*addr) {
                continue;
            }
            let stat = self.stats.get(addr);
            let cooled = stat
                .map(|stat| stat.in_cooldown(now, self.config.failure_cooldown))
                .unwrap_or(false);
            let silent = stat.map(|stat| stat.silent).unwrap_or(0);
            let rtt = stat
                .and_then(|stat| stat.ewma_rtt_ms)
                .unwrap_or(f64::INFINITY);
            let consecutive = stat.map(|stat| stat.consecutive_failures).unwrap_or(0);
            scored.push((u8::from(cooled), silent, rtt, consecutive, index, *addr));
        }
        // `sort_by` is stable, so equal keys keep document order.
        scored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| a.3.cmp(&b.3))
                .then_with(|| a.4.cmp(&b.4))
        });
        scored
            .into_iter()
            .map(|(_, _, _, _, _, addr)| addr)
            .collect()
    }

    /// Record a successful connect and fold its round trip time into the average.
    pub fn report_success(&mut self, addr: IpAddr, rtt: Duration, now: Instant) {
        let rtt_ms = rtt.as_secs_f64() * 1000.0;
        let alpha = self.config.ewma_alpha.clamp(0.01, 1.0);
        let stat = self.stats.entry(addr).or_default();
        stat.successes = stat.successes.saturating_add(1);
        stat.consecutive_failures = 0;
        stat.last_success = Some(now);
        stat.ewma_rtt_ms = Some(match stat.ewma_rtt_ms {
            Some(previous) => previous * (1.0 - alpha) + rtt_ms * alpha,
            None => rtt_ms,
        });
    }

    /// Record a failed connect. The address is deprioritized for the cooldown.
    pub fn report_failure(&mut self, addr: IpAddr, now: Instant) {
        let stat = self.stats.entry(addr).or_default();
        stat.failures = stat.failures.saturating_add(1);
        stat.consecutive_failures = stat.consecutive_failures.saturating_add(1);
        stat.last_failure = Some(now);
    }

    /// Record how a session ended, once the connection is over.
    ///
    /// This is the counterpart to [`Self::report_success`], which is called when
    /// the connection *opens*. A session can open perfectly and still be useless,
    /// and only the byte counters know the difference.
    ///
    /// `Silent` deliberately does not touch `last_failure` or the cooldown: the
    /// address is reachable, so the failure cooldown — which means "the road is
    /// closed" — would be the wrong conclusion. It moves down the ranking instead,
    /// which is enough to prefer another candidate without writing the address off.
    pub fn report_outcome(&mut self, addr: IpAddr, outcome: Outcome, now: Instant) {
        match outcome {
            Outcome::Refused => self.report_failure(addr, now),
            Outcome::Healthy => {
                let stat = self.stats.entry(addr).or_default();
                stat.silent = 0;
            }
            Outcome::Silent => {
                let stat = self.stats.entry(addr).or_default();
                stat.silent = stat.silent.saturating_add(1);
            }
        }
    }

    /// Whether `addr` is currently being avoided after a recent failure.
    ///
    /// [`Self::rank`] already sinks such an address to the bottom, but sinking is
    /// meaningless when there is only one candidate: it is bottom and top at
    /// once. That case is now **rare rather than the norm** — `/1` measured
    /// 2026-09-26 gives 5225 domains and 15952 addresses (3.05 per domain), and
    /// every one of those 5225 domains lists more than one address. The caller
    /// still needs to ask the question directly, because a domain can be
    /// *reduced* to a single candidate — for instance when every other address
    /// was rejected outright — and then it can only reorder its *stages*
    /// instead, which is the only lever left when there is nothing to rank.
    pub fn is_cooled(&self, addr: IpAddr, now: Instant) -> bool {
        self.stats
            .get(&addr)
            .map(|stat| stat.in_cooldown(now, self.config.failure_cooldown))
            .unwrap_or(false)
    }

    /// Drop history for addresses that have not been touched recently.
    pub fn prune(&mut self, now: Instant) {
        let retention = self.config.retention;
        self.stats.retain(|_, stat| {
            let last = [stat.last_success, stat.last_failure].into_iter().flatten().max();
            match last {
                Some(at) => now.saturating_duration_since(at) < retention,
                None => true,
            }
        });
    }

    /// Snapshot of the history, sorted by health then address for stable output.
    pub fn snapshot(&self) -> Vec<(IpAddr, IpStat)> {
        let mut rows: Vec<(IpAddr, IpStat)> = self
            .stats
            .iter()
            .map(|(addr, stat)| (*addr, stat.clone()))
            .collect();
        rows.sort_by(|a, b| {
            b.1.health()
                .partial_cmp(&a.1.health())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        rows
    }

    /// Forget all history, used when the rule set is replaced.
    pub fn clear(&mut self) {
        self.stats.clear();
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
    fn preserves_document_order_without_history() {
        let selector = IpSelector::default();
        let candidates = vec![ip(1), ip(2), ip(3)];
        assert_eq!(selector.rank(Instant::now(), &candidates), candidates);
    }

    #[test]
    fn deduplicates_candidates() {
        let selector = IpSelector::default();
        let ranked = selector.rank(Instant::now(), &[ip(1), ip(1), ip(2)]);
        assert_eq!(ranked, vec![ip(1), ip(2)]);
    }

    #[test]
    fn promotes_measured_fast_addresses() {
        let mut selector = IpSelector::default();
        let now = Instant::now();
        selector.report_success(ip(3), Duration::from_millis(20), now);
        let ranked = selector.rank(now, &[ip(1), ip(2), ip(3)]);
        assert_eq!(ranked[0], ip(3));
        // The untouched addresses keep their relative order behind it.
        assert_eq!(ranked[1], ip(1));
        assert_eq!(ranked[2], ip(2));
    }

    #[test]
    fn sinks_recently_failed_addresses() {
        let mut selector = IpSelector::default();
        let now = Instant::now();
        selector.report_success(ip(1), Duration::from_millis(5), now);
        selector.report_failure(ip(1), now);
        let ranked = selector.rank(now, &[ip(1), ip(2)]);
        assert_eq!(ranked, vec![ip(2), ip(1)], "cooled down address must be last");
    }

    #[test]
    fn cooldown_expires() {
        let config = IpSelectorConfig {
            failure_cooldown: Duration::from_millis(10),
            ..IpSelectorConfig::default()
        };
        let mut selector = IpSelector::new(config);
        let now = Instant::now();
        selector.report_failure(ip(1), now);
        let later = now + Duration::from_millis(50);
        let ranked = selector.rank(later, &[ip(1), ip(2)]);
        // The cooldown is over, so ip(1) is no longer forced to the back. It still
        // trails ip(2), which has not failed, so a dead address is not retried on
        // every cooldown expiry.
        assert_eq!(ranked, vec![ip(2), ip(1)]);
    }

    #[test]
    fn one_historic_failure_does_not_lose_to_an_untried_address() {
        let mut selector = IpSelector::default();
        let now = Instant::now();
        // ip(1) is proven fast, then has a single bad moment which is later
        // followed by a success. It must stay ahead of the untried ip(2).
        selector.report_success(ip(1), Duration::from_millis(15), now);
        selector.report_failure(ip(1), now);
        selector.report_success(ip(1), Duration::from_millis(16), now + Duration::from_secs(120));
        let ranked = selector.rank(now + Duration::from_secs(121), &[ip(1), ip(2)]);
        assert_eq!(ranked, vec![ip(1), ip(2)]);
    }

    #[test]
    fn ewma_smooths_repeated_samples() {
        let mut selector = IpSelector::default();
        let now = Instant::now();
        selector.report_success(ip(1), Duration::from_millis(100), now);
        selector.report_success(ip(1), Duration::from_millis(200), now);
        let ewma = selector.snapshot()[0].1.ewma_rtt_ms.expect("rtt recorded");
        assert!(ewma > 100.0 && ewma < 200.0, "unexpected ewma {ewma}");
    }

    #[test]
    fn a_silent_session_demotes_without_closing_the_road() {
        // A silent address is reachable, so it must not enter the failure
        // cooldown — that would claim the road is closed. It only loses its place.
        let mut selector = IpSelector::default();
        let now = Instant::now();
        selector.report_outcome(ip(1), Outcome::Silent, now);
        let stat = &selector.snapshot()[0].1;
        assert_eq!(stat.silent, 1);
        assert_eq!(stat.failures, 0, "a reachable address is not a failed connect");
        assert!(stat.last_failure.is_none());
        let ranked = selector.rank(now, &[ip(1), ip(2)]);
        assert_eq!(ranked, vec![ip(2), ip(1)], "the silent address gives way");
    }

    #[test]
    fn silence_outranks_slowness() {
        // A slow address still works; a silent one wastes the whole session. The
        // slow one must therefore be preferred.
        let mut selector = IpSelector::default();
        let now = Instant::now();
        selector.report_success(ip(1), Duration::from_millis(900), now);
        selector.report_outcome(ip(1), Outcome::Silent, now);
        selector.report_success(ip(2), Duration::from_millis(50), now);
        let ranked = selector.rank(now, &[ip(1), ip(2)]);
        assert_eq!(ranked, vec![ip(2), ip(1)]);
    }

    #[test]
    fn a_healthy_session_clears_the_silence_count() {
        let mut selector = IpSelector::default();
        let now = Instant::now();
        selector.report_outcome(ip(1), Outcome::Silent, now);
        selector.report_outcome(ip(1), Outcome::Silent, now);
        selector.report_outcome(ip(1), Outcome::Healthy, now);
        assert_eq!(selector.snapshot()[0].1.silent, 0);
    }

    #[test]
    fn a_refused_outcome_is_an_ordinary_failure() {
        let mut selector = IpSelector::default();
        let now = Instant::now();
        selector.report_outcome(ip(1), Outcome::Refused, now);
        let stat = &selector.snapshot()[0].1;
        assert_eq!(stat.failures, 1);
        assert_eq!(stat.silent, 0);
        assert!(stat.in_cooldown(now, Duration::from_secs(60)));
    }

    #[test]
    fn is_cooled_answers_for_the_single_candidate_case() {
        // `rank` sinks a cooled address, but with one candidate "sunk" still
        // means "first". The caller needs to ask directly so it can reorder its
        // stages instead, which is the only lever left once a domain has been
        // narrowed to a single candidate (rare on `/1`, where every domain lists
        // more than one address, but reachable when the others are rejected).
        let mut selector = IpSelector::default();
        let now = Instant::now();
        assert!(!selector.is_cooled(ip(1), now), "untried is not cooled");
        selector.report_failure(ip(1), now);
        assert!(selector.is_cooled(ip(1), now));
        assert!(!selector.is_cooled(ip(2), now), "a different address is unaffected");
    }

    #[test]
    fn a_silent_address_is_not_reported_as_cooled() {
        // Silence is not "the road is closed", so it must not take the address
        // out of the running — only out of first place.
        let mut selector = IpSelector::default();
        let now = Instant::now();
        selector.report_outcome(ip(1), Outcome::Silent, now);
        assert!(!selector.is_cooled(ip(1), now));
    }

    #[test]
    fn a_silent_session_lowers_the_reported_health() {
        // The connect succeeded, so `successes` rose. But the session served
        // nothing, and a health figure that ignored that would flatter the very
        // addresses this module exists to demote.
        let mut selector = IpSelector::default();
        let now = Instant::now();
        selector.report_success(ip(1), Duration::from_millis(10), now);
        selector.report_outcome(ip(1), Outcome::Silent, now);
        let stat = &selector.snapshot()[0].1;
        assert_eq!(stat.successes, 1, "the connect did succeed");
        assert!(stat.health() < 1.0, "but the session did not");
    }
}
