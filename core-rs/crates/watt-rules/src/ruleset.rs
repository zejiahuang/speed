//! The compiled rule set: indexes, matching and lookups.
//!
//! Compilation turns the loosely typed upstream document into dense indexes:
//!
//! * `domain_index` maps a normalized rule domain to one entry, so a lookup is
//!   at most a handful of hash probes (one per label of the query).
//! * `ip_index` maps a concrete address back to the entries that claim it, which
//!   the data plane uses to recognise traffic it already steered.
//!
//! Entries that carry only a CDN placeholder (`{Cloudflare}`) contribute to the
//! domain index but not to the IP index, because the placeholder is not an
//! address.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use crate::domain::{normalize_dial_name, normalize_domain, normalize_rule_domain, suffixes};
use crate::error::{Result, RuleError};
use crate::model::{FilteredGroup, Meta, RuleDocument};

/// A CDN placeholder such as `{Cloudflare}` found in an entry's `ips` array.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Placeholder {
    Cloudflare,
    Cloudfront,
    /// Any other `{Name}` token, kept verbatim so new upstream tokens survive.
    Other(String),
}

impl Placeholder {
    /// Parse a `{Name}` token. Returns `None` when the value is a real address.
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        let inner = trimmed.strip_prefix('{')?.strip_suffix('}')?.trim();
        if inner.is_empty() {
            return None;
        }
        Some(match inner.to_ascii_lowercase().as_str() {
            "cloudflare" => Placeholder::Cloudflare,
            "cloudfront" => Placeholder::Cloudfront,
            _ => Placeholder::Other(inner.to_string()),
        })
    }

    /// Render the placeholder back into its upstream token form.
    pub fn token(&self) -> String {
        match self {
            Placeholder::Cloudflare => "{Cloudflare}".to_string(),
            Placeholder::Cloudfront => "{Cloudfront}".to_string(),
            Placeholder::Other(name) => format!("{{{name}}}"),
        }
    }
}

/// Where a rule set came from, surfaced in diagnostics and the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSource {
    /// Small curated set compiled into the binary.
    Builtin,
    /// Loaded from the on-disk cache written by a previous update.
    Cache,
    /// Parsed directly from a payload supplied by the caller.
    Provided,
}

impl RuleSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleSource::Builtin => "builtin",
            RuleSource::Cache => "cache",
            RuleSource::Provided => "provided",
        }
    }
}

/// A rule entry with all fields normalized and parsed.
#[derive(Debug, Clone)]
pub struct CompiledEntry {
    /// Dense index inside [`RuleSet::entries`].
    pub key: u32,
    /// Upstream identifier, kept as a string because upstream sends strings.
    pub id: String,
    pub name: String,
    pub group: String,
    /// Normalized domains owned by this entry.
    pub domains: Vec<String>,
    /// Concrete addresses parsed from `ips`; excludes placeholders.
    pub ips: Vec<IpAddr>,
    /// Names to resolve before connecting, parsed from `ips`.
    ///
    /// A rule author sometimes knows a *better name* rather than a better
    /// address: `steamstore-a.akamaihd.net.edgesuite.net` is the same Akamai
    /// service reachable through a different routing label, and it resolves to a
    /// different edge than the bare name does. Writing the name down instead of
    /// the address keeps that choice alive — a literal address goes stale, a
    /// name gets re-resolved.
    ///
    /// This is only usable when the certificate the name serves covers the
    /// domain the client asked for, because nothing here terminates TLS. That
    /// constraint is the rule author's to satisfy, and the selector will learn
    /// about it the hard way if they get it wrong.
    pub dial_names: Vec<String>,
    /// Addresses from `ips` written as `!<address>`, meaning "dial this address
    /// and present *no* SNI".
    ///
    /// A third answer to "which name is the origin asked for", alongside the
    /// client's name and a rule-supplied name. The upstream's own Caddyfile
    /// reaches some of its blocks over a bare `reverse_proxy <address>` with no
    /// `tls_server_name`, which is Caddy's way of saying the upstream has no
    /// name — and a bare address is the only thing left to dial.
    ///
    /// Measured on the device over 13 such addresses: presenting the client's
    /// name in SNI is refused by 11 of them (RST 0.06–0.4 s in, i.e. after the
    /// TCP connect, so the address itself is live), while reaching the same
    /// addresses with no SNI completes the handshake on 10. An unrelated name in
    /// SNI succeeds on 8, which is what rules out "the address is dead".
    ///
    /// Always a subset of `ips`: the address stays a candidate to dial, and this
    /// only says which name to leave out when it is the one that answers. A flag
    /// on an address nobody dials would mean nothing, so the two are kept in step
    /// rather than the flag living in a list of its own.
    ///
    /// It is *not* inferable from "the first field is an address". A hosts file
    /// listing a domain's real address (`20.205.243.166 github.com`) describes an
    /// origin that does answer to the client's name, and dropping SNI there would
    /// ask a different question of a different server. The rule has to say which
    /// it means, which is what the `!` token is for.
    pub no_sni: Vec<IpAddr>,
    /// CDN placeholders found in `ips`.
    pub placeholders: Vec<Placeholder>,
    /// Upstream `port`, when present. Used as the default when the client does
    /// not supply one.
    pub port: Option<u16>,
    /// Certificate names, normalized and de-duplicated.
    pub cert: Vec<String>,
    /// Upstream `isPlaceholder` flag, retained for diagnostics.
    pub is_placeholder: bool,
}

impl CompiledEntry {
    /// True when the entry can steer traffic to a concrete address.
    pub fn has_concrete_ips(&self) -> bool {
        !self.ips.is_empty()
    }

    /// Best certificate name to present when the entry supplies several.
    ///
    /// The first non-wildcard name is preferred because it matches the host the
    /// client actually asked for in the common case.
    pub fn preferred_cert(&self) -> Option<&str> {
        self.cert
            .iter()
            .find(|name| !name.starts_with('*'))
            .or_else(|| self.cert.first())
            .map(String::as_str)
    }
}

/// Aggregate counters describing a compiled rule set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleStats {
    pub groups: usize,
    pub entries: usize,
    pub domains: usize,
    pub concrete_ips: usize,
    pub placeholder_entries: usize,
}

/// Result of a successful domain lookup.
#[derive(Debug, Clone)]
pub struct Match<'a> {
    pub entry: &'a CompiledEntry,
    /// The query after normalization.
    pub query: String,
    /// The rule domain that produced the match; may be a parent of `query`.
    pub rule_domain: &'a str,
}

/// Compiled, indexed rule set.
#[derive(Debug, Clone)]
pub struct RuleSet {
    meta: Meta,
    source: RuleSource,
    entries: Vec<CompiledEntry>,
    domain_index: HashMap<String, u32>,
    ip_index: HashMap<IpAddr, Vec<u32>>,
    filtered: Vec<FilteredGroup>,
    stats: RuleStats,
    /// Domains claimed by more than one entry; kept for diagnostics.
    duplicate_domains: Vec<String>,
}

impl RuleSet {
    /// Compile a parsed document into an indexed rule set.
    pub fn from_document(doc: RuleDocument, source: RuleSource) -> Result<Self> {
        let mut entries: Vec<CompiledEntry> = Vec::new();
        let mut domain_index: HashMap<String, u32> = HashMap::new();
        let mut duplicate_domains: Vec<String> = Vec::new();
        let mut domain_count = 0usize;

        for group in &doc.groups {
            for entry in &group.entries {
                let mut domains: Vec<String> = Vec::new();
                for raw in &entry.domains {
                    let Some(normalized) = normalize_rule_domain(raw) else {
                        continue;
                    };
                    if domains.contains(&normalized) {
                        continue;
                    }
                    domains.push(normalized);
                }
                if domains.is_empty() {
                    // Nothing to match on; the entry cannot be used.
                    continue;
                }

                // A `HashSet` rather than `Vec::contains`: one entry can list
                // nearly a thousand addresses (`autopatchos.starrails.com` has
                // 971), and scanning the growing list for each one makes loading
                // the rule set quadratic — measurable as seconds of startup.
                let mut ips: Vec<IpAddr> = Vec::new();
                let mut seen_ips: HashSet<IpAddr> = HashSet::new();
                let mut no_sni: Vec<IpAddr> = Vec::new();
                let mut dial_names: Vec<String> = Vec::new();
                let mut placeholders: Vec<Placeholder> = Vec::new();
                for raw in &entry.ips {
                    if let Some(placeholder) = Placeholder::parse(raw) {
                        if !placeholders.contains(&placeholder) {
                            placeholders.push(placeholder);
                        }
                        continue;
                    }
                    // `!<address>`: dial it and present no SNI. Checked before the
                    // bare-address branch because `!` is not part of an address,
                    // so the two can never be confused.
                    //
                    // A malformed one is dropped rather than reinterpreted. `!name`
                    // is a contradiction — a name *is* what SNI carries, so there is
                    // no way to present nothing — and reading it as the plain dial
                    // name `name` would silently discard the half of the author's
                    // intent that the `!` expressed. One fewer candidate is the
                    // honest outcome, and the same one a typo gets everywhere else
                    // in this loop.
                    if let Some(rest) = raw.trim().strip_prefix('!') {
                        if let Ok(addr) = rest.trim().parse::<IpAddr>() {
                            if !addr.is_unspecified() {
                                if seen_ips.insert(addr) {
                                    ips.push(addr);
                                }
                                // After `seen_ips` so that a bare `1.2.3.4`
                                // elsewhere in the same list cannot un-flag it:
                                // the `!` is the more specific claim, and it wins
                                // whichever order the two appear in.
                                if !no_sni.contains(&addr) {
                                    no_sni.push(addr);
                                }
                            }
                        }
                        continue;
                    }
                    if let Ok(addr) = raw.trim().parse::<IpAddr>() {
                        // Deduplicated here, once. The data plane's ranking relies
                        // on this: it used to re-check for duplicates on every
                        // connection, which was quadratic again on the same lists.
                        if !addr.is_unspecified() && seen_ips.insert(addr) {
                            ips.push(addr);
                        }
                        continue;
                    }
                    // Neither an address nor a placeholder: the rule is naming a
                    // host to resolve. Anything that is not a well-formed name is
                    // dropped rather than guessed at, so a typo degrades into
                    // "this entry has one fewer candidate" instead of a connect
                    // attempt against nonsense.
                    if let Some(name) = normalize_dial_name(raw) {
                        if !dial_names.contains(&name) {
                            dial_names.push(name);
                        }
                    }
                }

                let mut cert: Vec<String> = Vec::new();
                for name in entry.cert_names() {
                    let Some(normalized) = normalize_rule_domain(&name) else {
                        continue;
                    };
                    if !cert.contains(&normalized) {
                        cert.push(normalized);
                    }
                }

                let key = entries.len() as u32;
                for domain in &domains {
                    match domain_index.get(domain) {
                        // First writer wins so that compilation is deterministic
                        // regardless of group ordering changes upstream.
                        Some(_) => duplicate_domains.push(domain.clone()),
                        None => {
                            domain_index.insert(domain.clone(), key);
                            domain_count += 1;
                        }
                    }
                }

                entries.push(CompiledEntry {
                    key,
                    id: entry.id.clone(),
                    name: entry.name.clone(),
                    group: group.group.clone(),
                    domains,
                    ips,
                    dial_names,
                    no_sni,
                    placeholders,
                    port: entry.port,
                    cert,
                    is_placeholder: entry.is_placeholder,
                });
            }
        }

        if entries.is_empty() {
            return Err(RuleError::NoUsableEntries);
        }

        let mut ip_index: HashMap<IpAddr, Vec<u32>> = HashMap::new();
        let mut concrete_ips = 0usize;
        for entry in &entries {
            for addr in &entry.ips {
                let bucket = ip_index.entry(*addr).or_default();
                if bucket.is_empty() {
                    concrete_ips += 1;
                }
                bucket.push(entry.key);
            }
        }

        let stats = RuleStats {
            groups: doc.groups.len(),
            entries: entries.len(),
            domains: domain_count,
            concrete_ips,
            placeholder_entries: entries
                .iter()
                .filter(|entry| !entry.placeholders.is_empty() && entry.ips.is_empty())
                .count(),
        };

        duplicate_domains.sort();
        duplicate_domains.dedup();

        Ok(RuleSet {
            meta: doc.meta,
            source,
            entries,
            domain_index,
            ip_index,
            filtered: doc.filtered,
            stats,
            duplicate_domains,
        })
    }

    /// Compile a rule set from a raw JSON payload.
    pub fn from_slice(raw: &[u8], source: RuleSource) -> Result<Self> {
        Self::from_document(parse_document(raw)?, source)
    }
    /// Compile a rule set from a JSON string.
    pub fn from_str(raw: &str, source: RuleSource) -> Result<Self> {
        Self::from_slice(raw.as_bytes(), source)
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    pub fn source(&self) -> RuleSource {
        self.source
    }

    pub fn stats(&self) -> RuleStats {
        self.stats
    }

    pub fn filtered(&self) -> &[FilteredGroup] {
        &self.filtered
    }

    pub fn duplicate_domains(&self) -> &[String] {
        &self.duplicate_domains
    }

    /// All compiled entries, in document order.
    pub fn entries(&self) -> &[CompiledEntry] {
        &self.entries
    }

    /// Fetch an entry by its dense key.
    pub fn entry(&self, key: u32) -> Option<&CompiledEntry> {
        self.entries.get(key as usize)
    }

    /// Fetch an entry by its upstream string id.
    pub fn entry_by_id(&self, id: &str) -> Option<&CompiledEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// Find the entry responsible for `domain`.
    ///
    /// The most specific rule wins: `a.b.example.com` prefers a rule for
    /// `a.b.example.com` over one for `example.com`. Subdomains are included, so
    /// a rule for `example.com` also covers `cdn.example.com`.
    pub fn lookup(&self, domain: &str) -> Option<Match<'_>> {
        let query = normalize_domain(domain)?;

        // Resolve the owning key first, in a scope that ends the borrow of
        // `query`. The returned `rule_domain` borrows from `self.domain_index`
        // rather than from the normalized string, which is what lets `query` be
        // moved into the result afterwards.
        let (rule_domain, key) = {
            let mut hit = None;
            for candidate in suffixes(&query) {
                if let Some((key, value)) = self.domain_index.get_key_value(candidate) {
                    hit = Some((key.as_str(), *value));
                    break;
                }
            }
            hit
        }?;

        let entry = self.entries.get(key as usize)?;
        Some(Match {
            entry,
            query,
            rule_domain,
        })
    }

    /// Entries that claim `addr` as one of their concrete addresses.
    pub fn entries_for_ip(&self, addr: IpAddr) -> &[u32] {
        self.ip_index.get(&addr).map(Vec::as_slice).unwrap_or(&[])
    }

    /// True when any entry claims `addr`.
    pub fn owns_ip(&self, addr: IpAddr) -> bool {
        self.ip_index.contains_key(&addr)
    }

    /// Every domain in the set, used to prime DNS observation caches.
    pub fn domains(&self) -> impl Iterator<Item = &str> {
        self.domain_index.keys().map(String::as_str)
    }
}

/// Parse a JSON rule document, rejecting an empty payload.
///
/// Free function rather than a method because it belongs to the document, not to
/// the compiled set: the merge path parses several documents and compiles once.
pub fn parse_document(raw: &[u8]) -> Result<RuleDocument> {
    if raw.is_empty() {
        return Err(RuleError::Empty);
    }
    Ok(serde_json::from_slice(raw)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(ips: &str) -> RuleSet {
        let doc = format!(
            r#"{{"version":"test","groups":[{{"group":"g","entries":[
                {{"id":"1","name":"e","domains":["dial.example"],
                 "ips":{ips},"port":"443","isPlaceholder":false}}
            ]}}]}}"#
        );
        RuleSet::from_str(&doc, RuleSource::Provided).expect("fixture compiles")
    }

    fn only_entry(rules: &RuleSet) -> &CompiledEntry {
        rules.entry(0).expect("one entry")
    }

    #[test]
    fn a_non_address_string_is_a_dial_name() {
        let rules = compile(r#"["steamstore-a.akamaihd.net.edgesuite.net"]"#);
        let entry = only_entry(&rules);
        assert_eq!(
            entry.dial_names,
            vec!["steamstore-a.akamaihd.net.edgesuite.net"]
        );
        assert!(entry.ips.is_empty(), "a name must not be mistaken for an address");
        assert!(entry.placeholders.is_empty());
    }

    #[test]
    fn addresses_and_dial_names_coexist() {
        let rules = compile(r#"["203.0.113.10","steamstore-a.akamaihd.net.edgesuite.net"]"#);
        let entry = only_entry(&rules);
        assert_eq!(entry.ips.len(), 1);
        assert_eq!(entry.dial_names.len(), 1);
    }

    #[test]
    fn a_placeholder_is_not_a_dial_name() {
        let rules = compile(r#"["{Cloudflare}"]"#);
        let entry = only_entry(&rules);
        assert!(entry.dial_names.is_empty());
        assert_eq!(entry.placeholders.len(), 1);
    }

    #[test]
    fn a_wildcard_is_not_a_dial_name() {
        // It parses as a hostname, but dialling it is meaningless, so it is
        // dropped rather than resolved into something arbitrary.
        let rules = compile(r#"["*.akamaihd.net"]"#);
        let entry = only_entry(&rules);
        assert!(entry.dial_names.is_empty());
        assert!(entry.ips.is_empty());
    }

    #[test]
    fn a_bang_address_is_a_route_that_presents_no_name() {
        // The third state: dial this address and ask the origin for nothing.
        // The address has to stay in `ips` — it is still what gets dialled, and
        // `no_sni` alone would leave the entry looking like a placeholder.
        let rules = compile(r#"["!203.0.113.7"]"#);
        let entry = only_entry(&rules);
        assert_eq!(entry.ips, vec!["203.0.113.7".parse::<IpAddr>().unwrap()]);
        assert_eq!(entry.no_sni, vec!["203.0.113.7".parse::<IpAddr>().unwrap()]);
        assert!(
            entry.has_concrete_ips(),
            "it is dialable, not a placeholder"
        );
        assert!(entry.dial_names.is_empty());
        assert!(entry.placeholders.is_empty());
    }

    #[test]
    fn a_plain_address_is_dialled_under_the_clients_own_name() {
        // The guard against the tempting shortcut of reading "the first field is
        // an address" as "this origin has no name". A hosts file that lists a
        // domain's real address is describing an origin that *does* answer to the
        // client's name, and switching it to no SNI would ask a different server
        // a different question.
        let rules = compile(r#"["203.0.113.7"]"#);
        let entry = only_entry(&rules);
        assert_eq!(entry.ips.len(), 1);
        assert!(entry.no_sni.is_empty());
    }

    #[test]
    fn a_bang_token_that_is_not_an_address_is_dropped() {
        // `!name` is a contradiction — a name is exactly what SNI carries — so it
        // is dropped rather than quietly read as the plain dial name `name`.
        // `!1.2.3` is a typo, and an unspecified address is a hosts-file blocklist
        // entry, which must not become a route just because it was flagged.
        let rules = compile(r#"["!edge.example.net","!1.2.3","!0.0.0.0","!203.0.113.7"]"#);
        let entry = only_entry(&rules);
        assert_eq!(entry.ips, vec!["203.0.113.7".parse::<IpAddr>().unwrap()]);
        assert_eq!(entry.no_sni, vec!["203.0.113.7".parse::<IpAddr>().unwrap()]);
        assert!(
            entry.dial_names.is_empty(),
            "`!edge.example.net` is not a name"
        );
    }

    #[test]
    fn the_bang_form_wins_over_the_bare_one_in_either_order() {
        // One entry may list the same address twice, once flagged. The flag is the
        // more specific claim, so it must not depend on which line came first —
        // the compiled entry is a set, and a set has no order to lose.
        for ips in [
            r#"["203.0.113.7","!203.0.113.7"]"#,
            r#"["!203.0.113.7","203.0.113.7"]"#,
        ] {
            let rules = compile(ips);
            let entry = only_entry(&rules);
            assert_eq!(entry.ips.len(), 1, "still one address, not two");
            assert_eq!(
                entry.no_sni,
                vec!["203.0.113.7".parse::<IpAddr>().unwrap()],
                "for {ips}"
            );
        }
    }

    #[test]
    fn dial_names_are_deduplicated_and_ordered() {
        let rules = compile(
            r#"["b.example.com","a.example.com","B.Example.COM"]"#,
        );
        let entry = only_entry(&rules);
        assert_eq!(entry.dial_names, vec!["b.example.com", "a.example.com"]);
    }
}
