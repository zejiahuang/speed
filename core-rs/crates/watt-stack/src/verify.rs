//! Certificate verdicts for the tunnel's dial path.
//!
//! # Why the tunnel needs this at all
//!
//! The proxy sees `CONNECT github.com:443` and can check a candidate address
//! against the name before dialling. The tunnel sees a TCP packet to
//! `140.82.121.3:443` and has no name at all — until it asks the planner, which
//! has been recording address-to-domain mappings from the DNS it forwards.
//!
//! With that name it can do the same check, and it needs to for the same reason:
//! measured on a real device, a rule gave `github.com` thirty-nine addresses of
//! which about ten were real GitHub and the rest either timed out or answered
//! with someone else's certificate. The selector ranks by RTT, so the fastest
//! wrong address won every time, and the client rejected the certificate on a
//! connection that would otherwise have worked.
//!
//! # Why it is asynchronous
//!
//! **The relay loop must not block.** It is one loop driving every flow; a probe
//! takes a handshake, and doing that inline would stall every other connection
//! for the duration. So verdicts are produced by detached threads and consumed
//! from a shared table.
//!
//! The cost is a short wait on the first connection to a domain: the flow holds
//! off dialling until the verdict arrives or [`WAIT`] expires. That is invisible
//! in practice — the client is already connected to the listener and waiting for
//! a handshake, so the wait is latency on a handshake that was going to take a
//! round trip anyway.
//!
//! # A verdict is per pair
//!
//! Keyed by host *and* address, because a shared host serves many certificates.
//! A verdict of "does not cover" is a statement about that combination only.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use watt_net::probe;

/// How long a flow waits for a verdict before dialling anyway.
///
/// Long enough for a handshake on a slow link, short enough that a client would
/// not notice. The alternative — waiting for the probe's own timeout — would add
/// seconds to the first connection to every domain.
pub const WAIT: Duration = Duration::from_millis(900);

/// How long a verdict is trusted.
///
/// A certificate is valid for months and the address serving it changes even
/// less often. Being wrong costs one failed request; probing every time costs on
/// every request.
const TTL: Duration = Duration::from_secs(600);

/// How long "nothing accepted the connection" is trusted.
///
/// Much shorter than [`TTL`], because the two rejections are not the same kind
/// of fact. "This address serves the wrong certificate" is a property of the
/// address and holds for as long as its certificate does; "nothing answered" is
/// a property of *this moment* — the address may be behind a route that a phone
/// that just changed networks does not have yet. Remembering a transient
/// blackout for ten minutes is how a working address stays benched after the
/// link recovers.
const UNREACHABLE_TTL: Duration = Duration::from_secs(60);

/// How long an in-flight marker is believed before it is treated as abandoned.
///
/// A probe runs on a detached thread. If that thread never gets to write its
/// verdict — it panicked, or the process is under enough pressure that the spawn
/// died — the marker would otherwise sit there forever and the address would be
/// skipped by every later `check` while `get` kept answering `None`. That is an
/// address that can never be tried again for the life of the tunnel. Long enough
/// to cover a real handshake on a slow link, short enough that a lost probe
/// costs one retry rather than the address.
const IN_FLIGHT_TTL: Duration = Duration::from_secs(15);

/// Why an address was ruled out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Doubt {
    /// The address answered, and its certificate does not cover the host.
    WrongCertificate,
    /// Nothing accepted the connection. Could be a dead address; could be this
    /// network, this minute.
    Unreachable,
}

impl Doubt {
    fn ttl(self) -> Duration {
        match self {
            Doubt::WrongCertificate => TTL,
            Doubt::Unreachable => UNREACHABLE_TTL,
        }
    }
}

/// One entry in the verdict table.
///
/// The states are distinct on purpose. An earlier version stored a bare
/// `(bool, Instant)` and used the same shape for "being probed" and "probed",
/// which made `any_pending` answer with mere key presence — so the second
/// connection to a host found every key present, concluded nothing was pending,
/// and dialled at once without waiting for an answer that had not arrived. The
/// verdict that mattered then landed too late to be used, and the flow went out
/// on the address the probe was about to reject. Making "in flight" its own
/// state is what keeps that from depending on a coincidence of representation.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Entry {
    /// A probe is running. Not a verdict — only a promise of one.
    InFlight { started: Instant },
    /// A probe concluded: this address serves a certificate for the host.
    Covers { at: Instant },
    /// A probe concluded against it, and why — which decides how long the
    /// conclusion is worth keeping.
    Doubtful { why: Doubt, at: Instant },
}

/// The table: a verdict and when it was reached, per host and address.
type Table = HashMap<(String, IpAddr), Entry>;

/// The verdict table, shared between the relay loop and the probes.
#[derive(Clone, Default)]
pub struct Verdicts {
    inner: Arc<Mutex<Table>>,
}

impl std::fmt::Debug for Verdicts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Only the size: the table is a cache, and printing every pair would bury
        // the line it appears on.
        let known = self.inner.lock().map(|table| table.len()).unwrap_or(0);
        f.debug_struct("Verdicts").field("known", &known).finish()
    }
}

impl Verdicts {
    pub fn new() -> Self {
        Self::default()
    }

    /// What is known about this pair, if anything recent.
    ///
    /// A probe still running is not knowledge, and a placeholder must never be
    /// read as a `false` verdict: doing so would tell the dial path that an
    /// address serves the wrong certificate when nobody has asked it yet.
    pub fn get(&self, host: &str, address: IpAddr) -> Option<bool> {
        let table = self.inner.lock().ok()?;
        match table.get(&(host.to_string(), address))? {
            // Still running, or abandoned by a thread that never reported. Either
            // way there is no verdict to give — and an abandoned one must not
            // keep answering `None` forever, which is why the caller is allowed
            // to probe it again once `IN_FLIGHT_TTL` has passed.
            Entry::InFlight { .. } => None,
            Entry::Covers { at } => (at.elapsed() <= TTL).then_some(true),
            Entry::Doubtful { why, at } => (at.elapsed() <= why.ttl()).then_some(false),
        }
    }

    /// A read-only view of the table, for reporting to the shell.
    ///
    /// Freshness is judged by the same rules `get` uses, so a report can never
    /// show an address as "wrong certificate" after the conclusion has expired
    /// and the dial path has gone back to trying it. An entry that is no longer
    /// knowledge is simply absent — the same reason `get` returns `None` for an
    /// `InFlight` marker: a probe still running is a promise, not a verdict.
    pub fn snapshot(&self) -> Vec<(String, IpAddr, &'static str, Duration)> {
        let Ok(table) = self.inner.lock() else {
            return Vec::new();
        };
        let mut rows = Vec::with_capacity(table.len());
        for ((host, address), entry) in table.iter() {
            // Each arm applies its own entry's TTL, exactly as `get` does. The
            // state strings are the wire names the shell matches on, so they are
            // fixed here rather than spelled at the call site.
            let (state, age, ttl) = match entry {
                Entry::InFlight { started } => ("in_flight", started.elapsed(), IN_FLIGHT_TTL),
                Entry::Covers { at } => ("covers", at.elapsed(), TTL),
                Entry::Doubtful { why, at } => (
                    match why {
                        Doubt::WrongCertificate => "wrong_certificate",
                        Doubt::Unreachable => "unreachable",
                    },
                    at.elapsed(),
                    why.ttl(),
                ),
            };
            if age <= ttl {
                rows.push((host.clone(), *address, state, age));
            }
        }
        rows
    }

    fn put(&self, host: &str, address: IpAddr, verdict: Entry) {
        let Ok(mut table) = self.inner.lock() else {
            return;
        };
        table.insert((host.to_string(), address), verdict);
        // Bounded: a long-lived tunnel sees many hosts, and an unbounded table is
        // a leak with extra steps. Clearing is fine — it is a cache.
        if table.len() > 4096 {
            table.clear();
        }
    }

    /// Record a verdict directly, for tests that need a settled answer without
    /// waiting on a real handshake.
    #[cfg(test)]
    pub(crate) fn put_for_test(&self, host: &str, address: IpAddr, verdict: Entry) {
        self.put(host, address, verdict);
    }

    /// Start checking `addresses` against `host`, on detached threads.
    ///
    /// Returns immediately. Addresses already known, or already being checked,
    /// are skipped so that a burst of connections to one domain does not start a
    /// probe per connection.
    pub fn check(&self, host: &str, addresses: &[IpAddr]) {
        let mut pending = Vec::new();
        {
            let Ok(mut table) = self.inner.lock() else {
                return;
            };
            for address in addresses {
                let key = (host.to_string(), *address);
                match table.get(&key) {
                    // A settled verdict that is still fresh, under its own TTL.
                    Some(Entry::Covers { at }) if at.elapsed() <= TTL => continue,
                    Some(Entry::Doubtful { why, at }) if at.elapsed() <= why.ttl() => continue,
                    // Already being probed: one probe per pair, not one per
                    // connection. A marker older than `IN_FLIGHT_TTL` is a probe
                    // that never reported, so it is re-asked rather than trusted.
                    Some(Entry::InFlight { started }) if started.elapsed() <= IN_FLIGHT_TTL => {
                        continue
                    }
                    // Absent, stale, or abandoned.
                    Some(_) | None => {
                        table.insert(
                            key,
                            Entry::InFlight {
                                started: Instant::now(),
                            },
                        );
                        pending.push(*address);
                    }
                }
            }
        }

        for address in pending {
            let name = host.to_string();
            let verdicts = self.clone();
            // A second copy for the closure: the outer one is still needed on the
            // path where spawning fails.
            let worker_name = name.clone();
            let spawned = std::thread::Builder::new()
                .name("watt-cert".to_string())
                .spawn(move || {
                    let target = SocketAddr::new(address, 443);
                    match probe::check(target, &worker_name) {
                        probe::Probe::Covers => verdicts.put(
                            &worker_name,
                            address,
                            Entry::Covers {
                                at: Instant::now(),
                            },
                        ),
                        // The address answered with someone else's certificate.
                        // A property of the address, so remembered for a long
                        // time.
                        probe::Probe::DoesNotCover => verdicts.put(
                            &worker_name,
                            address,
                            Entry::Doubtful {
                                why: Doubt::WrongCertificate,
                                at: Instant::now(),
                            },
                        ),
                        // Nothing answered. Also a reason not to prefer the
                        // address — the tunnel's failover pays a full connect
                        // timeout per candidate — but a much weaker claim, so it
                        // is forgotten quickly.
                        probe::Probe::Unreachable => verdicts.put(
                            &worker_name,
                            address,
                            Entry::Doubtful {
                                why: Doubt::Unreachable,
                                at: Instant::now(),
                            },
                        ),
                        // Inconclusive: forget it, so a later connection retries
                        // rather than treating "could not tell" as "no".
                        probe::Probe::Inconclusive => {
                            if let Ok(mut table) = verdicts.inner.lock() {
                                table.remove(&(worker_name.clone(), address));
                            }
                        }
                    }
                });
            if spawned.is_err() {
                // Out of threads. Leave it unverified rather than marking it bad.
                if let Ok(mut table) = self.inner.lock() {
                    table.remove(&(name, address));
                }
            }
        }
    }

    /// Whether any address in the list has no settled verdict yet.
    ///
    /// Used to decide whether waiting is worth it: with no verdicts and no
    /// pending probes, a flow would only be delaying for nothing. An address
    /// being probed right now counts, because its answer is exactly what the
    /// wait exists to collect — missing that is how a flow came to dial the
    /// address the probe was about to reject.
    pub fn any_pending(&self, host: &str, addresses: &[IpAddr]) -> bool {
        let Ok(table) = self.inner.lock() else {
            return false;
        };
        addresses.iter().any(|address| {
            match table.get(&(host.to_string(), *address)) {
                None => true,
                // A live probe is what the wait exists to collect. An abandoned
                // one is not, or the flow would wait out its whole budget for an
                // answer that is never coming.
                Some(Entry::InFlight { started }) => started.elapsed() <= IN_FLIGHT_TTL,
                Some(Entry::Covers { at }) => at.elapsed() > TTL,
                Some(Entry::Doubtful { why, at }) => at.elapsed() > why.ttl(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn address(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, last))
    }

    /// Record a settled verdict without running a probe.
    fn settle(verdicts: &Verdicts, host: &str, address: IpAddr, covers: bool) {
        let entry = if covers {
            Entry::Covers {
                at: Instant::now(),
            }
        } else {
            Entry::Doubtful {
                why: Doubt::WrongCertificate,
                at: Instant::now(),
            }
        };
        verdicts.put(host, address, entry);
    }

    #[test]
    fn an_unknown_pair_has_no_verdict() {
        let verdicts = Verdicts::new();
        assert_eq!(verdicts.get("github.com", address(1)), None);
    }

    #[test]
    fn a_probe_in_flight_is_not_a_verdict() {
        // The bug this guards: a placeholder used to look like a settled "does
        // not cover", so anything reading the table before the answer arrived
        // saw a rejection that nobody had reached.
        let verdicts = Verdicts::new();
        verdicts.check("github.com", &[address(1)]);
        assert_eq!(verdicts.get("github.com", address(1)), None);
    }

    #[test]
    fn a_verdict_is_per_host_and_address() {
        // A shared host serves many certificates: the same address can cover one
        // name and not another.
        let verdicts = Verdicts::new();
        settle(&verdicts, "github.com", address(1), true);
        assert_eq!(verdicts.get("github.com", address(1)), Some(true));
        assert_eq!(verdicts.get("example.com", address(1)), None);
        assert_eq!(verdicts.get("github.com", address(2)), None);
    }

    #[test]
    fn an_in_flight_probe_counts_as_pending_so_the_dial_waits_for_it() {
        // The second half of the same bug. `any_pending` used to answer with
        // key presence, so once a burst had registered every address the next
        // connection saw nothing pending, dialled immediately, and went out on
        // the address the probe was about to reject. A probe that is running is
        // exactly what the wait exists to collect.
        let verdicts = Verdicts::new();
        let addresses = [address(1), address(2)];
        verdicts.check("github.com", &addresses);
        assert!(
            verdicts.any_pending("github.com", &addresses),
            "a running probe must keep the dial waiting"
        );
    }

    #[test]
    fn a_settled_verdict_stops_counting_as_pending() {
        let verdicts = Verdicts::new();
        settle(&verdicts, "github.com", address(1), true);
        settle(&verdicts, "github.com", address(2), false);
        assert!(!verdicts.any_pending("github.com", &[address(1), address(2)]));
    }

    #[test]
    fn an_address_nothing_is_listening_on_is_recorded_as_unusable() {
        // A refused connection is a fact about the address, and the tunnel acts
        // on it: its failover is sequential and pays a full connect timeout per
        // candidate, so leaving dead addresses in the list is what made
        // `github.com` take forty seconds to fail.
        let verdicts = Verdicts::new();
        // Nothing is listening on 127.0.0.1:443 in the test environment.
        verdicts.check("example.com", &[IpAddr::V4(Ipv4Addr::LOCALHOST)]);

        // The probe runs on its own thread; wait for it to conclude.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if verdicts.get("example.com", IpAddr::V4(Ipv4Addr::LOCALHOST)) == Some(false) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the refused connection was never recorded");
    }

    #[test]
    fn a_verdict_of_bad_is_only_useful_next_to_a_verdict_of_good() {
        // The rule the tunnel applies before shortening a candidate list. Stated
        // here because getting it wrong is not obvious: dropping every address
        // that is merely unverified leaves a flow with one bad candidate instead
        // of thirty-nine, which turned a working request into a failed one.
        let verdicts = Verdicts::new();
        settle(&verdicts, "github.com", address(1), false);
        settle(&verdicts, "github.com", address(2), false);

        let candidates = [address(1), address(2), address(3)];
        let confirmed_good = candidates
            .iter()
            .any(|candidate| verdicts.get("github.com", *candidate) == Some(true));
        assert!(
            !confirmed_good,
            "two rejections and no confirmation is not grounds to shorten the list"
        );

        settle(&verdicts, "github.com", address(3), true);
        let confirmed_good = candidates
            .iter()
            .any(|candidate| verdicts.get("github.com", *candidate) == Some(true));
        assert!(confirmed_good, "now there is something to prefer");
    }

    #[test]
    fn a_second_check_of_a_known_pair_does_not_spawn_again() {
        let verdicts = Verdicts::new();
        settle(&verdicts, "github.com", address(1), true);
        assert!(!verdicts.any_pending("github.com", &[address(1)]));
        assert!(verdicts.any_pending("github.com", &[address(1), address(2)]));
    }

    #[test]
    fn an_unreachable_verdict_is_forgotten_sooner_than_a_wrong_certificate() {
        // "Nothing answered" is a statement about this minute — a phone that just
        // changed networks has no route yet — while "the certificate is for
        // someone else" holds until the certificate changes. Benching an address
        // for ten minutes after a momentary blackout is how a working address
        // stays unused after the link recovers.
        let verdicts = Verdicts::new();
        verdicts.put(
            "github.com",
            address(1),
            Entry::Doubtful {
                why: Doubt::Unreachable,
                // Backdated past the short TTL but well inside the long one.
                at: Instant::now() - UNREACHABLE_TTL - Duration::from_secs(1),
            },
        );
        verdicts.put(
            "github.com",
            address(2),
            Entry::Doubtful {
                why: Doubt::WrongCertificate,
                at: Instant::now() - UNREACHABLE_TTL - Duration::from_secs(1),
            },
        );

        assert_eq!(
            verdicts.get("github.com", address(1)),
            None,
            "a transient failure should not outlive its TTL"
        );
        assert_eq!(
            verdicts.get("github.com", address(2)),
            Some(false),
            "a wrong certificate is worth remembering for much longer"
        );
    }

    #[test]
    fn a_snapshot_shows_only_what_is_still_knowledge() {
        // The report the shell draws must agree with the dial path. A conclusion
        // `get` has already stopped using — because its TTL passed — must not
        // still be shown as a verdict, or the screen would say an address serves
        // the wrong certificate at the very moment the kernel went back to trying
        // it. The same holds for an abandoned probe: it is a promise nobody kept,
        // not a fact.
        let verdicts = Verdicts::new();
        let fresh_doubt = address(1);
        let stale_doubt = address(2);
        let covers = address(3);
        let running = address(4);
        let abandoned = address(5);

        verdicts.put_for_test(
            "github.com",
            fresh_doubt,
            Entry::Doubtful {
                why: Doubt::WrongCertificate,
                at: Instant::now(),
            },
        );
        verdicts.put_for_test(
            "github.com",
            stale_doubt,
            Entry::Doubtful {
                why: Doubt::WrongCertificate,
                // Older than the long TTL, so the dial path has forgotten it.
                at: Instant::now() - TTL - Duration::from_secs(1),
            },
        );
        verdicts.put_for_test(
            "github.com",
            covers,
            Entry::Covers {
                at: Instant::now(),
            },
        );
        verdicts.put_for_test(
            "github.com",
            running,
            Entry::InFlight {
                started: Instant::now(),
            },
        );
        verdicts.put_for_test(
            "github.com",
            abandoned,
            Entry::InFlight {
                started: Instant::now() - IN_FLIGHT_TTL - Duration::from_secs(1),
            },
        );

        let rows = verdicts.snapshot();
        let state_of = |ip: IpAddr| {
            rows.iter()
                .find(|(_, candidate, _, _)| *candidate == ip)
                .map(|(_, _, state, _)| *state)
        };

        assert_eq!(
            state_of(fresh_doubt),
            Some("wrong_certificate"),
            "a fresh doubt is knowledge and must be reported"
        );
        assert_eq!(
            state_of(stale_doubt),
            None,
            "an expired doubt must not be shown once the dial path has moved on"
        );
        assert_eq!(
            state_of(covers),
            Some("covers"),
            "a fresh confirmation is knowledge"
        );
        assert_eq!(
            state_of(running),
            Some("in_flight"),
            "a live probe is worth showing, as long as it is live"
        );
        assert_eq!(
            state_of(abandoned),
            None,
            "a probe past its in-flight budget is not a verdict"
        );

        // The age is the entry's own age, taken from its timestamp rather than
        // from a fresh clock at report time.
        let (_, _, _, age) = rows
            .iter()
            .find(|(_, candidate, _, _)| *candidate == covers)
            .expect("the covers entry is present");
        assert!(*age < Duration::from_secs(5), "age should be tiny, was {age:?}");
    }
}
