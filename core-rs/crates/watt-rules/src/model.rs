//! Serde model for the rule document the kernel parses.
//!
//! This is **not** the shape the upstream short paths return by default. `/1`
//! and `/2` serve hosts text, and their `?format=json` form is a *different*
//! schema (`{"entries":[{"ip":..,"domain":..}]}`) that this model cannot read:
//! every field below is `#[serde(default)]`, so that mismatch deserializes as an
//! `Ok` document with zero entries rather than an error. What happens next
//! depends on the caller, and the two callers differ:
//!
//! * `RuleSet::from_document` (the daemon's `--rules-url` path) rejects an empty
//!   set with [`crate::RuleError::NoUsableEntries`], so the download fails and the
//!   previous/built-in rules are kept. Loud.
//! * `dyn`/merge path (`watt_rules::parse_document` used directly, e.g. the C
//!   ABI's merge) does **not** reject it: a zero-entry document merges to a
//!   zero-entry document, and the app comes up relaying nothing. Silent. See
//!   `RulesRepository.kt`.
//!
//! Either way the mismatch is a bug to avoid, not to rely on. The kernel is fed
//! hosts text parsed by [`crate::parse_hosts`]; this model exists for the JSON
//! documents that do match it (the built-in set, and `/rules` while it was live).
//!
//! The document is third-party data that evolves independently of this client,
//! so every field is optional and every scalar is accepted in more than one
//! representation. Two quirks are handled explicitly:
//!
//! * `id` and `port` are JSON **strings** (`"21001"`, `"443"`), not numbers.
//! * `cert` is a single comma separated string, not an array.
//!
//! Unknown fields are ignored so that new upstream keys never break parsing.

use serde::{Deserialize, Deserializer, Serialize};

/// Root of the rule document.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RuleDocument {
    #[serde(default)]
    pub meta: Meta,
    #[serde(default)]
    pub groups: Vec<Group>,
    /// Groups the upstream maintainer removed from the mobile profile.
    #[serde(default)]
    pub filtered: Vec<FilteredGroup>,
}

/// Document level metadata, used for cache validation and diagnostics.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Meta {
    #[serde(default, deserialize_with = "de_string_or_number")]
    pub version: String,
    #[serde(default)]
    pub update_time: Option<String>,
}

/// A named group of entries, for example `developer` / `开发者资源`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Group {
    #[serde(default)]
    pub group: String,
    #[serde(default, rename = "groupZh")]
    pub group_zh: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default, rename = "iconSlug")]
    pub icon_slug: Option<String>,
    #[serde(default)]
    pub entries: Vec<Entry>,
    #[serde(default, rename = "iconUrl")]
    pub icon_url: Option<String>,
}

/// One routing rule: a set of domains that should resolve to a set of IPs.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Entry {
    #[serde(default, deserialize_with = "de_string_or_number")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "nameZh")]
    pub name_zh: Option<String>,
    /// Concrete addresses, or CDN placeholders such as `{Cloudflare}`.
    #[serde(default)]
    pub ips: Vec<String>,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default, deserialize_with = "de_opt_u16")]
    pub port: Option<u16>,
    /// Comma separated certificate names, e.g. `a.example.com,*.example.com`.
    #[serde(default)]
    pub cert: Option<String>,
    /// Upstream flag meaning "`ips` holds a CDN placeholder, not real addresses".
    #[serde(default, rename = "isPlaceholder")]
    pub is_placeholder: bool,
    #[serde(default, rename = "iconSlug")]
    pub icon_slug: Option<String>,
    #[serde(default, rename = "ipCountry")]
    pub ip_country: Option<String>,
    #[serde(default, rename = "ipCountryName")]
    pub ip_country_name: Option<String>,
    #[serde(default, rename = "iconUrl")]
    pub icon_url: Option<String>,
}

/// A group present upstream but excluded from the mobile profile.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FilteredGroup {
    #[serde(default)]
    pub group: String,
    #[serde(default, rename = "groupZh")]
    pub group_zh: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default, rename = "iconSlug")]
    pub icon_slug: Option<String>,
}

/// Accept `"443"`, `443` and `null` for fields whose type drifted upstream.
fn de_string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    })
}

/// Accept a port as string, number or `null`; anything else becomes `None`.
fn de_opt_u16<'de, D>(deserializer: D) -> Result<Option<u16>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Number(n) => n.as_u64().and_then(|v| u16::try_from(v).ok()),
        serde_json::Value::String(s) => s.trim().parse::<u16>().ok(),
        _ => None,
    })
}

impl Entry {
    /// Split `cert` into individual names, trimmed and with empty parts removed.
    pub fn cert_names(&self) -> Vec<String> {
        self.cert
            .as_deref()
            .map(|raw| {
                raw.split(',')
                    .map(|part| part.trim())
                    .filter(|part| !part.is_empty())
                    .map(|part| part.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "meta": { "version": "1.0.47", "update_time": "2026/09/05 16:13" },
      "groups": [
        {
          "group": "developer",
          "groupZh": "开发者资源",
          "category": "MOBILE_USEFUL",
          "iconSlug": "github",
          "entries": [
            {
              "id": "20004",
              "name": "GitHub.com",
              "ips": ["140.82.121.4", "2a04:4e42::1"],
              "domains": ["github.com"],
              "port": "443",
              "cert": "github.com,*.github.com",
              "isPlaceholder": false,
              "iconSlug": "github",
              "ipCountry": null,
              "ipCountryName": null,
              "iconUrl": "https://example.invalid/icon/20004"
            },
            {
              "id": 21001,
              "name": "Cdnjs",
              "ips": ["{Cloudflare}"],
              "domains": ["cdnjs.com", "cdnjs.cloudflare.com"],
              "port": 443,
              "isPlaceholder": true
            }
          ]
        }
      ],
      "filtered": [{ "group": "Steam", "groupZh": "Steam", "reason": "PC_ONLY", "iconSlug": "steam" }]
    }"#;

    #[test]
    fn parses_string_and_number_scalars() {
        let doc: RuleDocument = serde_json::from_str(SAMPLE).expect("sample must parse");
        assert_eq!(doc.meta.version, "1.0.47");
        assert_eq!(doc.groups.len(), 1);
        assert_eq!(doc.filtered.len(), 1);

        let entries = &doc.groups[0].entries;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "20004");
        assert_eq!(entries[0].port, Some(443));
        assert_eq!(entries[0].cert_names(), vec!["github.com", "*.github.com"]);

        // The second entry uses numeric scalars and omits `cert` entirely.
        assert_eq!(entries[1].id, "21001");
        assert_eq!(entries[1].port, Some(443));
        assert!(entries[1].is_placeholder);
        assert!(entries[1].cert_names().is_empty());
    }

    #[test]
    fn tolerates_missing_and_unknown_fields() {
        let doc: RuleDocument =
            serde_json::from_str(r#"{ "groups": [ { "entries": [ { "domains": ["a.com"], "brandNewKey": 7 } ] } ] }"#)
                .expect("minimal document must parse");
        let entry = &doc.groups[0].entries[0];
        assert_eq!(entry.id, "");
        assert_eq!(entry.port, None);
        assert!(!entry.is_placeholder);
    }
}
