//! Apply the user's switches to a rule document.
//!
//! The rules screen offers three levels — a group, a domain, an address — and all
//! three have to reach the kernel as a *smaller document*. There is no runtime
//! flag for this on purpose: the kernel's job is to relay what it is given, and
//! deciding what to give it is the shell's job. Filtering the document before
//! compilation is also the only way the change is visible in the same place the
//! user sees it.
//!
//! # Why not filter in the shell
//!
//! The document is around 4.6 MB and its shape is the kernel's own model. A
//! second implementation of "which domains does this entry claim" would drift
//! from the first, and the failure mode is silent: a domain quietly stops being
//! routed, or quietly keeps being routed after the user switched it off.
//!
//! # The keys
//!
//! A policy is a set of strings, each prefixed so one flat set can carry all
//! three levels:
//!
//! * `g:<group>` — switch off a whole group
//! * `d:<domain>` — switch off one domain everywhere it appears
//! * `a:<address>` — switch off one address
//!
//! The prefixes match what the shell stores, so there is one definition of what a
//! key looks like rather than two that have to be kept in step.

use std::collections::HashSet;

use crate::model::{Entry, Group, RuleDocument};

/// The prefix that marks a group key. See the module docs.
pub const GROUP_PREFIX: &str = "g:";
/// The prefix that marks a domain key.
pub const DOMAIN_PREFIX: &str = "d:";
/// The prefix that marks an address key.
pub const ADDRESS_PREFIX: &str = "a:";

/// What the user has switched off.
#[derive(Debug, Clone, Default)]
pub struct FilterPolicy {
    groups: HashSet<String>,
    domains: HashSet<String>,
    addresses: HashSet<String>,
}

/// What a filter removed, for the log and for the screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FilterStats {
    pub groups_removed: usize,
    pub domains_removed: usize,
    pub addresses_removed: usize,
}

impl FilterPolicy {
    /// Build from the flat key set the shell stores.
    pub fn from_keys<I, S>(keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut policy = Self::default();
        for key in keys {
            policy.insert(key.as_ref());
        }
        policy
    }

    /// Add one key. An unknown prefix is ignored rather than rejected: a stale
    /// key left behind by an older build should not stop the tunnel starting.
    pub fn insert(&mut self, key: &str) {
        if let Some(name) = key.strip_prefix(GROUP_PREFIX) {
            self.groups.insert(name.to_string());
        } else if let Some(name) = key.strip_prefix(DOMAIN_PREFIX) {
            self.domains.insert(name.to_string());
        } else if let Some(address) = key.strip_prefix(ADDRESS_PREFIX) {
            self.addresses.insert(address.to_string());
        }
    }

    /// Whether anything is switched off at all.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.domains.is_empty() && self.addresses.is_empty()
    }

    /// How many keys are held, for diagnostics.
    pub fn len(&self) -> usize {
        self.groups.len() + self.domains.len() + self.addresses.len()
    }
}

/// Remove everything the policy switches off.
///
/// A group that ends up with no entries is dropped entirely, so the rules screen
/// shows "switched off" rather than "present but empty" — those look the same to
/// a reader and mean different things.
///
/// **`filtered` is left alone.** Those are groups the upstream maintainer removed
/// from the mobile profile; they are already not in `groups`, and rewriting
/// someone else's bookkeeping to match ours would be a lie about what upstream
/// said.
pub fn filter_document(document: &RuleDocument, policy: &FilterPolicy) -> (RuleDocument, FilterStats) {
    if policy.is_empty() {
        return (document.clone(), FilterStats::default());
    }

    let mut stats = FilterStats::default();
    let mut result = document.clone();
    result.groups.clear();

    for group in &document.groups {
        if policy.groups.contains(&group.group) {
            stats.groups_removed += 1;
            continue;
        }

        let mut kept = Group {
            entries: Vec::with_capacity(group.entries.len()),
            ..group.clone()
        };
        for entry in &group.entries {
            if let Some(filtered) = filter_entry(entry, policy, &mut stats) {
                kept.entries.push(filtered);
            }
        }

        if kept.entries.is_empty() {
            continue;
        }
        result.groups.push(kept);
    }

    (result, stats)
}

/// One entry, minus whatever the policy removes. `None` when nothing is left.
///
/// An entry with no domains is dropped even if its addresses survive: the router
/// is keyed by domain, so an entry nothing can reach is not a rule.
fn filter_entry(entry: &Entry, policy: &FilterPolicy, stats: &mut FilterStats) -> Option<Entry> {
    let domains: Vec<String> = entry
        .domains
        .iter()
        .filter(|domain| !policy.domains.contains(domain.as_str()))
        .cloned()
        .collect();
    stats.domains_removed += entry.domains.len() - domains.len();

    if domains.is_empty() {
        return None;
    }

    let ips: Vec<String> = entry
        .ips
        .iter()
        .filter(|address| !policy.addresses.contains(address.as_str()))
        .cloned()
        .collect();
    stats.addresses_removed += entry.ips.len() - ips.len();

    // An entry whose every address was switched off is kept, not dropped. The
    // domains are still the user's to route — the proxy falls back to the system
    // resolver when the rule has no usable address, and dropping the entry would
    // turn "this one address is bad" into "this domain is broken".
    Some(Entry {
        ips,
        domains,
        ..entry.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Entry, Group, RuleDocument};

    fn entry(id: &str, ips: &[&str], domains: &[&str]) -> Entry {
        Entry {
            id: id.to_string(),
            ips: ips.iter().map(|s| s.to_string()).collect(),
            domains: domains.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn document() -> RuleDocument {
        RuleDocument {
            groups: vec![
                Group {
                    group: "developer".to_string(),
                    entries: vec![
                        entry("a", &["203.0.113.1", "203.0.113.2"], &["github.com"]),
                        entry("b", &["203.0.113.3"], &["crates.io", "docs.rs"]),
                    ],
                    ..Default::default()
                },
                Group {
                    group: "In Game".to_string(),
                    entries: vec![entry("c", &["203.0.113.4"], &["game.example"])],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn an_empty_policy_changes_nothing() {
        let original = document();
        let (filtered, stats) = filter_document(&original, &FilterPolicy::default());
        assert_eq!(stats, FilterStats::default());
        assert_eq!(filtered.groups.len(), 2);
        assert_eq!(filtered.groups[0].entries.len(), 2);
    }

    #[test]
    fn a_disabled_group_disappears_entirely() {
        // Not "present but empty": an empty group and a switched-off group look
        // identical on screen and mean different things.
        let policy = FilterPolicy::from_keys(["g:In Game"]);
        let (filtered, stats) = filter_document(&document(), &policy);
        assert_eq!(stats.groups_removed, 1);
        assert_eq!(filtered.groups.len(), 1);
        assert_eq!(filtered.groups[0].group, "developer");
    }

    #[test]
    fn a_disabled_domain_is_removed_but_its_siblings_are_not() {
        let policy = FilterPolicy::from_keys(["d:crates.io"]);
        let (filtered, stats) = filter_document(&document(), &policy);
        assert_eq!(stats.domains_removed, 1);
        let domains: Vec<&str> = filtered.groups[0].entries[1]
            .domains
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(domains, vec!["docs.rs"]);
    }

    #[test]
    fn a_domain_switched_off_in_one_place_is_off_everywhere() {
        // The key names a domain, not an entry. A domain can appear in more than
        // one entry after a merge, and switching it off in one place while it
        // stays live in another is exactly the bug this prevents.
        let mut doc = document();
        doc.groups[1].entries[0].domains.push("github.com".to_string());
        let policy = FilterPolicy::from_keys(["d:github.com"]);
        let (filtered, _) = filter_document(&doc, &policy);
        for group in &filtered.groups {
            for entry in &group.entries {
                assert!(
                    !entry.domains.iter().any(|d| d == "github.com"),
                    "github.com survived in {}",
                    group.group,
                );
            }
        }
    }

    #[test]
    fn an_entry_whose_domains_all_go_is_dropped() {
        let policy = FilterPolicy::from_keys(["d:crates.io", "d:docs.rs"]);
        let (filtered, _) = filter_document(&document(), &policy);
        assert_eq!(filtered.groups[0].entries.len(), 1);
        assert_eq!(filtered.groups[0].entries[0].id, "a");
    }

    #[test]
    fn an_entry_whose_addresses_all_go_is_kept() {
        // The domains are still the user's to route. Dropping the entry would
        // turn "this one address is bad" into "this domain is broken", and the
        // proxy's fallback to the system resolver is the right answer instead.
        let policy = FilterPolicy::from_keys(["a:203.0.113.1", "a:203.0.113.2"]);
        let (filtered, stats) = filter_document(&document(), &policy);
        assert_eq!(stats.addresses_removed, 2);
        assert_eq!(filtered.groups[0].entries.len(), 2);
        assert!(filtered.groups[0].entries[0].ips.is_empty());
        assert_eq!(filtered.groups[0].entries[0].domains, vec!["github.com"]);
    }

    #[test]
    fn an_unknown_prefix_is_ignored_rather_than_rejected() {
        // A key left behind by an older build should not stop the tunnel. The
        // unknown key is dropped; the known one beside it still applies.
        let policy = FilterPolicy::from_keys(["x:nonsense", "d:github.com"]);
        assert_eq!(policy.len(), 1, "only the recognised key was stored");

        let (filtered, stats) = filter_document(&document(), &policy);
        // Both groups survive: the unknown key removed nothing.
        assert_eq!(filtered.groups.len(), 2);
        assert_eq!(stats.groups_removed, 0);
        // `github.com` was entry "a"'s only domain, so the entry is gone and the
        // remaining one in that group is "b".
        assert_eq!(filtered.groups[0].entries.len(), 1);
        assert_eq!(filtered.groups[0].entries[0].id, "b");
    }

    #[test]
    fn the_filtered_list_is_left_alone() {
        // Those groups were removed by the upstream maintainer, not by us.
        // Rewriting their bookkeeping to match ours would misreport upstream.
        let mut doc = document();
        doc.filtered.push(crate::model::FilteredGroup {
            group: "In Game".to_string(),
            ..Default::default()
        });
        let policy = FilterPolicy::from_keys(["g:In Game"]);
        let (filtered, _) = filter_document(&doc, &policy);
        assert_eq!(filtered.filtered.len(), 1);
        assert_eq!(filtered.groups.len(), 1);
    }
}
