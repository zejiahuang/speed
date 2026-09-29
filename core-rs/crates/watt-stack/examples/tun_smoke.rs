//! End-to-end smoke test on a real Linux TUN device.
//!
//! This is the host equivalent of "install the VPN on a phone and browse": it
//! creates a real TUN interface, hands the descriptor to the engine, and then
//! lets the operating system's own routing push traffic into it. Nothing is
//! injected by hand, so a pass here means the whole path works — kernel routing,
//! the userspace TCP stack, both relays and the upstream sockets.
//!
//! It is driven by an internal harness that is not distributed with this
//! repository, which owns the parts that are not the library's job: assigning
//! the interface address, installing the route, and starting the local servers
//! that relayed flows are rewritten onto.
//!
//! Two destinations are deliberately rewritten rather than reached for real:
//!
//! * the rule address becomes a local server, so the test needs no internet
//!   access and no cooperation from anyone else's network;
//! * the same is done for the UDP target.
//!
//! Everything else is genuine. Packets leave a real interface, are read by the
//! engine, and are answered by the userspace stack, which is exactly the code
//! path Android's `VpnService` descriptor will exercise.
//!
//! # What each leg proves
//!
//! | Leg | Proves |
//! |---|---|
//! | DNS query for a rule domain | the rule set is reachable from the data plane and the answer is synthesised locally |
//! | three parallel TCP connections | the listener pool holds, so a browser's concurrency does not stall |
//! | one UDP datagram | the NAT table forwards and the reply is re-addressed |
//! | one ICMP echo | the kernel counts what it cannot relay instead of silently dropping it |

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::process;
use std::time::{Duration, Instant};

use watt_rules::{Router, RuleSet, RuleSource};
use watt_stack::{open_tun_unprotected, DestinationOverride, PacketDevice, StackConfig, Stats};

/// Longest the engine blocks per iteration. Small enough that a stalled
/// connection is not felt, large enough that an idle kernel costs nothing.
const POLL_MS: u64 = 20;

/// How often a progress line is printed while waiting for the client.
const REPORT_SECS: u64 = 5;

/// The rule set the smoke test steers by.
///
/// `203.0.113.0/24` is TEST-NET-3 (RFC 5737), which is guaranteed never to be a
/// real address. That makes it safe to route into a tunnel on a machine that
/// also has a working internet connection: nothing the host does on the real
/// network can collide with it.
const SMOKE_DOC: &str = r#"
{
  "meta": { "version": "smoke", "update_time": "2026-01-01T00:00:00Z" },
  "groups": [
    {
      "group": "Smoke",
      "entries": [
        {
          "id": "smoke-cdn",
          "name": "Smoke CDN",
          "nameZh": "冒烟测试",
          "domains": ["cdn.smoke.test", "smoke.test"],
          "ips": ["203.0.113.10", "203.0.113.11"],
          "port": "443",
          "isPlaceholder": false,
          "ipCountry": "ZZ",
          "ipCountryName": "Nowhere"
        }
      ]
    }
  ]
}
"#;

/// Address the smoke test treats as a DNS server.
const DNS_ADDRESS: &str = "203.0.113.53";
/// Address the rule set owns, so a connection to it is steered.
const RULE_ADDRESS: &str = "203.0.113.10";
/// Address no rule owns, used for the plain UDP leg.
const UDP_ADDRESS: &str = "203.0.113.80";
/// Domain the rule set owns.
const RULE_DOMAIN: &str = "cdn.smoke.test";

/// Concurrent TCP connections the client opens, to exercise the listener pool.
const PARALLEL_CONNECTIONS: u64 = 3;

fn main() {
    if let Err(err) = run() {
        eprintln!("tun-smoke: {err}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse()?;

    let rules = load_rules(options.rules.as_deref())?;
    let stats = rules.stats();
    println!(
        "RULES entries={} domains={} concrete_ips={} placeholders={}",
        stats.entries, stats.domains, stats.concrete_ips, stats.placeholder_entries
    );

    // Proving the rule engine finds the domain on a live host, and handing the
    // client the exact answer set to expect, both fall out of the same lookup.
    match rules.lookup(RULE_DOMAIN) {
        Some(matched) => println!(
            "RULE_DOMAIN {RULE_DOMAIN} ips={}",
            matched
                .entry
                .ips
                .iter()
                .map(|addr| addr.to_string())
                .collect::<Vec<_>>()
                .join(",")
        ),
        None => return Err(format!("the smoke rule set does not own {RULE_DOMAIN}").into()),
    }

    let config = build_config(&options);
    let mut engine = open_tun_unprotected(config, Router::new(rules))?;

    // The script waits for this line before configuring the interface, so it
    // must reach the file even when stdout is a pipe.
    println!(
        "TUN_READY name={} mtu={}",
        engine.device().name(),
        engine.device().mtu()
    );
    println!(
        "SMOKE dns={DNS_ADDRESS} rule={RULE_ADDRESS} udp={UDP_ADDRESS} domain={RULE_DOMAIN} parallel={PARALLEL_CONNECTIONS}"
    );
    if let (Some(tcp), Some(udp)) = (options.tcp_port, options.udp_port) {
        println!("REWRITE {RULE_ADDRESS}:80 -> 127.0.0.1:{tcp}, {UDP_ADDRESS}:{udp} -> 127.0.0.1:{udp}");
    }
    std::io::stdout().flush()?;

    let started = Instant::now();
    let deadline = started + Duration::from_secs(options.seconds);
    let mut last_report = started;
    let mut stop_reason = "deadline";

    while Instant::now() < deadline {
        engine.step(Duration::from_millis(POLL_MS))?;

        if let Some(path) = options.stop_file.as_deref() {
            if path.exists() {
                stop_reason = "stop-file";
                break;
            }
        }

        if !options.quiet && last_report.elapsed() >= Duration::from_secs(REPORT_SECS) {
            print_stats("STATS", engine.stats());
            last_report = Instant::now();
        }
    }

    println!(
        "STOPPED reason={stop_reason} uptime_s={:.1}",
        started.elapsed().as_secs_f64()
    );

    let totals = engine.stats().clone();
    print_stats("FINAL", &totals);
    describe_flows(&engine);
    check(&totals, options.tcp_port.is_some(), options.udp_port.is_some())
}

/// Read the rule document the engine will steer by.
///
/// A real upstream document can be handed in with `--rules`, which is how the
/// 1 MiB production file gets loaded on a live interface rather than only in a
/// unit test.
fn load_rules(path: Option<&std::path::Path>) -> Result<RuleSet, Box<dyn std::error::Error>> {
    match path {
        Some(path) => {
            let raw = std::fs::read(path)?;
            let started = Instant::now();
            let rules = RuleSet::from_slice(&raw, RuleSource::Provided)?;
            println!(
                "LOADED path={} bytes={} parse_ms={:.1}",
                path.display(),
                raw.len(),
                started.elapsed().as_secs_f64() * 1000.0
            );
            Ok(rules)
        }
        None => Ok(RuleSet::from_str(SMOKE_DOC, RuleSource::Provided)?),
    }
}

/// Build the engine configuration, including the rewrites that keep the test
/// off the real network.
fn build_config(options: &Options) -> StackConfig {
    let mut config = StackConfig {
        tun_name: options.tun.clone(),
        ..StackConfig::default()
    };

    if let Some(port) = options.tcp_port {
        config.overrides.push(DestinationOverride::endpoint(
            RULE_ADDRESS.parse().expect("valid literal"),
            80,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
        ));
    }

    if let Some(port) = options.udp_port {
        config.overrides.push(DestinationOverride::endpoint(
            UDP_ADDRESS.parse().expect("valid literal"),
            port,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
        ));
    }

    config
}

fn print_stats(prefix: &str, stats: &Stats) {
    println!(
        "{prefix} in={} out={} unparsable={} ignored={} \
         tcp_open={} tcp_closed={} tcp_rejected={} tcp_resets={} tcp_failed={} tcp_exhausted={} \
         udp_open={} udp_closed={} udp_rejected={} udp_dropped={} \
         dns={} dns_local={} dns_forwarded={} dns_bad={} observed={} \
         c2u={} u2c={} matched={} direct={}",
        stats.packets_in,
        stats.packets_out,
        stats.packets_unparsable,
        stats.packets_ignored,
        stats.tcp_flows_opened,
        stats.tcp_flows_closed,
        stats.tcp_flows_rejected,
        stats.tcp_resets_sent,
        stats.tcp_connect_failures,
        stats.tcp_listener_ceiling_hits,
        stats.udp_flows_opened,
        stats.udp_flows_closed,
        stats.udp_flows_rejected,
        stats.udp_datagrams_dropped,
        stats.dns_queries,
        stats.dns_answered_locally,
        stats.dns_forwarded,
        stats.dns_unparsable,
        stats.dns_addresses_observed,
        stats.bytes_client_to_upstream,
        stats.bytes_upstream_to_client,
        stats.flows_matched_rules,
        stats.flows_direct,
    );
}

/// Dump what is still being relayed, so a stall can be read off the log.
fn describe_flows(engine: &watt_stack::Engine<watt_stack::TunDevice>) {
    let snapshot = engine.flows(Instant::now());
    println!(
        "FLOWS tcp={} udp={} listeners={}",
        snapshot.tcp.len(),
        snapshot.udp.len(),
        snapshot.tcp_listeners
    );

    for flow in &snapshot.tcp {
        println!(
            "TCP requested={} target={} steered={} reason={} candidate={}/{} failed={} up={} down={} age_s={:.1} idle_s={:.1}",
            flow.requested,
            flow.target
                .map(|target| target.to_string())
                .unwrap_or_else(|| "-".to_string()),
            flow.steered,
            flow.reason,
            flow.candidate_index,
            flow.candidates,
            flow.failed,
            flow.pending_upstream,
            flow.pending_client,
            flow.age.as_secs_f64(),
            flow.idle.as_secs_f64(),
        );
    }

    for flow in &snapshot.udp {
        println!(
            "UDP requested={} target={} steered={} reason={} dns={} queued={} age_s={:.1} idle_s={:.1}",
            flow.requested,
            flow.target,
            flow.steered,
            flow.reason,
            flow.dns,
            flow.queued_upstream,
            flow.age.as_secs_f64(),
            flow.idle.as_secs_f64(),
        );
    }
}

/// Turn the counters into a verdict.
///
/// The exit status is the point: the shell script only has to look at it, and a
/// human reading the log sees which expectation failed and by how much.
fn check(
    stats: &Stats,
    expect_tcp: bool,
    expect_udp: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut checks = Checks::default();

    checks.required(
        "traffic reached the tunnel",
        stats.packets_in >= 4,
        format!("packets_in={}", stats.packets_in),
    );
    checks.required(
        "every packet was parsable",
        stats.packets_unparsable == 0,
        format!("packets_unparsable={}", stats.packets_unparsable),
    );
    checks.required(
        "the rule owned domain was answered locally",
        stats.dns_answered_locally >= 1,
        format!("dns_answered_locally={}", stats.dns_answered_locally),
    );
    checks.required(
        "the local answer taught the kernel the domain",
        stats.dns_addresses_observed >= 2,
        format!("dns_addresses_observed={}", stats.dns_addresses_observed),
    );

    if expect_tcp {
        checks.required(
            "parallel connections all opened",
            stats.tcp_flows_opened >= PARALLEL_CONNECTIONS,
            format!(
                "tcp_flows_opened={} (expected >= {PARALLEL_CONNECTIONS})",
                stats.tcp_flows_opened
            ),
        );
        checks.required(
            "no connection failed to reach upstream",
            stats.tcp_connect_failures == 0,
            format!("tcp_connect_failures={}", stats.tcp_connect_failures),
        );
        checks.required(
            "every SYN found a listener",
            stats.tcp_listener_ceiling_hits == 0,
            format!(
                "tcp_listener_ceiling_hits={}",
                stats.tcp_listener_ceiling_hits
            ),
        );
        checks.required(
            "bytes travelled upstream",
            stats.bytes_client_to_upstream > 0,
            format!("bytes_client_to_upstream={}", stats.bytes_client_to_upstream),
        );
        checks.required(
            "bytes travelled back",
            stats.bytes_upstream_to_client > 0,
            format!("bytes_upstream_to_client={}", stats.bytes_upstream_to_client),
        );
    }

    if expect_udp {
        checks.required(
            "the datagram was relayed",
            stats.udp_flows_opened >= 1,
            format!("udp_flows_opened={}", stats.udp_flows_opened),
        );
        checks.required(
            "the datagram was not dropped",
            stats.udp_datagrams_dropped == 0,
            format!("udp_datagrams_dropped={}", stats.udp_datagrams_dropped),
        );
    }

    // The ICMP leg depends on a raw socket being permitted, so a miss here is
    // reported without failing the run.
    checks.advisory(
        "icmp was counted rather than relayed",
        stats.packets_ignored >= 1,
        format!("packets_ignored={}", stats.packets_ignored),
    );

    if checks.failures > 0 {
        println!("RESULT FAIL failures={} passed={}", checks.failures, checks.passed);
        return Err(format!("{} check(s) failed", checks.failures).into());
    }

    println!("RESULT PASS checks={}", checks.passed);
    Ok(())
}

#[derive(Default)]
struct Checks {
    passed: usize,
    failures: usize,
}

impl Checks {
    fn required(&mut self, name: &str, ok: bool, detail: String) {
        if ok {
            self.passed += 1;
            println!("CHECK PASS {name} ({detail})");
        } else {
            self.failures += 1;
            println!("CHECK FAIL {name} ({detail})");
        }
    }

    /// A check that depends on something outside the kernel. A miss is
    /// reported, but does not fail the run.
    fn advisory(&mut self, name: &str, ok: bool, detail: String) {
        if ok {
            self.passed += 1;
            println!("CHECK PASS {name} ({detail})");
        } else {
            println!("CHECK WARN {name} ({detail})");
        }
    }
}

#[derive(Debug)]
struct Options {
    tun: String,
    seconds: u64,
    tcp_port: Option<u16>,
    udp_port: Option<u16>,
    rules: Option<PathBuf>,
    stop_file: Option<PathBuf>,
    quiet: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            tun: "watt0".to_string(),
            seconds: 60,
            tcp_port: None,
            udp_port: None,
            rules: None,
            stop_file: None,
            quiet: false,
        }
    }
}

impl Options {
    fn parse() -> Result<Self, String> {
        let mut options = Options::default();
        let mut args = std::env::args().skip(1);

        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--tun" => options.tun = args.next().ok_or("--tun needs a name")?,
                "--seconds" => options.seconds = value(&mut args, "--seconds")?,
                "--tcp-port" => options.tcp_port = Some(value(&mut args, "--tcp-port")?),
                "--udp-port" => options.udp_port = Some(value(&mut args, "--udp-port")?),
                "--rules" => {
                    options.rules = Some(PathBuf::from(args.next().ok_or("--rules needs a path")?));
                }
                "--stop-file" => {
                    options.stop_file =
                        Some(PathBuf::from(args.next().ok_or("--stop-file needs a path")?));
                }
                "--quiet" => options.quiet = true,
                "--help" | "-h" => {
                    print_usage();
                    process::exit(0);
                }
                other => return Err(format!("unrecognised argument {other:?}, try --help")),
            }
        }

        Ok(options)
    }
}

fn value<T>(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
    raw.parse().map_err(|err| format!("{flag}: {err}"))
}

fn print_usage() {
    println!(
        "usage: tun_smoke [options]\n\
         \n\
         Options:\n\
           --tun <name>       interface to create (default: watt0)\n\
           --seconds <n>      hard limit on the run (default: 60)\n\
           --tcp-port <n>     rewrite {RULE_ADDRESS}:80 to 127.0.0.1:<n>\n\
           --udp-port <n>     rewrite {UDP_ADDRESS}:<n> to 127.0.0.1:<n>\n\
           --rules <path>     steer by this document instead of the built-in smoke set\n\
           --stop-file <path> exit cleanly once this file exists\n\
           --quiet            only print the final report\n\
         \n\
         Needs root: creating a TUN device requires CAP_NET_ADMIN. The interface\n\
         still has to be addressed and routed by the caller; the external harness\n\
         does both."
    );
}
