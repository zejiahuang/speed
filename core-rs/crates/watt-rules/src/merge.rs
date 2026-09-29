//! Merge rule documents from more than one source.
//!
//! The upstream used to publish two documents at `/hosts?all=1` and `/rules`;
//! **both endpoints have been retired** (they now return an nginx 404) and were
//! replaced by two *different rule sets* at the short paths `/1` and `/2`.
//! Measured against the live endpoints:
//!
//! | | domains | addresses | domains with >1 address |
//! |---|---:|---:|---:|
//! | UsbEAm host records (`/1`) | 5225 | 15952 | **5225** |
//! | S302 hijack block (`/2`) | 862 | 862 | **0** |
//!
//! ## Merging is not automatically the goal here
//!
//! This function unions addresses per domain, and for two *address-bearing*
//! documents that is strictly better than either alone. `/2` is not such a
//! document. Every one of its 862 addresses is `127.0.0.1`, and that address has
//! the *opposite* meaning in the two worlds it appears in: in the upstream
//! aggregator that produced it, it points at a local reverse proxy (a hijack),
//! but in this kernel a rule may not relay loopback at all.
//! `Planner::can_relay` (`watt-stack`, planner.rs) returns false for a
//! non-overridden loopback target, and `tcp.rs` answers such a flow with
//! `socket.abort()` and a RST (`tcp_flows_rejected`). Note it is `can_relay`
//! that inspects the *target*; `is_blocked_target` is called only over
//! `decision.alternatives` (the tail of the candidate list), never the target.
//!
//! So merging `/2` in does not *add* coverage — it *breaks* it. Measured, the
//! merged set is 5905 domains: 5230 hold a real address, and **675 are
//! loopback-only, every one of them `/2`-only**. Each of those 675 would
//! otherwise be unmatched and go direct and work; after the merge it compiles to
//! a single `127.0.0.1` and is refused. That is why neither the daemon nor the
//! app merges `/2` by default (see `DEFAULT_HOSTS_URLS` in `watt-daemon`; it is
//! `/1` alone). An operator who genuinely runs a loopback reverse proxy opts in
//! explicitly.
//!
//! This is not a reason to weaken the merge: it is a reason to be careful which
//! documents are handed to it. `watt_rules` faithfully unions whatever it is
//! given, and the 5225 multi-address domains of `/1` are exactly the address
//! density the selector needs.
//!
//! ## Density is no longer the limit; correctness is
//!
//! The numbers above were re-measured 2026-09-26 and replaced an earlier record
//! of 2879 domains / ~2900 addresses / 1.03 per domain / 79 multi-address
//! domains. That earlier record is what justified "one stale address kills a
//! domain": with a single candidate the selector had nothing to rank. It no
//! longer holds — **every** domain in `/1` lists more than one address, so the
//! selector has real ranking room.
//!
//! What replaces it is a limit ranking cannot remove. Sampling 25 domains from
//! `/1` and testing each rule address for both TCP reachability and TLS
//! certificate validity (`server_hostname` = the domain) gave **18/25 = 72%
//! usable** on one run and 17/25 = 68% on an independent re-sample — so treat
//! "roughly 7 in 10 usable" as the stable quantity, not the exact figure. The
//! failures must not be conflated:
//!
//!   * **certificate mismatch** — the address connects but its chain does not
//!     cover the domain. Architecturally unsolvable without MITM, which this
//!     kernel refuses; the only fix is a *different* address.
//!   * **refused** — connection refused / reset / EOF. Ranking can move past it.
//!   * **timeout** — no response. Ranking can move past it, after cooldown.
//!
//! So merging buys more candidates to rank, but the ceiling on the rescue rate
//! is set by how many of those candidates are *correct*, not how many there are.
//! Re-measure by fetching `/1` and repeating the reachability + certificate test
//! over a fresh sample; the 72% figure will drift, the method will not.
//!
//! A default `watt-daemon` run also folds in the built-in set as the base
//! document (there is no JSON endpoint to fill that slot — see
//! `DEFAULT_RULES_URL` in `watt-daemon`), so it reports a higher domain count
//! than `/1` alone. The extras are builtin-only entries, not live-source data.

use std::collections::{HashMap, HashSet};

use crate::model::{Entry, Group, Meta, RuleDocument};

/// Merge documents into one, unioning the addresses of each domain.
///
/// Merging happens per *domain*, not per entry, because the two sources disagree
/// about how to group: a hosts line is one domain, while a JSON entry can claim
/// dozens. Keying on the entry would leave the same domain in two entries and the
/// compiler's "first writer wins" rule would then throw one of them away,
/// discarding exactly the addresses this function exists to keep.
///
/// Metadata comes from whichever source supplies it: a port or certificate list
/// survives even when the addresses came from the other document. A domain stays
/// a placeholder only if *every* source called it one, because a single real
/// address makes the entry usable no matter what the other source said.
pub fn merge_documents(docs: &[RuleDocument]) -> RuleDocument {
    // Hash maps, not ordered ones: the keys are domain and group names, so an
    // ordered map pays a string comparison per level of every lookup, and this
    // runs over every domain of every source. Output order is fixed at the end.
    let mut by_domain: HashMap<String, Entry> = HashMap::new();
    // Domains bucketed by their group as they are seen, so the groups can be
    // emitted in one pass. Rebuilding them by scanning every domain once per
    // group was quadratic — 22 groups over 5900 domains, each scan a string-keyed
    // lookup — and showed up as seconds of startup.
    let mut group_entries: HashMap<String, Vec<String>> = HashMap::new();
    let mut groups_seen: Vec<(String, Group)> = Vec::new();

    for doc in docs {
        for group in &doc.groups {
            for entry in &group.entries {
                for domain in &entry.domains {
                    match by_domain.get_mut(domain) {
                        Some(existing) => absorb(existing, entry),
                        None => {
                            let mut fresh = entry.clone();
                            fresh.domains = vec![domain.clone()];
                            by_domain.insert(domain.clone(), fresh);
                            group_entries
                                .entry(group.group.clone())
                                .or_insert_with(|| {
                                    groups_seen.push((group.group.clone(), Group {
                                        group: group.group.clone(),
                                        group_zh: group.group_zh.clone(),
                                        category: group.category.clone(),
                                        icon_slug: group.icon_slug.clone(),
                                        entries: Vec::new(),
                                        icon_url: group.icon_url.clone(),
                                    }));
                                    Vec::new()
                                })
                                .push(domain.clone());
                        }
                    }
                }
            }
        }
    }

    let mut groups: Vec<Group> = Vec::new();
    for (name, mut group) in groups_seen {
        let Some(domains) = group_entries.get(&name) else {
            continue;
        };
        group.entries = domains
            .iter()
            .filter_map(|domain| by_domain.remove(domain))
            .collect();
        if group.entries.is_empty() {
            continue;
        }
        groups.push(group);
    }

    // The version is deliberately not merged: two documents have two version
    // numbers and picking one would misreport what is loaded. The caller reports
    // each source separately.
    RuleDocument {
        meta: Meta::default(),
        groups,
        filtered: Vec::new(),
    }
}

/// Fold one entry's contribution into the entry already held for this domain.
fn absorb(existing: &mut Entry, incoming: &Entry) {
    // A set rather than `Vec::contains`: an entry can list nearly a thousand
    // addresses, and this runs once per source that claims the domain. Scanning
    // the growing list each time is quadratic on exactly the entries that carry
    // the most addresses — the ones a merge is most valuable for.
    let mut present: HashSet<String> = existing.ips.iter().cloned().collect();
    for ip in &incoming.ips {
        if present.insert(ip.clone()) {
            existing.ips.push(ip.clone());
        }
    }
    if existing.port.is_none() {
        existing.port = incoming.port;
    }
    if existing.cert.is_none() {
        existing.cert = incoming.cert.clone();
    }
    if existing.name_zh.is_none() {
        existing.name_zh = incoming.name_zh.clone();
    }
    if existing.icon_slug.is_none() {
        existing.icon_slug = incoming.icon_slug.clone();
    }
    // Only a placeholder if every source said so. One real address settles it.
    existing.is_placeholder = existing.is_placeholder && incoming.is_placeholder;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Group;

    fn doc(version: &str, entries: Vec<Entry>) -> RuleDocument {
        RuleDocument {
            meta: Meta {
                version: version.to_string(),
                update_time: None,
            },
            groups: vec![Group {
                group: "g".to_string(),
                group_zh: None,
                category: None,
                icon_slug: None,
                entries,
                icon_url: None,
            }],
            filtered: Vec::new(),
        }
    }

    fn entry(domain: &str, ips: &[&str]) -> Entry {
        Entry {
            id: domain.to_string(),
            name: domain.to_string(),
            name_zh: None,
            ips: ips.iter().map(|ip| ip.to_string()).collect(),
            domains: vec![domain.to_string()],
            port: None,
            cert: None,
            is_placeholder: false,
            icon_slug: None,
            ip_country: None,
            ip_country_name: None,
            icon_url: None,
        }
    }

    fn addresses(doc: &RuleDocument, domain: &str) -> Vec<String> {
        doc.groups
            .iter()
            .flat_map(|group| group.entries.iter())
            .find(|entry| entry.domains == vec![domain.to_string()])
            .map(|entry| entry.ips.clone())
            .unwrap_or_default()
    }

    #[test]
    fn the_same_domain_keeps_the_addresses_of_both_sources() {
        // The whole point: hosts gives one dead address, the JSON document gives
        // a working one, and the merged rule must be able to dial both.
        let merged = merge_documents(&[
            doc("h", vec![entry("github.com", &["51.142.105.107"])]),
            doc("j", vec![entry("github.com", &["20.200.245.247"])]),
        ]);
        assert_eq!(
            addresses(&merged, "github.com"),
            vec!["51.142.105.107", "20.200.245.247"]
        );
    }

    #[test]
    fn a_domain_only_one_source_has_survives() {
        let merged = merge_documents(&[
            doc("h", vec![entry("steam.example", &["203.0.113.1"])]),
            doc("j", vec![entry("github.com", &["203.0.113.2"])]),
        ]);
        assert_eq!(addresses(&merged, "steam.example").len(), 1);
        assert_eq!(addresses(&merged, "github.com").len(), 1);
    }

    #[test]
    fn an_address_listed_twice_is_stored_once() {
        let merged = merge_documents(&[
            doc("h", vec![entry("a.example", &["203.0.113.1"])]),
            doc("j", vec![entry("a.example", &["203.0.113.1", "203.0.113.2"])]),
        ]);
        assert_eq!(
            addresses(&merged, "a.example"),
            vec!["203.0.113.1", "203.0.113.2"]
        );
    }

    #[test]
    fn metadata_comes_from_whichever_source_has_it() {
        let mut json_entry = entry("a.example", &["203.0.113.1"]);
        json_entry.port = Some(8443);
        json_entry.cert = Some("a.example".to_string());
        let merged = merge_documents(&[
            doc("h", vec![entry("a.example", &["203.0.113.2"])]),
            doc("j", vec![json_entry]),
        ]);
        let found = merged
            .groups
            .iter()
            .flat_map(|group| group.entries.iter())
            .find(|entry| entry.domains == vec!["a.example".to_string()])
            .expect("merged entry exists");
        assert_eq!(found.port, Some(8443));
        assert_eq!(found.cert.as_deref(), Some("a.example"));
    }

    #[test]
    fn a_real_address_cancels_a_placeholder() {
        // One source says "this is a {Cloudflare} placeholder", the other supplies
        // a concrete address. The domain is usable, so it must not stay marked as
        // a placeholder.
        let mut placeholder = entry("discord.com", &["{Cloudflare}"]);
        placeholder.is_placeholder = true;
        let merged = merge_documents(&[
            doc("j", vec![placeholder]),
            doc("h", vec![entry("discord.com", &["140.245.84.81"])]),
        ]);
        let found = merged
            .groups
            .iter()
            .flat_map(|group| group.entries.iter())
            .find(|entry| entry.domains == vec!["discord.com".to_string()])
            .expect("merged entry exists");
        assert!(!found.is_placeholder);
        assert_eq!(found.ips.len(), 2);
    }

    #[test]
    fn merging_nothing_yields_nothing() {
        let merged = merge_documents(&[]);
        assert!(merged.groups.is_empty());
    }
}
