//! Rule engine for the Watt Android full-traffic kernel.
//!
//! The crate is deliberately free of networking, file descriptors and unsafe
//! code so that every routing decision can be tested as a pure function.
//!
//! ```text
//! upstream JSON ──▶ model ──▶ RuleSet (indexed) ──▶ Router ──▶ Plan
//!                                  ▲                  ▲
//!                                  │                  │
//!                            RuleCache (disk)     IpSelector (history)
//!                                  ▲
//!                                  │
//!                   RuleUpdater (builtin │ cache │ download)
//! ```
//!
//! Typical use:
//!
//! ```
//! use std::net::IpAddr;
//! use std::time::Instant;
//! use watt_rules::{Family, RuleSource, Router, RuleSet, Strategy};
//!
//! let rules = RuleSet::from_str(
//!     r#"{"groups":[{"entries":[
//!         {"id":"1","name":"Example","domains":["example.com"],
//!          "ips":["203.0.113.7"],"port":"443"}
//!     ]}]}"#,
//!     RuleSource::Provided,
//! )
//! .unwrap();
//!
//! let router = Router::new(rules);
//! // A subdomain of a rule domain inherits the rule.
//! let plan = router.plan(Instant::now(), "cdn.example.com", Family::V4);
//! assert_eq!(plan.strategy, Strategy::RuleAddresses);
//! assert_eq!(plan.addresses, vec!["203.0.113.7".parse::<IpAddr>().unwrap()]);
//!
//! // A name no rule covers is left alone.
//! let plan = router.plan(Instant::now(), "elsewhere.test", Family::V4);
//! assert_eq!(plan.strategy, Strategy::Direct);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod builtin;
pub mod cache;
pub mod domain;
pub mod error;
pub mod filter;
pub mod hosts;
pub mod merge;
pub mod model;
pub mod router;
pub mod ruleset;
pub mod selector;
pub mod update;

pub use builtin::BUILTIN_RULES_JSON;
pub use cache::{CacheMeta, RuleCache};
pub use hosts::{parse as parse_hosts, HostsStats};
pub use merge::merge_documents;
pub use domain::{is_subdomain_of, normalize_domain, normalize_rule_domain, suffixes};
pub use error::{Result, RuleError};
pub use filter::{filter_document, FilterPolicy, FilterStats};
pub use model::{Entry, FilteredGroup, Group, Meta, RuleDocument};
pub use router::{Family, Plan, Router, Strategy};
pub use ruleset::{
    parse_document, CompiledEntry, Match, Placeholder, RuleSet, RuleSource, RuleStats,
};
pub use selector::{IpSelector, IpSelectorConfig, IpStat, Outcome};
pub use update::{Fetcher, FileFetcher, LoadedRules, RuleOrigin, RuleUpdater};
