//! Hosts file support: the upstream's address-bearing source.
//!
//! **Both** live rule sets are fetched in this form. `/1` (UsbEAm) and `/2`
//! (Steamcommunity 302) each serve hosts text by default, and each can also emit
//! `?format=json` — but that JSON is a *different schema* the kernel cannot parse
//! (`{"entries":[{"ip":..,"domain":..}]}` vs `RuleDocument`'s
//! `{"groups":[{"entries":[{"domains":[..],"ips":[..]}]}]}`), and every field on
//! `RuleDocument` is `#[serde(default)]`, so the mismatch is accepted as an `Ok`
//! document with zero entries rather than an error. Hosts text has no such
//! failure mode.
//!
//! Measured 2026-09-26: `/1` yields 15971 lines / 15952 addresses / 5225 domains
//! (19 skipped); `/2` yields 862 lines / 862 domains, addresses all `127.0.0.1`.
//! The two sets are not subsets of each other, so a client that wants coverage
//! merges them — see `watt_rules::merge_documents`. This parser is what the same
//! data the desktop tool consumes is read with.
//!
//! The syntax is an ordinary hosts file plus two upstream conventions:
//! `# === [group name] ===` opens a section, so the grouping survives into the
//! compiled rule set instead of collapsing into a single bucket; and a first
//! field written `!<address>` means "dial this address and present no SNI",
//! which is how a source with an address but no name for it expresses that —
//! see `CompiledEntry::no_sni` for why that is a distinct claim.

use std::collections::BTreeMap;
use std::net::IpAddr;

use crate::domain::normalize_dial_name;
use crate::error::{Result, RuleError};
use crate::model::{Entry, Group, Meta, RuleDocument};

/// What a hosts file turned into, for the startup log.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HostsStats {
    /// Lines that were not blank and not a comment.
    pub lines: usize,
    /// `address domain` pairs accepted.
    pub addresses: usize,
    /// Of [`HostsStats::addresses`], how many were written `!<address>` and so
    /// are reached without SNI. A sub-count rather than a peer, because those
    /// lines *are* address lines — they are dialled like any other — and a total
    /// that excluded them would break the reading `addresses + dial_names +
    /// skipped == lines`.
    pub no_sni: usize,
    /// Lines whose first field named a host to resolve rather than an address.
    pub dial_names: usize,
    /// Distinct domains.
    pub domains: usize,
    /// Lines skipped because the address or the domain was unusable.
    pub skipped: usize,
    /// Sections opened by `# === [name] ===`.
    pub groups: usize,
}

/// Parse a hosts file into a rule document.
///
/// Entries keep the section they were declared under, and each domain becomes
/// one rule entry holding every address the file gives it. Unspecified
/// addresses (`0.0.0.0`, `::`) are dropped: in a hosts file those are blocklist
/// entries, and routing a domain to them would be the opposite of the intent.
pub fn parse(raw: &str) -> Result<(RuleDocument, HostsStats)> {
    let mut stats = HostsStats::default();
    let mut meta = Meta {
        version: String::new(),
        update_time: None,
    };

    // Section name -> domain -> targets. Ordered so that two runs over the same
    // file produce byte-identical rule sets.
    //
    // A target is held as text rather than as an `IpAddr` because the file may
    // name a host instead of an address. Both are legitimate things to dial; the
    // rule compiler downstream is what tells them apart.
    let mut sections: Vec<(String, BTreeMap<String, Vec<String>>)> =
        vec![(String::new(), BTreeMap::new())];

    for line in raw.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            let rest = rest.trim();
            if let Some(name) = rest.strip_prefix("=== [").and_then(|r| r.strip_suffix("] ===")) {
                let name = name.trim().to_string();
                if sections.iter().all(|(existing, _)| *existing != name) {
                    stats.groups += 1;
                    sections.push((name, BTreeMap::new()));
                }
            } else if let Some(version) = rest.strip_prefix("版本:") {
                meta.version = version.trim().to_string();
            } else if let Some(updated) = rest.strip_prefix("上游更新时间:") {
                meta.update_time = Some(updated.trim().to_string());
            }
            continue;
        }

        stats.lines += 1;
        let mut fields = line.split_whitespace();
        let (Some(address), Some(domain)) = (fields.next(), fields.next()) else {
            stats.skipped += 1;
            continue;
        };
        // The first field is normally an address. When it is not, the file is
        // naming a host to resolve instead — the same extension the JSON rule
        // format accepts, so both sources can express "dial this name".
        //
        // A leading `!` on an address is the third form: dial it and present no
        // SNI. It only means anything for an address — a name *is* what SNI
        // carries, so `!name` asks for a contradiction and is dropped rather than
        // reinterpreted as the plain name. The token survives into the document
        // unchanged; `RuleSet` is what tells the three forms apart, so the
        // classification lives in one place rather than in each parser.
        let (field, no_sni) = match address.strip_prefix('!') {
            Some(rest) => (rest, true),
            None => (address, false),
        };
        let target = match field.parse::<IpAddr>() {
            Ok(addr) => {
                if addr.is_unspecified() || addr.is_multicast() {
                    stats.skipped += 1;
                    continue;
                }
                stats.addresses += 1;
                if no_sni {
                    stats.no_sni += 1;
                    format!("!{addr}")
                } else {
                    addr.to_string()
                }
            }
            Err(_) => {
                if no_sni {
                    stats.skipped += 1;
                    continue;
                }
                let Some(name) = normalize_dial_name(field) else {
                    stats.skipped += 1;
                    continue;
                };
                stats.dial_names += 1;
                name
            }
        };
        let domain = domain.to_ascii_lowercase();
        if domain.is_empty() || domain == "localhost" || domain.ends_with(".local") {
            stats.skipped += 1;
            continue;
        }

        let bucket = sections.last_mut().expect("a section always exists");
        let targets = bucket.1.entry(domain).or_default();
        if !targets.contains(&target) {
            targets.push(target);
        }
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut counter = 0usize;
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (name, domains) in sections {
        if domains.is_empty() {
            continue;
        }
        let mut entries = Vec::new();
        for (domain, targets) in domains {
            // First writer wins across sections, matching how the JSON compiler
            // resolves a domain claimed twice: deterministic, and the earlier
            // section is the more specific one upstream.
            if seen.contains_key(&domain) {
                continue;
            }
            seen.insert(domain.clone(), counter);
            entries.push(Entry {
                id: format!("h{counter}"),
                name: domain.clone(),
                name_zh: None,
                ips: targets,
                domains: vec![domain],
                port: None,
                cert: None,
                is_placeholder: false,
                icon_slug: None,
                ip_country: None,
                ip_country_name: None,
                icon_url: None,
            });
            counter += 1;
        }
        if entries.is_empty() {
            continue;
        }
        groups.push(Group {
            group: if name.is_empty() {
                "hosts".to_string()
            } else {
                name
            },
            group_zh: None,
            category: Some("HOSTS".to_string()),
            icon_slug: None,
            entries,
            icon_url: None,
        });
    }

    stats.domains = seen.len();
    if meta.version.is_empty() {
        meta.version = "hosts".to_string();
    }

    if groups.is_empty() {
        return Err(RuleError::NoUsableEntries);
    }

    Ok((
        RuleDocument {
            meta,
            groups,
            filtered: Vec::new(),
        },
        stats,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Fast Proxy
# 版本: 1.0.48
# 上游更新时间: 2026/09/05 16:13

# === [developer] ===
151.101.62.137\tcode.jquery.com
146.75.2.132\tapache.org
146.75.2.132\twww.apache.org

# === [For Web] ===
140.245.84.81\tdiscordapp.com
140.245.84.81\tgateway.discord.gg

# 0.0.0.0\tblocked.example
";

    #[test]
    fn reads_sections_addresses_and_metadata() {
        let (doc, stats) = parse(SAMPLE).unwrap();
        assert_eq!(stats.groups, 2);
        assert_eq!(stats.domains, 5);
        assert_eq!(doc.meta.version, "1.0.48");
        assert_eq!(doc.meta.update_time.as_deref(), Some("2026/09/05 16:13"));
        assert_eq!(doc.groups.len(), 2);
        assert_eq!(doc.groups[0].group, "developer");
        assert_eq!(doc.groups[0].entries.len(), 3);
        assert_eq!(doc.groups[1].group, "For Web");
    }

    #[test]
    fn a_hostname_in_the_address_column_is_a_dial_name() {
        // The same extension the JSON format accepts: the first field may name a
        // host to resolve instead of an address to dial.
        let (doc, stats) = parse(
            "steamstore-a.akamaihd.net.edgesuite.net\tstore.steampowered.com\n1.1.1.1 plain.example\n",
        )
        .unwrap();
        assert_eq!(stats.dial_names, 1);
        assert_eq!(stats.addresses, 1);
        assert_eq!(stats.skipped, 0);
        // Entries are ordered by domain, so look the one up rather than assuming
        // it came first.
        let entry = doc.groups[0]
            .entries
            .iter()
            .find(|entry| entry.domains == vec!["store.steampowered.com"])
            .expect("the dialled domain is present");
        assert_eq!(
            entry.ips,
            vec!["steamstore-a.akamaihd.net.edgesuite.net".to_string()]
        );
    }

    #[test]
    fn a_wildcard_in_the_address_column_is_skipped() {
        // It cannot be resolved, so it is not a route and not a dial name. A
        // second, valid line keeps the document from being empty, which is a
        // separate error.
        let (doc, stats) = parse("*.akamaihd.net wild.example\n1.1.1.1 keep.example\n").unwrap();
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.dial_names, 0);
        assert_eq!(stats.domains, 1);
        assert_eq!(doc.groups[0].entries[0].domains, vec!["keep.example"]);
    }

    #[test]
    fn a_bang_address_is_carried_through_as_a_route_with_no_name() {
        // The token is not interpreted here — `RuleSet` owns that classification —
        // but it has to *survive* this parser. Without this branch it would be an
        // unusable first field and the line would be silently dropped, which is
        // the failure mode that would make the whole feature look like it did not
        // work rather than like it was never fed anything.
        let (doc, stats) = parse("!45.79.115.66\texample.com\n1.1.1.1 plain.example\n").unwrap();
        assert_eq!(stats.no_sni, 1);
        assert_eq!(stats.addresses, 2, "a flagged address is still an address");
        assert_eq!(stats.dial_names, 0);
        assert_eq!(stats.skipped, 0);
        let entry = doc.groups[0]
            .entries
            .iter()
            .find(|entry| entry.domains == vec!["example.com"])
            .expect("the flagged domain is present");
        assert_eq!(entry.ips, vec!["!45.79.115.66".to_string()]);
    }

    #[test]
    fn a_bang_on_something_that_is_not_an_address_is_skipped() {
        // `!name` contradicts itself, and `!0.0.0.0` is a blocklist entry that must
        // not become a route just because it was flagged. Both are skipped rather
        // than read as the unflagged form.
        let (doc, stats) = parse(
            "!edge.example.net a.example\n!0.0.0.0 b.example\n!1.2.3 c.example\n1.1.1.1 keep.example\n",
        )
        .unwrap();
        assert_eq!(stats.skipped, 3);
        assert_eq!(stats.no_sni, 0);
        assert_eq!(stats.dial_names, 0);
        assert_eq!(stats.domains, 1);
        assert_eq!(doc.groups[0].entries[0].domains, vec!["keep.example"]);
    }

    #[test]
    fn a_bang_address_compiles_into_a_route_that_presents_nothing() {
        // End to end through both layers, which is the pairing that matters: the
        // parser keeps the token and the compiler is what gives it meaning.
        let (doc, _) = parse("!45.79.115.66\texample.com\n").unwrap();
        let rules =
            crate::ruleset::RuleSet::from_document(doc, crate::ruleset::RuleSource::Provided)
                .unwrap();
        let entry = rules
            .lookup("example.com")
            .expect("a rule must match")
            .entry;
        assert_eq!(entry.ips.len(), 1);
        assert_eq!(entry.no_sni, entry.ips);
    }

    #[test]
    fn a_blocklist_address_is_not_a_route() {
        // `0.0.0.0 name` means "do not resolve this"; treating it as a
        // destination would send traffic into the void.
        let (doc, stats) = parse("1.1.1.1 keep.example\n0.0.0.0 blocked.example\n").unwrap();
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.domains, 1);
        assert_eq!(doc.groups[0].entries[0].domains, vec!["keep.example"]);
    }

    #[test]
    fn a_file_of_nothing_but_blocklist_lines_is_an_error() {
        // There is no rule set here, and saying so is better than starting with
        // one that silently routes nothing.
        assert!(parse("0.0.0.0 blocked.example\n").is_err());
    }

    #[test]
    fn one_domain_keeps_every_address() {
        let (doc, _) = parse("1.1.1.1 a.example\n2.2.2.2 a.example\n").unwrap();
        let entry = &doc.groups[0].entries[0];
        assert_eq!(entry.ips, vec!["1.1.1.1", "2.2.2.2"]);
    }

    #[test]
    fn a_domain_in_two_sections_is_claimed_once() {
        let raw = "# === [a] ===\n1.1.1.1 x.example\n# === [b] ===\n2.2.2.2 x.example\n";
        let (doc, stats) = parse(raw).unwrap();
        assert_eq!(stats.domains, 1);
        assert_eq!(doc.groups.len(), 1);
        assert_eq!(doc.groups[0].group, "a");
    }

    #[test]
    fn a_file_with_nothing_usable_is_an_error() {
        assert!(parse("# only a comment\n").is_err());
    }

    #[test]
    fn compiles_into_a_rule_set() {
        let (doc, _) = parse(SAMPLE).unwrap();
        let rules = crate::ruleset::RuleSet::from_document(doc, crate::ruleset::RuleSource::Provided)
            .unwrap();
        let found = rules.lookup("code.jquery.com").expect("a rule must match");
        assert_eq!(found.entry.ips.len(), 1);
        // Subdomains resolve through the same rule, which is what makes a hosts
        // file usable for anything that talks to a CDN hostname.
        assert!(rules.lookup("cdn.apache.org").is_some());
    }
}
