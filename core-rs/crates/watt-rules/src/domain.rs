//! Domain name normalization and suffix walking.
//!
//! The rule document lists plain ASCII domains, while queries may arrive as
//! `Example.COM.`, with trailing dots, or with uppercase letters. Everything is
//! funneled through [`normalize_domain`] before it reaches an index so that the
//! matcher only ever compares canonical strings.

/// Longest domain label accepted by DNS.
const MAX_LABEL_LEN: usize = 63;
/// Longest domain name accepted by DNS (wire format limit, excluding the root dot).
const MAX_DOMAIN_LEN: usize = 253;

/// Lowercase an ASCII domain and drop the optional trailing root dot.
///
/// Returns `None` for inputs that cannot be a hostname: empty strings, bare IP
/// literals, over-long names and names containing empty labels (`a..b`).
pub fn normalize_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    // Bracketed IPv6 literals (`[::1]`) and bare IP literals are handled by the
    // caller through `normalize_host`, not here.
    let without_root = trimmed.strip_suffix('.').unwrap_or(trimmed);
    if without_root.is_empty() || without_root.len() > MAX_DOMAIN_LEN {
        return None;
    }
    if without_root.starts_with('.') || without_root.contains("..") {
        return None;
    }

    let mut out = String::with_capacity(without_root.len());
    for ch in without_root.chars() {
        match ch {
            'A'..='Z' => out.push(ch.to_ascii_lowercase()),
            // Underscores appear in some service records; keep them.
            'a'..='z' | '0'..='9' | '-' | '_' | '.' => out.push(ch),
            // Keep non-ASCII as-is so IDN queries encoded as UTF-8 still match a
            // UTF-8 rule. Real clients normally send punycode, which is ASCII.
            c if !c.is_ascii() => out.push(c),
            _ => return None,
        }
    }

    if out.split('.').any(|label| label.is_empty() || label.len() > MAX_LABEL_LEN) {
        return None;
    }
    // A single label with no dot is legal in DNS but never appears in this rule
    // set; rejecting it avoids matching stray tokens such as "localhost".
    if !out.contains('.') {
        return None;
    }
    Some(out)
}

/// Strip the wildcard prefix used by rule files: `*.example.com` -> `example.com`.
///
/// A leading dot (`.example.com`) carries the same meaning in certificate lists
/// and is treated identically.
pub fn normalize_rule_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    let trimmed = trimmed.strip_prefix("*.").unwrap_or(trimmed);
    let trimmed = trimmed.strip_prefix('.').unwrap_or(trimmed);
    normalize_domain(trimmed)
}

/// Normalize a *dial name*: a hostname the kernel should resolve itself.
///
/// This is the same character set as a rule domain, with one difference that
/// matters: a wildcard is a matching pattern, not something that can be looked
/// up. `*.akamaihd.net` says "any host under here", which is a useful thing to
/// match against and a useless thing to resolve, so it is rejected rather than
/// silently stripped into a name the author never wrote.
pub fn normalize_dial_name(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.starts_with("*.") || trimmed.starts_with('.') {
        return None;
    }
    normalize_domain(trimmed)
}

/// Walk the domain from the most specific suffix to the least specific one.
///
/// `a.b.example.com` yields `a.b.example.com`, `b.example.com`, `example.com`.
/// The public suffix (`com`) is intentionally included so that a rule for a
/// single-label domain can still match, but such rules are rejected earlier by
/// [`normalize_domain`].
pub fn suffixes(domain: &str) -> impl Iterator<Item = &str> {
    SuffixWalker { rest: Some(domain) }
}

struct SuffixWalker<'a> {
    rest: Option<&'a str>,
}

impl<'a> Iterator for SuffixWalker<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.rest?;
        self.rest = current.find('.').map(|idx| &current[idx + 1..]);
        Some(current)
    }
}

/// True when `host` equals `rule` or is a subdomain of it.
///
/// `notexample.com` must not match `example.com`, which a naive
/// `ends_with` check would get wrong.
pub fn is_subdomain_of(host: &str, rule: &str) -> bool {
    if host == rule {
        return true;
    }
    match host.len().checked_sub(rule.len()) {
        Some(diff) if diff >= 2 => host.ends_with(rule) && host.as_bytes()[diff - 1] == b'.',
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_and_root_dot() {
        assert_eq!(normalize_domain("Example.COM.").as_deref(), Some("example.com"));
        assert_eq!(normalize_domain("  api.github.com  ").as_deref(), Some("api.github.com"));
    }

    #[test]
    fn rejects_unusable_inputs() {
        assert_eq!(normalize_domain(""), None);
        assert_eq!(normalize_domain("."), None);
        assert_eq!(normalize_domain("a..b.com"), None);
        assert_eq!(normalize_domain("localhost"), None);
        assert_eq!(normalize_domain("bad domain.com"), None);
        let long_label = format!("{}.com", "a".repeat(64));
        assert_eq!(normalize_domain(&long_label), None);
    }

    #[test]
    fn strips_wildcards_from_rule_domains() {
        assert_eq!(normalize_rule_domain("*.cdnjs.cloudflare.com").as_deref(), Some("cdnjs.cloudflare.com"));
        assert_eq!(normalize_rule_domain(".khanacademy.org").as_deref(), Some("khanacademy.org"));
        assert_eq!(normalize_rule_domain("github.com").as_deref(), Some("github.com"));
    }

    #[test]
    fn a_dial_name_normalizes_like_a_host() {
        assert_eq!(
            normalize_dial_name("  Steamstore-A.Akamaihd.NET.Edgesuite.net ").as_deref(),
            Some("steamstore-a.akamaihd.net.edgesuite.net")
        );
    }

    #[test]
    fn a_wildcard_is_not_a_dial_name() {
        // Stripping the `*.` would turn a matching pattern into a name the rule
        // author never wrote, and the kernel would then dial something invented.
        assert_eq!(normalize_dial_name("*.akamaihd.net"), None);
        assert_eq!(normalize_dial_name(".example.com"), None);
    }

    #[test]
    fn a_dial_name_must_be_resolvable_shaped() {
        assert_eq!(normalize_dial_name("localhost"), None, "single label is not looked up");
        assert_eq!(normalize_dial_name("has space.com"), None);
        assert_eq!(normalize_dial_name(""), None);
    }

    #[test]
    fn walks_suffixes_from_specific_to_general() {
        let got: Vec<&str> = suffixes("a.b.example.com").collect();
        assert_eq!(got, vec!["a.b.example.com", "b.example.com", "example.com", "com"]);
    }

    #[test]
    fn subdomain_check_does_not_over_match() {
        assert!(is_subdomain_of("example.com", "example.com"));
        assert!(is_subdomain_of("a.example.com", "example.com"));
        assert!(!is_subdomain_of("notexample.com", "example.com"));
        assert!(!is_subdomain_of("example.com.evil.net", "example.com"));
    }
}
