//! Tests against the real upstream rule document.
//!
//! The fixture lives outside the crate (in the repository's gitignored `tmp/`
//! directory) and is not committed, so the test is skipped when it is absent.
//! Populate it by saving an upstream `groups`-shape rule document to that path.
//!
//! These tests exist because the upstream document is third-party data whose
//! shape has changed before: `id` and `port` used to be numbers, `cert` is a
//! comma separated string rather than an array, and `ips` mixes real addresses
//! with `{Cloudflare}` style placeholders. A unit test with a hand written
//! fixture would not catch a future drift; this one will.

use std::path::PathBuf;
use std::time::Instant;

use watt_rules::{Family, Placeholder, RuleSet, RuleSource, Router, Strategy};

fn fixture_path() -> PathBuf {
    // crate dir: <repo>/core-rs/crates/watt-rules
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tmp/rules.json")
}

fn load() -> Option<RuleSet> {
    let path = fixture_path();
    let bytes = std::fs::read(&path).ok()?;
    Some(
        RuleSet::from_slice(&bytes, RuleSource::Provided)
            .unwrap_or_else(|err| panic!("upstream fixture at {} failed to parse: {err}", path.display())),
    )
}

macro_rules! fixture_or_skip {
    () => {
        match load() {
            Some(rules) => rules,
            None => {
                eprintln!(
                    "skipping: upstream fixture {} not present; save the upstream \
                     rule document there to run it",
                    fixture_path().display()
                );
                return;
            }
        }
    };
}

#[test]
fn upstream_document_compiles_into_a_usable_index() {
    let rules = fixture_or_skip!();
    let stats = rules.stats();

    assert!(stats.groups > 0, "no groups parsed");
    assert!(stats.entries > 50, "suspiciously few entries: {}", stats.entries);
    assert!(stats.domains > 50, "suspiciously few domains: {}", stats.domains);
    assert!(stats.concrete_ips > 50, "suspiciously few addresses: {}", stats.concrete_ips);
    assert!(
        stats.placeholder_entries > 0,
        "expected some placeholder only entries"
    );

    // Every compiled entry must be reachable through the domain index.
    for entry in rules.entries() {
        assert!(!entry.domains.is_empty(), "entry {} has no domains", entry.id);
        for domain in &entry.domains {
            let found = rules.lookup(domain).expect("indexed domain must resolve");
            assert_eq!(found.entry.key, entry.key, "domain {domain} resolved to the wrong entry");
        }
    }
}

#[test]
fn concrete_and_placeholder_entries_are_separated() {
    let rules = fixture_or_skip!();

    // jQuery ships a long list of real Fastly addresses.
    let jquery = rules
        .lookup("code.jquery.com")
        .expect("code.jquery.com must be present")
        .entry
        .clone();
    assert!(jquery.has_concrete_ips(), "jQuery entry should carry addresses");
    assert!(jquery.placeholders.is_empty(), "jQuery entry should not be a placeholder");
    assert_eq!(jquery.port, Some(443));
    assert!(jquery.ips.iter().all(|addr| !addr.is_unspecified()));

    // Cdnjs only carries a Cloudflare placeholder.
    let cdnjs = rules
        .lookup("cdnjs.cloudflare.com")
        .expect("cdnjs.cloudflare.com must be present")
        .entry
        .clone();
    assert!(!cdnjs.has_concrete_ips(), "cdnjs should not carry addresses");
    assert_eq!(cdnjs.placeholders, vec![Placeholder::Cloudflare]);
}

#[test]
fn routing_a_placeholder_domain_falls_back_to_upstream_resolution() {
    let rules = fixture_or_skip!();
    let router = Router::new(rules);
    let plan = router.plan(Instant::now(), "cdnjs.cloudflare.com", Family::V4);
    assert_eq!(plan.strategy, Strategy::PlaceholderFallback);
    assert!(plan.needs_upstream_resolution());
    assert!(plan.entry_key.is_some(), "the domain is still rule owned");
}

#[test]
fn routing_a_concrete_domain_returns_ranked_addresses() {
    let rules = fixture_or_skip!();
    let router = Router::new(rules);
    let plan = router.plan(Instant::now(), "code.jquery.com", Family::V4);

    assert_eq!(plan.strategy, Strategy::RuleAddresses);
    assert!(!plan.addresses.is_empty());
    assert!(
        plan.addresses.iter().all(|addr| matches!(addr, std::net::IpAddr::V4(_))),
        "an IPv4 client must not receive IPv6 candidates"
    );
    assert!(plan.addresses.iter().all(|addr| router.rules().owns_ip(*addr)));
}

#[test]
fn unmatched_domains_are_left_alone() {
    let rules = fixture_or_skip!();
    let router = Router::new(rules);
    for domain in ["example.com", "localhost.localdomain", "wikipedia.org"] {
        let plan = router.plan(Instant::now(), domain, Family::V4);
        assert_eq!(plan.strategy, Strategy::Direct, "{domain} should not be routed");
    }
}

#[test]
fn lookups_are_fast_enough_for_the_data_plane() {
    let rules = fixture_or_skip!();
    let probes = [
        "raw.githubusercontent.com",
        "cdn.jsdelivr.net",
        "api.github.com",
        "not-a-rule.example",
        "code.jquery.com",
    ];

    let start = Instant::now();
    let rounds = 20_000;
    for _ in 0..rounds {
        for probe in probes {
            std::hint::black_box(rules.lookup(probe));
        }
    }
    let elapsed = start.elapsed();
    let per_lookup = elapsed / (rounds * probes.len() as u32);

    // Generous bound: this is a smoke test for accidental O(n) scans, not a
    // benchmark. A linear scan over ~8000 entries would be orders of magnitude
    // slower than this.
    assert!(
        per_lookup.as_micros() < 20,
        "lookup took {per_lookup:?} on average, expected microseconds"
    );
}
