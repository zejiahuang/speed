//! Ask the router what it would do for a few domains.
//!
//! Run: cargo run -p watt-stack --example plan_check -- <rules.json>
//!
//! Written because "the DNS was not answered locally" and "the rule does not
//! match" look identical from the app's counters, and only the router can say
//! which one it is.

use std::time::Instant;
use watt_rules::{Family, RuleSource, RuleSet, Router, Strategy};

fn main() {
    let path = std::env::args().nth(1).expect("usage: plan_check <rules.json>");
    let document = std::fs::read(&path).expect("read the rule document");
    println!("document: {} KiB", document.len() / 1024);

    let rules = RuleSet::from_slice(&document, RuleSource::Provided).expect("compile");
    println!("compiled: {} domains", rules.domains().count());
    let router = Router::new(rules);
    let now = Instant::now();

    for name in [
        "github.com",
        "api.github.com",
        "raw.githubusercontent.com",
        "objects.githubusercontent.com",
    ] {
        for family in [Family::V4, Family::V6] {
            let plan = router.plan(now, name, family);
            let strategy = match plan.strategy {
                Strategy::RuleAddresses => "RuleAddresses",
                Strategy::Direct => "Direct",
                other => {
                    let _ = other;
                    "other"
                }
            };
            println!(
                "  {name:32} {family:?} -> {strategy:14} addresses={} dial_names={}",
                plan.addresses.len(),
                plan.dial_names.len()
            );
        }
    }
}
