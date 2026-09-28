//! Command line driver for the full-traffic kernel.
//!
//! This is the piece that turns the libraries into something a person can run.
//! It owns exactly four things, and nothing else:
//!
//! 1. **The rule document's lifecycle** — which copy to start with, when to fetch
//!    a new one, and what to do when the fetch fails.
//! 2. **The tunnel's identity** — interface name, address, MTU.
//! 3. **The clock** — how often staleness is checked and how often the counters
//!    are written out.
//! 4. **Signals** — `SIGUSR1` to refresh now, `SIGINT`/`SIGTERM` to stop.
//!
//! It deliberately does *not* configure the interface. Assigning the address and
//! installing a route are `ip addr` and `ip route` on a host, and
//! `VpnService.Builder` on Android, and keeping them out of the program means the
//! daemon never has to shell out.
//!
//! ```text
//!   --check ──▶ load rules ──▶ report ──▶ exit            (no root needed)
//!
//!   default ──▶ load rules ──▶ refresh if stale ──▶ open TUN ──▶ loop
//!                                                              │
//!                        SIGUSR1 ──────────────────────────────┤ refresh
//!                        tick ─────────────────────────────────┤ refresh if stale
//!                        SIGINT / SIGTERM ─────────────────────┘ stop
//! ```

mod fetcher;
mod options;


use std::io::Write;
use std::net::{TcpListener, ToSocketAddrs};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use watt_rules::{
    Family, Fetcher, FileFetcher, LoadedRules, RuleCache, RuleDocument, RuleOrigin, RuleSet,
    RuleSource, RuleUpdater,
};
use watt_stack::{
    Engine, MarkProtector, NoProtector, PacketDevice, Protector, StackConfig, Stats,
    TunDevice,
};

use crate::fetcher::CurlFetcher;
use crate::options::{usage, Options};

/// How long the engine blocks waiting for a descriptor each iteration.
///
/// The loop checks its signals and its timers between iterations, so this is also
/// the worst case latency for `SIGUSR1` and for a stop request.
const POLL: Duration = Duration::from_millis(20);

static SHUTDOWN: AtomicBool = AtomicBool::new(false);
static REFRESH_NOW: AtomicBool = AtomicBool::new(false);

extern "C" fn request_shutdown(_signal: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

extern "C" fn request_refresh(_signal: libc::c_int) {
    REFRESH_NOW.store(true, Ordering::SeqCst);
}

/// Install the three handlers the daemon reacts to.
///
/// `signal(3)` rather than `sigaction(2)`: the default glibc behaviour of
/// restarting interrupted calls is exactly what a `poll` loop wants, and none of
/// the handlers needs the extra control `sigaction` offers.
fn install_signal_handlers() {
    // `sighandler_t` is an integer, so a function item cannot be cast to it in
    // one step; it goes through a pointer, which is what the kernel wants anyway.
    let shutdown = request_shutdown as *const () as libc::sighandler_t;
    let refresh = request_refresh as *const () as libc::sighandler_t;

    // SAFETY: both handlers only store to an atomic, which is the whole of what
    // is async-signal-safe. The previous handler is deliberately dropped: nothing
    // else in this process installs one.
    unsafe {
        libc::signal(libc::SIGINT, shutdown);
        libc::signal(libc::SIGTERM, shutdown);
        libc::signal(libc::SIGUSR1, refresh);
    }
}

fn main() {
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) if message.is_empty() => {
            println!("{}", usage());
            return;
        }
        Err(message) => {
            eprintln!("watt-daemon: {message}");
            eprintln!("try --help");
            process::exit(2);
        }
    };

    if let Err(err) = run(&options) {
        eprintln!("watt-daemon: {err}");
        process::exit(1);
    }
}

fn run(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let updater = RuleUpdater::new(
        options.rules_url.clone(),
        RuleCache::new(&options.cache_dir),
        options.max_age,
    );
    let mut fetcher = pick_fetcher(options);
    // A hosts file, a pinned document and a hosts URL are all "do not refresh":
    // each names exactly what to run. With the defaults this is every bare run —
    // the live source is hosts, and hosts have no cache to refresh from — which
    // is why a bare `watt-daemon` run fetches the default short path once and does
    // not re-fetch on the staleness timer.
    let pinned =
        options.rules_file.is_some() || options.hosts_file.is_some() || !options.hosts_url.is_empty();

    // --- rules --------------------------------------------------------------
    //
    // Every source that was named contributes, and the documents are unioned per
    // domain before compiling. The default is one source: `/1` (UsbEAm host
    // records — 5225 domains, 15952 real addresses). `/2` (the Steamcommunity 302
    // hijack block) is *not* a default and merging it is not "better": its 862
    // addresses are all `127.0.0.1`, and `Planner::can_relay` refuses a
    // non-overridden loopback target, so the 675 merged domains it names alone are
    // reset instead of going direct. Naming two is still allowed for an operator
    // who runs a loopback reverse proxy. The old `/rules` and `/hosts?all=1`
    // endpoints are retired.
    //
    // The union has to happen before compilation: the compiler keeps only the
    // first entry that claims a domain, so compiling each source and merging
    // afterwards would discard exactly the addresses this is here to keep.
    let mut documents = Vec::new();
    let (origin, fetched_at) = if let Some(path) = &options.rules_file {
        documents.push(watt_rules::parse_document(&std::fs::read(path)?)?);
        (RuleOrigin::Provided, None)
    } else {
        match updater.cache().read_raw()? {
            Some(raw) => {
                documents.push(watt_rules::parse_document(&raw)?);
                (
                    RuleOrigin::Cache,
                    updater.cache().meta().and_then(|meta| meta.fetched_at()),
                )
            }
            None => {
                documents.push(watt_rules::parse_document(
                    watt_rules::BUILTIN_RULES_JSON.as_bytes(),
                )?);
                (RuleOrigin::Builtin, None)
            }
        }
    };

    if let Some(path) = &options.hosts_file {
        documents.push(load_hosts_document(
            started,
            &std::fs::read(path)?,
            &format!("file:{}", path.display()),
        )?);
    }
    // Every hosts source named contributes its own document; by default that is
    // `/1` alone, unioned with the base document below. Skipped entirely under
    // `--offline`: the default is a network URL, so without this guard `--offline`
    // would still fetch it, which is the one thing the flag promises not to do.
    if !options.offline {
        for url in &options.hosts_url {
            let raw = fetcher.fetch(url)?;
            documents.push(load_hosts_document(started, &raw, url)?);
        }
    }

    let mut loaded = if documents.len() > 1 {
        let merged = watt_rules::merge_documents(&documents);
        let domains = merged
            .groups
            .iter()
            .map(|group| group.entries.len())
            .sum::<usize>();
        let multi = merged
            .groups
            .iter()
            .flat_map(|group| group.entries.iter())
            .filter(|entry| entry.ips.len() > 1)
            .count();
        log(
            started,
            "merge",
            &format!(
                "sources={} domains={domains} multi_address={multi}",
                documents.len()
            ),
        );
        LoadedRules {
            rules: RuleSet::from_document(merged, RuleSource::Provided)?,
            origin,
            fetched_at,
        }
    } else {
        let document = documents.pop().expect("one document was pushed");
        LoadedRules {
            rules: RuleSet::from_document(document, RuleSource::Provided)?,
            origin,
            fetched_at,
        }
    };
    log(started, "rules", &loaded.describe());

    // A pinned document is never refreshed, and `--offline` means the cache is
    // the newest thing available by definition. An empty `rules_url` means there
    // is no JSON endpoint to refresh from (the default — see `DEFAULT_RULES_URL`),
    // so the JSON lane stays on the cache or the built-in set and only the hosts
    // sources, already loaded above, carry live rules.
    if !pinned && !options.offline && !options.rules_url.is_empty() {
        loaded = refresh_if_stale(started, &updater, fetcher.as_mut(), loaded);
    }

    if options.check {
        report(started, &loaded, &options.probe);
        return Ok(());
    }

    // --- HTTP CONNECT proxy -------------------------------------------------
    //
    // This is the no-Root Android bootstrap path. It does not create a TUN,
    // does not alter routes, and never terminates TLS; the proxy only sees the
    // CONNECT authority and then copies opaque bytes.
    if let Some(address) = &options.proxy_listen {
        let listener = TcpListener::bind(address)?;
        log(started, "proxy", &format!("listen={} tls=opaque policy=listed-domains-only", listener.local_addr()?));
        watt_proxy::serve(listener, watt_rules::Router::new(loaded.rules))?;
        return Ok(());
    }

    // --- tunnel -------------------------------------------------------------
    install_signal_handlers();

    let config = StackConfig {
        tun_name: options.tun.clone(),
        mtu: options.mtu,
        address: options.address,
        prefix_len: options.prefix_len,
        overrides: options.overrides.clone(),
        ..StackConfig::default()
    };
    // The protector has to exist before the engine does, because every upstream
    // socket is handed to it at creation.
    let protector: Box<dyn Protector> = match options.protect_mark {
        Some(mark) => Box::new(MarkProtector::new(mark)),
        None => Box::new(NoProtector),
    };
    let mut engine = Engine::open_tun(
        config,
        watt_rules::Router::new(loaded.rules),
        protector,
    )?;

    match options.protect_mark {
        Some(mark) => log(
            started,
            "protect",
            &format!(
                "mark=0x{mark:x}; the host needs `ip rule add fwmark 0x{mark:x} lookup <table>` \
                 pointing at a table without the tunnel routes"
            ),
        ),
        None => log(
            started,
            "protect",
            "none; relayed destinations must be overridden to addresses outside the \
             tunnel's routes, or the kernel will capture its own upstream traffic",
        ),
    }
    log(
        started,
        "tun",
        &format!(
            "name={} mtu={} origin={}",
            engine.device().name(),
            engine.device().mtu(),
            loaded.origin
        ),
    );

    // Printed because an override silently not applying looks exactly like a rule
    // that did not match, and the two need very different debugging.
    for rule in &options.overrides {
        log(
            started,
            "override",
            &format!(
                "match={}{} target={}{}",
                rule.match_addr,
                rule.match_port
                    .map(|port| format!(":{port}"))
                    .unwrap_or_else(|| " (any port)".to_string()),
                rule.target_addr,
                rule.target_port
                    .map(|port| format!(":{port}"))
                    .unwrap_or_else(|| " (keep port)".to_string()),
            ),
        );
    }

    let mut origin = loaded.origin;
    let mut last_tick = Instant::now();
    let mut last_stats = Instant::now();

    // Whether the JSON lane has an endpoint to refresh from at all. False on a
    // default run (no `--rules-url`), where refreshing would fetch the empty
    // string; true only when the operator named a JSON source.
    let has_json_source = !options.rules_url.is_empty();
    let can_refresh = !pinned && !options.offline && has_json_source;

    while !SHUTDOWN.load(Ordering::SeqCst) {
        engine.step(POLL)?;

        if REFRESH_NOW.swap(false, Ordering::SeqCst) && can_refresh {
            origin = refresh_now(started, &updater, fetcher.as_mut(), &mut engine, origin);
        }

        if last_tick.elapsed() >= options.tick {
            last_tick = Instant::now();
            if can_refresh && updater.should_refresh(SystemTime::now()) {
                origin = refresh_now(started, &updater, fetcher.as_mut(), &mut engine, origin);
            }
            // Re-resolve dial names on the same cadence. The engine cannot do this
            // itself: a lookup blocks, and its step must not. Doing it here is what
            // gives the TUN path dial-name support at all.
            let resolved = engine.resolve_dial_names(|name| {
                (name, 0u16)
                    .to_socket_addrs()
                    .map(|addrs| addrs.map(|addr| addr.ip()).collect())
                    .unwrap_or_default()
            });
            if resolved > 0 {
                log(
                    started,
                    "dial",
                    &format!("re-resolved dial names for {resolved} entries"),
                );
            }
        }

        if last_stats.elapsed() >= options.stats_interval {
            last_stats = Instant::now();
            log(started, "stats", &describe_stats(engine.stats()));
        }
    }

    log(
        started,
        "stop",
        &format!(
            "uptime_s={:.1} origin={origin} {}",
            started.elapsed().as_secs_f64(),
            describe_stats(engine.stats())
        ),
    );
    Ok(())
}

/// Fetch at start-up, but only when the cache has actually gone stale.
fn refresh_if_stale(
    started: Instant,
    updater: &RuleUpdater,
    fetcher: &mut dyn Fetcher,
    current: LoadedRules,
) -> LoadedRules {
    if !updater.should_refresh(SystemTime::now()) {
        let age = current
            .age(SystemTime::now())
            .map(|age| format!("{:.0}s", age.as_secs_f64()))
            .unwrap_or_else(|| "unknown".to_string());
        log(started, "refresh", &format!("cache is fresh, age {age}"));
        return current;
    }

    match updater.refresh(fetcher, SystemTime::now()) {
        Ok(fresh) => {
            log(started, "refresh", &fresh.describe());
            fresh
        }
        Err(err) => {
            log(
                started,
                "refresh",
                &format!("failed, keeping {}: {err}", current.origin),
            );
            current
        }
    }
}

/// Fetch and hand the result to the running engine, which swaps the rule set
/// without touching the flows that are already open.
fn refresh_now(
    started: Instant,
    updater: &RuleUpdater,
    fetcher: &mut dyn Fetcher,
    engine: &mut Engine<TunDevice>,
    current: RuleOrigin,
) -> RuleOrigin {
    match updater.refresh(fetcher, SystemTime::now()) {
        Ok(fresh) => {
            log(started, "refresh", &fresh.describe());
            let origin = fresh.origin;
            engine.replace_rules(fresh.rules);
            origin
        }
        Err(err) => {
            log(
                started,
                "refresh",
                &format!("failed, keeping {current}: {err}"),
            );
            current
        }
    }
}

/// Choose the transport for the named sources.
///
/// A URL with a scheme goes over the network. Anything else is a path on disk,
/// which is how a deployment serves the document from its own storage without
/// putting a web server in front of it.
///
/// The network fetcher is chosen when **either** the rules URL or any hosts URL
/// carries a scheme. Keying only off `rules_url` would break the default run: the
/// default `rules_url` is empty (see `DEFAULT_RULES_URL`) while the default hosts
/// URLs are `https://…`, so a `rules_url`-only check would hand the hosts URLs to
/// [`FileFetcher`], which would try to open `https://abhuang.dpdns.org/1` as a
/// local path and fail. The open handlers cover both cases.
fn pick_fetcher(options: &Options) -> Box<dyn Fetcher> {
    let wants_network = options.rules_url.contains("://")
        || options.hosts_url.iter().any(|url| url.contains("://"));
    if wants_network {
        Box::new(CurlFetcher::new(
            &options.fetch_program,
            options.fetch_timeout,
        ))
    } else {
        Box::new(FileFetcher)
    }
}

/// Print what the loaded rule set contains, and answer any probe lookups.
fn report(started: Instant, loaded: &LoadedRules, probes: &[String]) {
    let stats = loaded.rules.stats();
    log(
        started,
        "report",
        &format!(
            "groups={} entries={} domains={} concrete_ips={} placeholders={}",
            stats.groups,
            stats.entries,
            stats.domains,
            stats.concrete_ips,
            stats.placeholder_entries
        ),
    );

    let duplicates = loaded.rules.duplicate_domains();
    if !duplicates.is_empty() {
        log(
            started,
            "report",
            &format!(
                "{} domain(s) claimed by more than one entry; the first wins, e.g. {}",
                duplicates.len(),
                duplicates
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }

    if probes.is_empty() {
        return;
    }

    let router = watt_rules::Router::new(loaded.rules.clone());
    let now = Instant::now();
    for domain in probes {
        for family in [Family::V4, Family::V6] {
            let plan = router.plan(now, domain, family);
            let addresses: Vec<String> = plan
                .addresses
                .iter()
                .take(3)
                .map(|address| address.to_string())
                .collect();
            log(
                started,
                "probe",
                &format!(
                    "{domain} {} strategy={} entry={} addresses=[{}{}] port={}",
                    match family {
                        Family::V4 => "v4",
                        Family::V6 => "v6",
                    },
                    plan.strategy.as_str(),
                    plan.entry_name.as_deref().unwrap_or("-"),
                    addresses.join(", "),
                    if plan.addresses.len() > addresses.len() {
                        format!(", +{} more", plan.addresses.len() - addresses.len())
                    } else {
                        String::new()
                    },
                    plan.port
                        .map(|port| port.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                ),
            );
        }
    }
}

/// One line of counters, in the shape the examples print.
fn describe_stats(stats: &Stats) -> String {
    format!(
        "in={} out={} tcp_open={} tcp_closed={} tcp_rejected={} tcp_failed={} tcp_ceiling={} \
         udp_open={} udp_closed={} udp_reused={} udp_rejected={} udp_evicted={} \
         dns={} dns_local={} dns_trimmed={} observed={} \
         c2u={} u2c={} matched={} direct={} flows={}",
        stats.packets_in,
        stats.packets_out,
        stats.tcp_flows_opened,
        stats.tcp_flows_closed,
        stats.tcp_flows_rejected,
        stats.tcp_connect_failures,
        stats.tcp_listener_ceiling_hits,
        stats.udp_flows_opened,
        stats.udp_flows_closed,
        stats.udp_datagrams_reused,
        stats.udp_flows_rejected,
        stats.udp_flows_evicted,
        stats.dns_queries,
        stats.dns_answered_locally,
        stats.dns_answers_trimmed,
        stats.dns_addresses_observed,
        stats.bytes_client_to_upstream,
        stats.bytes_upstream_to_client,
        stats.flows_matched_rules,
        stats.flows_direct,
        stats.open_flows(),
    )
}

/// Compile a hosts file into the rule set the daemon runs.
///
/// Logged separately from the JSON path because the numbers are the reason to
/// use it: the live upstream serves hosts text (`/1`, `/2`), and this is the path
/// that reads it. The JSON path remains for documents that match `RuleDocument`
/// (the built-in set; a pinned file) — it cannot read the endpoints' hosts text,
/// and the endpoints' `?format=json` is a schema it silently reduces to zero
/// entries.
/// Parse a hosts payload into a document, logging what it held.
///
/// Returns the document rather than a compiled set so that several sources can be
/// unioned before compilation — see the rules block in [`run`].
fn load_hosts_document(
    started: Instant,
    raw: &[u8],
    origin: &str,
) -> Result<RuleDocument, Box<dyn std::error::Error>> {
    let text = std::str::from_utf8(raw)?;
    let (document, stats) = watt_rules::parse_hosts(text)?;
    log(
        started,
        "hosts",
        &format!(
            "origin={origin} version={} lines={} addresses={} dial_names={} domains={} groups={} skipped={}",
            document.meta.version,
            stats.lines,
            stats.addresses,
            stats.dial_names,
            stats.domains,
            stats.groups,
            stats.skipped
        ),
    );
    Ok(document)
}

/// Write one line to stdout, tagged and timestamped.
///
/// Flushed every time: a daemon's output is read through a pipe or a log file
/// while it is still running, and a buffered line is a line nobody has seen.
fn log(started: Instant, tag: &str, message: &str) {
    let elapsed = started.elapsed().as_secs_f64();
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "[{elapsed:8.1}s] {tag:<8} {message}");
    let _ = stdout.flush();
}
