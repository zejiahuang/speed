//! Load and endurance test for the full-traffic kernel, on a real TUN device.
//!
//! `tun_smoke` proves the path works. This one asks whether it keeps working:
//! many connections at once, a bulk transfer large enough to exercise
//! backpressure, a wide UDP table, and a run long enough that a descriptor or a
//! buffer leak has nowhere to hide.
//!
//! It reports two things the smoke test cannot:
//!
//! * a `SAMPLE` line every second carrying the process' resident memory and open
//!   descriptor count, which is where a leak shows up first;
//! * a settle phase after the client stops, so the verdict is formed against a
//!   drained kernel rather than one that is still tearing flows down.
//!
//! Driven by `scripts/tun-stress.sh`.

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::process;
use std::time::{Duration, Instant};

use watt_rules::{Router, RuleSet, RuleSource};
use watt_stack::{open_tun_unprotected, DestinationOverride, PacketDevice, StackConfig, Stats};

const POLL_MS: u64 = 20;
/// Resident pages are 4 KiB on every architecture this runs on.
const PAGE_KIB: usize = 4;
/// How much the process may grow before it counts as a leak. Generous on
/// purpose: the allocator keeps freed flow buffers around, and the point is to
/// catch growth that never comes back, not to police the heap.
const MEMORY_GROWTH_LIMIT_KIB: usize = 32 * 1024;
/// How many descriptors may still be open at the end beyond the baseline.
const DESCRIPTOR_SLACK: usize = 8;

/// The rule set the stress test steers by. Mirrors `tun_smoke`.
const STRESS_DOC: &str = r#"
{
  "meta": { "version": "stress", "update_time": "2026-01-01T00:00:00Z" },
  "groups": [
    {
      "group": "Stress",
      "entries": [
        {
          "id": "stress-cdn",
          "name": "Stress CDN",
          "domains": ["cdn.stress.test"],
          "ips": ["203.0.113.10", "203.0.113.11"],
          "port": "443",
          "isPlaceholder": false
        }
      ]
    }
  ]
}
"#;

const DNS_ADDRESS: &str = "203.0.113.53";
const RULE_ADDRESS: &str = "203.0.113.10";
const UDP_ADDRESS: &str = "203.0.113.80";
const RULE_DOMAIN: &str = "cdn.stress.test";

fn main() {
    if let Err(err) = run() {
        eprintln!("tun-stress: {err}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse()?;

    let rules = match options.rules.as_deref() {
        Some(path) => RuleSet::from_slice(&std::fs::read(path)?, RuleSource::Provided)?,
        None => RuleSet::from_str(STRESS_DOC, RuleSource::Provided)?,
    };
    let rules_stats = rules.stats();
    println!(
        "RULES entries={} domains={} concrete_ips={} placeholders={}",
        rules_stats.entries, rules_stats.domains, rules_stats.concrete_ips, rules_stats.placeholder_entries
    );

    let config = build_config(&options);
    let mut engine = open_tun_unprotected(config, Router::new(rules))?;

    println!(
        "TUN_READY name={} mtu={}",
        engine.device().name(),
        engine.device().mtu()
    );
    println!(
        "SMOKE dns={DNS_ADDRESS} rule={RULE_ADDRESS} udp={UDP_ADDRESS} domain={RULE_DOMAIN} connections={} udp_flows={} bulk_bytes={}",
        options.connections, options.udp_flows, options.bulk_bytes
    );
    if let (Some(tcp), Some(udp)) = (options.tcp_port, options.udp_port) {
        println!("REWRITE {RULE_ADDRESS}:80 -> 127.0.0.1:{tcp}, {UDP_ADDRESS}:{udp} -> 127.0.0.1:{udp}");
    }
    std::io::stdout().flush()?;

    let baseline = Sample::now();
    println!("BASELINE {}", baseline.describe(0.0));
    let mut peak = baseline;

    // --- load phase ---------------------------------------------------------
    let started = Instant::now();
    let deadline = started + Duration::from_secs(options.seconds);
    let mut next_sample = started;
    let mut stop_reason = "deadline";

    while Instant::now() < deadline {
        engine.step(Duration::from_millis(POLL_MS))?;

        if let Some(path) = options.stop_file.as_deref() {
            if path.exists() {
                stop_reason = "stop-file";
                break;
            }
        }

        let now = Instant::now();
        if now >= next_sample {
            let sample = Sample::now();
            peak = peak.peak(sample);
            if !options.quiet {
                println!(
                    "SAMPLE {} {}",
                    sample.describe(started.elapsed().as_secs_f64()),
                    describe_progress(engine.stats())
                );
            }
            next_sample = now + Duration::from_secs(options.sample_seconds);
        }
    }
    let load_elapsed = started.elapsed();

    // --- settle phase -------------------------------------------------------
    // Nothing new arrives now. Whatever is still open is either a flow the
    // kernel failed to reap or one that is genuinely still finishing, and the
    // difference is what the checks below are about.
    let settle_deadline = Instant::now() + Duration::from_secs(options.settle);
    while Instant::now() < settle_deadline {
        engine.step(Duration::from_millis(POLL_MS))?;
    }

    let final_sample = Sample::now();
    peak = peak.peak(final_sample);

    let totals = engine.stats().clone();
    println!(
        "STOPPED reason={stop_reason} load_s={:.1} settle_s={}",
        load_elapsed.as_secs_f64(),
        options.settle
    );
    println!("PEAK {}", peak.describe(0.0));
    print_stats("FINAL", &totals);
    let snapshot = engine.flows(Instant::now());
    describe_flows(&snapshot);

    check(&options, &totals, &snapshot, baseline, final_sample)
}

fn build_config(options: &Options) -> StackConfig {
    let mut config = StackConfig {
        tun_name: options.tun.clone(),
        ..StackConfig::default()
    };

    // A burst of simultaneous connections to one host needs one listening socket
    // each, so the ceiling has to clear the widest wave the client will open.
    config.max_listeners_per_endpoint = config.max_listeners_per_endpoint.max(options.listener_ceiling);
    config.max_listeners = config.max_listeners.max(options.listener_ceiling * 4);

    // Zero means "whatever the default is", which is the interesting case: a run
    // that leaves it alone is testing the shipped configuration rather than a
    // ceiling the harness picked to make the test pass.
    if options.udp_ceiling > 0 {
        config.max_udp_flows = options.udp_ceiling;
    }

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

/// One observation of what the process is holding.
#[derive(Debug, Clone, Copy)]
struct Sample {
    fds: usize,
    rss_kib: usize,
}

impl Sample {
    fn now() -> Self {
        Self {
            fds: open_descriptors(),
            rss_kib: resident_kib(),
        }
    }

    /// The highest value each field has reached across two observations.
    fn peak(self, other: Self) -> Self {
        Self {
            fds: self.fds.max(other.fds),
            rss_kib: self.rss_kib.max(other.rss_kib),
        }
    }

    fn describe(&self, elapsed_s: f64) -> String {
        format!("t={elapsed_s:.1}s fds={} rss_kib={}", self.fds, self.rss_kib)
    }
}

/// Count the descriptors this process holds.
///
/// Reading the directory is the only way to see a leak of descriptors that are
/// not owned by anything the kernel tracks.
fn open_descriptors() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|entries| entries.count())
        .unwrap_or(0)
}

/// Resident set size, from `/proc/self/statm`.
fn resident_kib() -> usize {
    let Ok(text) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    let mut fields = text.split_whitespace();
    let _total = fields.next();
    fields
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        * PAGE_KIB
}

/// The counters that say whether the kernel is still keeping up.
///
/// A stalled load test is the one case where the final report is not enough: by
/// the time the verdict is printed, the moment worth seeing has passed. Putting
/// the flow table's size and its refusal counters on every sample line is what
/// turns "the client hung" into "the table filled up at t=30s".
fn describe_progress(stats: &Stats) -> String {
    format!(
        "flows={} tcp_open={} udp_open={} udp_reused={} udp_evicted={} udp_rejected={} \
         udp_dropped={} ceiling_hits={} c2u={} u2c={}",
        stats.open_flows(),
        stats.tcp_flows_opened,
        stats.udp_flows_opened,
        stats.udp_datagrams_reused,
        stats.udp_flows_evicted,
        stats.udp_flows_rejected,
        stats.udp_datagrams_dropped,
        stats.tcp_listener_ceiling_hits,
        stats.bytes_client_to_upstream,
        stats.bytes_upstream_to_client,
    )
}

fn print_stats(prefix: &str, stats: &Stats) {
    println!(
        "{prefix} in={} out={} unparsable={} ignored={} \
         tcp_open={} tcp_closed={} tcp_rejected={} tcp_resets={} tcp_failed={} tcp_exhausted={} \
         udp_open={} udp_closed={} udp_reused={} udp_rejected={} udp_evicted={} udp_dropped={} \
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
        stats.udp_datagrams_reused,
        stats.udp_flows_rejected,
        stats.udp_flows_evicted,
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

fn describe_flows(snapshot: &watt_stack::FlowSnapshot) {
    println!(
        "FLOWS tcp={} udp={} listeners={}",
        snapshot.tcp.len(),
        snapshot.udp.len(),
        snapshot.tcp_listeners
    );

    // A handful of stragglers is normal; listing them makes a leak obvious.
    for flow in snapshot.tcp.iter().take(10) {
        println!(
            "TCP_OPEN requested={} target={} steered={} reason={} failed={} age_s={:.1} idle_s={:.1}",
            flow.requested,
            flow.target
                .map(|target| target.to_string())
                .unwrap_or_else(|| "-".to_string()),
            flow.steered,
            flow.reason,
            flow.failed,
            flow.age.as_secs_f64(),
            flow.idle.as_secs_f64(),
        );
    }
}

fn check(
    options: &Options,
    stats: &Stats,
    snapshot: &watt_stack::FlowSnapshot,
    baseline: Sample,
    final_sample: Sample,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut checks = Checks::default();

    checks.required(
        "the client's traffic arrived",
        stats.packets_in >= options.connections + options.udp_flows,
        format!(
            "packets_in={} (>= {} expected)",
            stats.packets_in,
            options.connections + options.udp_flows
        ),
    );
    checks.required(
        "every packet was parsable",
        stats.packets_unparsable == 0,
        format!("packets_unparsable={}", stats.packets_unparsable),
    );
    checks.required(
        "every connection opened",
        stats.tcp_flows_opened >= options.connections,
        format!(
            "tcp_flows_opened={} (>= {} expected)",
            stats.tcp_flows_opened, options.connections
        ),
    );
    checks.required(
        "no connection was refused",
        stats.tcp_flows_rejected == 0,
        format!("tcp_flows_rejected={}", stats.tcp_flows_rejected),
    );
    checks.required(
        "no upstream connect failed",
        stats.tcp_connect_failures == 0,
        format!("tcp_connect_failures={}", stats.tcp_connect_failures),
    );
    // A ceiling hit does not prove a client was refused — a socket already in the
    // pool can serve the SYN — so the authoritative check is the client's own
    // verdict. Reported here so a ceiling that is genuinely too low stays visible.
    checks.advisory(
        "the listener ceiling was never reached",
        stats.tcp_listener_ceiling_hits == 0,
        format!(
            "tcp_listener_ceiling_hits={} (ceiling {})",
            stats.tcp_listener_ceiling_hits, options.listener_ceiling
        ),
    );
    // A datagram either opened a flow or was carried by one that already existed:
    // the client's ephemeral port allocator can hand out a port the previous
    // round's socket left behind, and the datagram then lands on the flow that
    // socket left in the table. Counting only the opens would make a working NAT
    // look like it had lost traffic.
    checks.required(
        "every datagram was carried by a flow",
        stats.udp_flows_opened + stats.udp_datagrams_reused >= options.udp_flows,
        format!(
            "udp_flows_opened={} udp_datagrams_reused={} (>= {} expected)",
            stats.udp_flows_opened, stats.udp_datagrams_reused, options.udp_flows
        ),
    );
    checks.required(
        "no datagram was rejected",
        stats.udp_flows_rejected == 0 && stats.udp_datagrams_dropped == 0,
        format!(
            "udp_flows_rejected={} udp_datagrams_dropped={}",
            stats.udp_flows_rejected, stats.udp_datagrams_dropped
        ),
    );
    // Eviction is the table's pressure valve, so it firing is not a failure. A
    // count close to the number of flows opened means the ceiling is doing the
    // work, and the idle guard is the only thing standing between the table and a
    // thrash that would rebuild every flow on every datagram.
    checks.advisory(
        "the udp table did not have to churn",
        stats.udp_flows_evicted * 4 <= stats.udp_flows_opened,
        format!(
            "udp_flows_evicted={} of {} opened",
            stats.udp_flows_evicted, stats.udp_flows_opened
        ),
    );

    if options.bulk_bytes > 0 {
        checks.required(
            "the bulk transfer completed",
            stats.bytes_upstream_to_client >= options.bulk_bytes,
            format!(
                "bytes_upstream_to_client={} (>= {} expected)",
                stats.bytes_upstream_to_client, options.bulk_bytes
            ),
        );
    }

    // Everything the client opened should be gone once the dust settles. The
    // kernel reaps a flow when both ends have closed, so a non-zero count here
    // means flows are being held after they finish.
    checks.required(
        "connections were reaped after they closed",
        stats.tcp_flows_closed + 2 >= stats.tcp_flows_opened,
        format!(
            "opened={} closed={}",
            stats.tcp_flows_opened, stats.tcp_flows_closed
        ),
    );

    // Descriptors are only evidence of a leak once the flows that legitimately
    // hold them are accounted for. A UDP flow stays in the table until it has
    // been idle for a minute, so the settle phase will not have reaped it.
    let live_flows = snapshot.tcp.len() + snapshot.udp.len();
    let allowed = baseline.fds + DESCRIPTOR_SLACK + live_flows;
    checks.required(
        "descriptors came back",
        final_sample.fds <= allowed,
        format!(
            "baseline={} final={} live_flows={} (allowed {allowed})",
            baseline.fds, final_sample.fds, live_flows
        ),
    );

    let growth = final_sample.rss_kib.saturating_sub(baseline.rss_kib);
    checks.required(
        "memory came back",
        growth < MEMORY_GROWTH_LIMIT_KIB,
        format!(
            "baseline={}KiB final={}KiB growth={}KiB (limit {}KiB)",
            baseline.rss_kib, final_sample.rss_kib, growth, MEMORY_GROWTH_LIMIT_KIB
        ),
    );

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
    settle: u64,
    sample_seconds: u64,
    tcp_port: Option<u16>,
    udp_port: Option<u16>,
    rules: Option<PathBuf>,
    stop_file: Option<PathBuf>,
    quiet: bool,
    connections: u64,
    udp_flows: u64,
    bulk_bytes: u64,
    listener_ceiling: usize,
    udp_ceiling: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            tun: "watt0".to_string(),
            seconds: 60,
            settle: 3,
            sample_seconds: 1,
            tcp_port: None,
            udp_port: None,
            rules: None,
            stop_file: None,
            quiet: false,
            connections: 0,
            udp_flows: 0,
            bulk_bytes: 0,
            listener_ceiling: 0,
            udp_ceiling: 0,
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
                "--settle" => options.settle = value(&mut args, "--settle")?,
                "--sample-seconds" => options.sample_seconds = value(&mut args, "--sample-seconds")?,
                "--tcp-port" => options.tcp_port = Some(value(&mut args, "--tcp-port")?),
                "--udp-port" => options.udp_port = Some(value(&mut args, "--udp-port")?),
                "--connections" => options.connections = value(&mut args, "--connections")?,
                "--udp-flows" => options.udp_flows = value(&mut args, "--udp-flows")?,
                "--bulk-bytes" => options.bulk_bytes = value(&mut args, "--bulk-bytes")?,
                "--listener-ceiling" => {
                    options.listener_ceiling = value(&mut args, "--listener-ceiling")?
                }
                "--udp-ceiling" => options.udp_ceiling = value(&mut args, "--udp-ceiling")?,
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
        "usage: tun_stress [options]\n\
         \n\
         Options:\n\
           --tun <name>         interface to create (default: watt0)\n\
           --seconds <n>        how long to take load (default: 60)\n\
           --settle <n>         quiet seconds before the verdict (default: 3)\n\
           --sample-seconds <n> gap between SAMPLE lines (default: 1)\n\
           --connections <n>    TCP connections the client will open\n\
           --udp-flows <n>      UDP flows the client will open\n\
           --bulk-bytes <n>     bytes the bulk transfer will move\n\
           --listener-ceiling <n>  raise the per-destination listener ceiling\n\
           --udp-ceiling <n>    override the UDP flow table ceiling (default: config)\n\
           --tcp-port <n>       rewrite {RULE_ADDRESS}:80 to 127.0.0.1:<n>\n\
           --udp-port <n>       rewrite {UDP_ADDRESS}:<n> to 127.0.0.1:<n>\n\
           --rules <path>       steer by this document instead of the built-in set\n\
           --stop-file <path>   start the settle phase once this file exists\n\
           --quiet              only print the final report\n\
         \n\
         Needs root: creating a TUN device requires CAP_NET_ADMIN. Driven by\n\
         scripts/tun-stress.sh, which also configures the interface."
    );
}
