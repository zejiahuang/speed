//! Command line surface.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use watt_net::DEFAULT_MTU;
use watt_stack::DestinationOverride;

/// The JSON lane's source, which is deliberately **empty** by default.
///
/// An empty `rules_url` means "there is no JSON endpoint to refresh from": the
/// JSON lane then contributes only the on-disk cache or the compiled-in
/// [`watt_rules::BUILTIN_RULES_JSON`]. That is the honest default because the
/// upstream retired the only endpoint that served this shape (`/rules` now
/// returns an nginx 404) and no public replacement does — `/1` and `/2` serve
/// *hosts text*, and their `?format=json` is a different schema
/// (`{"entries":[{"ip":..,"domain":..}]}`) that [`watt_rules::parse_document`]
/// cannot read. Pointing this constant at `/rules` would ship a 404; pointing it
/// at `/1?format=json` would ship a download that fails every six hours. An
/// operator who runs their own upstream rule aggregator emitting the `groups`
/// shape passes `--rules-url https://their-host/rules` and the JSON lane works
/// again.
///
/// `Options::parse` skips the JSON refresh when this is empty (see `run`), so an
/// empty default does not produce a fetch of the empty string.
pub const DEFAULT_RULES_URL: &str = "";

/// The live rule source used when nothing else is configured.
///
/// **`/1` only — deliberately.** `/1` is UsbEAm host records: 5225 domains and
/// 15952 addresses, none of them a loopback placeholder the way `/2`'s are.
/// "Real" means *routable*, not *working*: the source also carries 19 `0.0.0.0`
/// rows (e.g. `ap-gae2.spotify.com`), which the hosts parser drops as blocklist
/// rows (`hosts.rs`, `is_unspecified`), and sampled 2026-09-26 only about 7 in
/// 10 domains had any address that passed both TCP reachability and a
/// certificate check for its own name — so "every domain it names is dialable"
/// would be too strong.
///
/// `/2` is the Steamcommunity 302 hijack block: 862 domains whose addresses are
/// **all `127.0.0.1`**. In the world of the upstream aggregator that produced
/// this block, that address is meaningful — there a local reverse proxy listens
/// on loopback and the hijack points traffic at it. In *this* kernel it is not:
/// `Planner::can_relay` (`watt-stack`, planner.rs) returns false for a
/// non-overridden loopback target, and `tcp.rs` answers such a flow with
/// `socket.abort()` and a RST (`tcp_flows_rejected`). So folding `/2` in does not
/// *add* domains — it **breaks** them: measured, the
/// merged set is 5905 domains, 5230 hold a real address, and **675 are
/// loopback-only, every one of them `/2`-only**. Those 675 would otherwise be
/// unmatched and go **direct and work**; carrying `/2` turns each into a refusal.
///
/// That is why the default is `/1` alone, and why this is not the "merge both
/// sources" rule the JSON era used. An operator who *does* run a reverse proxy on
/// loopback can add it explicitly:
///
/// ```text
/// watt-daemon --hosts-url https://abhuang.dpdns.org/2
/// ```
///
/// The app makes the same call (`RulesRepository`'s `merged` is `/1` only); the
/// daemon and the app must agree on what "default rules" means.
///
/// These are hosts text, fetched through the same `--hosts-url` path as any
/// other hosts source, and therefore also the reason `rules_url` can be empty.
pub const DEFAULT_HOSTS_URLS: &[&str] = &["https://abhuang.dpdns.org/1"];

/// Default refresh age. Six hours is frequent enough that a rule added upstream
/// is picked up the same day, and rare enough that a phone on a metered
/// connection is not fetching a megabyte every hour.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);

/// How often the daemon looks at whether the cache has gone stale.
///
/// This is not the refresh interval — that is `max_age`. It is only how often the
/// age is compared against it, which is why it can be much smaller than the
/// interval it enforces.
pub const DEFAULT_TICK: Duration = Duration::from_secs(60);

/// How often the running counters are written out.
pub const DEFAULT_STATS_INTERVAL: Duration = Duration::from_secs(300);

/// Everything the daemon can be told to do.
#[derive(Debug, Clone)]
pub struct Options {
    /// Interface name to request from the kernel.
    pub tun: String,
    /// Address the interface claims, and the prefix reported for it.
    pub address: IpAddr,
    pub prefix_len: u8,
    /// Interface MTU.
    pub mtu: usize,

    /// Optional local HTTP CONNECT listener. This mode is the no-Root Android
    /// bootstrap path: an app can point its HTTP proxy setting at it without
    /// creating a TUN or modifying routes. TLS bytes are copied opaquely.
    pub proxy_listen: Option<String>,

    /// Static destination rewrites, consulted before any rule.
    ///
    /// The rule set says where a domain's traffic should go; an override says
    /// where a specific address must go instead, whatever the rules think. That
    /// makes it the tool for pointing a destination at a different host without
    /// editing the document — and the only way to exercise the relay end to end
    /// against a local server, since a rule's addresses are otherwise reached
    /// for real.
    pub overrides: Vec<DestinationOverride>,

    /// Socket mark used to exempt the kernel's own upstream connections.
    ///
    /// `None` leaves them unprotected, which is only safe when every relayed
    /// destination is overridden to something outside the tunnel's routes. See
    /// [`watt_stack::MarkProtector`] for the policy rule this requires.
    pub protect_mark: Option<u32>,

    /// Where to download the rule document from.
    pub rules_url: String,
    /// A local document to compile instead, bypassing cache and network.
    pub rules_file: Option<PathBuf>,

    /// A local hosts file to compile as a source document.
    ///
    /// This is the form the live endpoint serves (see [`DEFAULT_HOSTS_URLS`]).
    /// Naming a file drops the default network source (a pinned run must not
    /// also phone home); an explicit `--hosts-url` alongside it is honoured and
    /// the file is added on top. Pass `--offline` to fetch nothing.
    pub hosts_file: Option<PathBuf>,

    /// Hosts sources to download once at startup and compile.
    ///
    /// A list, not a single URL, so an operator can name more than one source
    /// (e.g. `/1` plus the loopback hijack block for a setup that runs a reverse
    /// proxy). Repeating `--hosts-url` appends rather than replacing (the
    /// opposite of `--rules-url`, which is last-wins). Defaults to
    /// [`DEFAULT_HOSTS_URLS`], which is `/1` alone — see that constant for why
    /// `/2` is not a default.
    ///
    /// One shot rather than cached-and-refreshed: a hosts file is a flat address
    /// list with no version negotiation, so there is nothing to compare a cached
    /// copy against, and a refresh would only ever replace it wholesale.
    pub hosts_url: Vec<String>,
    /// Directory holding the cached document.
    pub cache_dir: PathBuf,
    /// Age at which a cached document counts as stale.
    pub max_age: Duration,
    /// How often staleness is checked.
    pub tick: Duration,
    /// How often the counters are logged.
    pub stats_interval: Duration,

    /// Program used to fetch the document.
    pub fetch_program: String,
    /// Give up on a fetch after this long.
    pub fetch_timeout: Duration,

    /// Load the rules, report, and exit without opening a tunnel.
    pub check: bool,
    /// Never touch the network, even if the cache is stale.
    pub offline: bool,
    /// Domains to look up in the report, to show the index working.
    pub probe: Vec<String>,
    /// Reduce the per-iteration log to the periodic counters.
    pub quiet: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            tun: "watt0".to_string(),
            // 198.18.0.0/15 is reserved for benchmarking by RFC 2544 and is not
            // routable, which makes it a safe private identity for the tunnel.
            address: "198.18.0.1".parse().expect("valid literal"),
            prefix_len: 15,
            mtu: DEFAULT_MTU,
            proxy_listen: None,
            overrides: Vec::new(),
            protect_mark: None,
            rules_url: DEFAULT_RULES_URL.to_string(),
            rules_file: None,
            hosts_file: None,
            hosts_url: DEFAULT_HOSTS_URLS
                .iter()
                .map(|url| (*url).to_string())
                .collect(),
            cache_dir: default_cache_dir(),
            max_age: DEFAULT_MAX_AGE,
            tick: DEFAULT_TICK,
            stats_interval: DEFAULT_STATS_INTERVAL,
            fetch_program: crate::fetcher::DEFAULT_PROGRAM.to_string(),
            fetch_timeout: Duration::from_secs(60),
            check: false,
            offline: false,
            probe: Vec::new(),
            quiet: false,
        }
    }
}

/// Cache location for the invoking user.
///
/// `XDG_CACHE_HOME` when it is set, which is also where Android's `filesDir`
/// equivalent belongs once the daemon runs inside an app.
fn default_cache_dir() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(base) if !base.is_empty() => PathBuf::from(base).join("watt"),
        _ => match std::env::var_os("HOME") {
            Some(home) if !home.is_empty() => PathBuf::from(home).join(".cache").join("watt"),
            _ => PathBuf::from("watt-cache"),
        },
    }
}

impl Options {
    /// Parse the command line. `Err` carries a message meant for a human.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut options = Options::default();
        let mut args = args.into_iter();

        // Whether the operator named a hosts URL themselves. The default list is
        // dropped on the first explicit `--hosts-url`, so "name your sources"
        // means exactly that.
        let mut explicit_hosts_url = false;

        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--tun" => options.tun = text(&mut args, "--tun")?,
                "--address" => {
                    let raw = text(&mut args, "--address")?;
                    let (address, prefix_len) = parse_address(&raw)?;
                    options.address = address;
                    options.prefix_len = prefix_len.unwrap_or(options.prefix_len);
                }
                "--prefix" => options.prefix_len = number(&mut args, "--prefix")?,
                "--mtu" => options.mtu = number(&mut args, "--mtu")?,
                "--proxy-listen" => options.proxy_listen = Some(text(&mut args, "--proxy-listen")?),
                "--override" => {
                    let raw = text(&mut args, "--override")?;
                    options.overrides.push(parse_override(&raw)?);
                }
                // Takes a value rather than being a flag with an optional one: an
                // optional value cannot be told apart from the next flag without
                // lookahead, and `default` says what a bare flag would have meant.
                "--protect-mark" => {
                    let raw = text(&mut args, "--protect-mark")?;
                    options.protect_mark = Some(parse_mark(&raw)?);
                }

                "--rules-url" => options.rules_url = text(&mut args, "--rules-url")?,
                "--rules-file" => {
                    options.rules_file = Some(PathBuf::from(text(&mut args, "--rules-file")?));
                }
                "--hosts-file" => {
                    options.hosts_file = Some(PathBuf::from(text(&mut args, "--hosts-file")?));
                }
                // Appends, unlike `--rules-url`, so more than one source can be
                // named (last-wins would make that impossible). The first
                // explicit `--hosts-url` also drops the built-in default list:
                // naming sources is a request to use *those* sources, not
                // those-plus-the-default.
                "--hosts-url" => {
                    if !explicit_hosts_url {
                        explicit_hosts_url = true;
                        options.hosts_url.clear();
                    }
                    options.hosts_url.push(text(&mut args, "--hosts-url")?);
                }
                "--cache-dir" => {
                    options.cache_dir = PathBuf::from(text(&mut args, "--cache-dir")?);
                }
                "--max-age" => options.max_age = secs(&mut args, "--max-age")?,
                "--tick" => options.tick = secs(&mut args, "--tick")?,
                "--stats-interval" => {
                    options.stats_interval = secs(&mut args, "--stats-interval")?;
                }

                "--fetch-program" => options.fetch_program = text(&mut args, "--fetch-program")?,
                "--fetch-timeout" => options.fetch_timeout = secs(&mut args, "--fetch-timeout")?,

                "--check" => options.check = true,
                "--offline" => options.offline = true,
                "--probe" => options.probe.push(text(&mut args, "--probe")?),
                "--quiet" => options.quiet = true,
                "--help" | "-h" => return Err(String::new()),
                other => return Err(format!("unrecognised argument {other:?}")),
            }
        }

        // More than one source is not a mistake — it is the recommended setup.
        // The upstream publishes two *rule sets* that are not subsets of each
        // other (now at the short paths `/1` and `/2`; the old `/rules` and
        // `/hosts?all=1` are retired). Merged they are strictly better than
        // either alone, so naming both is allowed and the documents are unioned
        // per domain. `watt_rules::merge_documents` carries the measured numbers.
        if options.rules_file.is_some() && options.offline {
            // Not contradictory, but the combination suggests a misunderstanding:
            // a pinned document is already offline.
            return Err("--rules-file and --offline both mean \"do not fetch\"; drop one".to_string());
        }

        // A pinned document means "use exactly this": drop the default network
        // sources so a run that names a file does not also phone home. An
        // explicit `--hosts-url` alongside a file is honoured (the file is added
        // on top), because that is an unambiguous request for both.
        if !explicit_hosts_url && (options.rules_file.is_some() || options.hosts_file.is_some()) {
            options.hosts_url.clear();
        }

        Ok(options)
    }
}

/// `22356`, `0x5754`, or the word `default`.
///
/// Hex is accepted because the value has to be written into an `ip rule` by hand,
/// and those are conventionally written in hex.
fn parse_mark(raw: &str) -> Result<u32, String> {
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("default") {
        return Ok(watt_stack::DEFAULT_MARK);
    }
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16)
            .map_err(|err| format!("--protect-mark {raw:?}: {err}"));
    }
    raw.parse()
        .map_err(|err| format!("--protect-mark {raw:?}: {err}"))
}

/// `<addr>[:port]=<addr>[:port]`, the two halves being a match and a target.
///
/// A port is optional on both sides, and its absence means "every port" when
/// matching and "keep the port that was asked for" when targeting. That is the
/// difference between redirecting a service and redirecting a host.
fn parse_override(raw: &str) -> Result<DestinationOverride, String> {
    let (left, right) = raw.split_once('=').ok_or_else(|| {
        format!("--override {raw:?}: expected <addr>[:port]=<addr>[:port]")
    })?;

    let (match_addr, match_port) = parse_endpoint(left, raw)?;
    let (target_addr, target_port) = parse_endpoint(right, raw)?;

    Ok(DestinationOverride {
        match_addr,
        match_port,
        target_addr,
        target_port,
    })
}

/// One side of an override.
///
/// A bare literal is tried as a whole address first. `::1` and `2001:db8::1` are
/// valid addresses whose last colon must not be read as a port separator, and no
/// amount of inspecting the text tells the two cases apart — only parsing does.
/// Once that has failed, the bracketed form and the v4 `addr:port` form are the
/// remaining possibilities.
fn parse_endpoint(raw: &str, whole: &str) -> Result<(IpAddr, Option<u16>), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("--override {whole:?}: one side is empty"));
    }

    if let Ok(address) = raw.parse::<IpAddr>() {
        return Ok((address, None));
    }

    let (address, port) = if let Some(rest) = raw.strip_prefix('[') {
        let (address, tail) = rest
            .split_once(']')
            .ok_or_else(|| format!("--override {whole:?}: unbalanced '['"))?;
        let port = match tail {
            "" => None,
            _ => Some(
                tail.strip_prefix(':')
                    .ok_or_else(|| format!("--override {whole:?}: expected ':' after ']'"))?,
            ),
        };
        (address, port)
    } else {
        let (address, port) = raw
            .rsplit_once(':')
            .ok_or_else(|| format!("--override {whole:?}: expected <addr>[:port]"))?;
        (address, Some(port))
    };

    let address: IpAddr = address
        .parse()
        .map_err(|err| format!("--override {whole:?}: {address:?} is not an address: {err}"))?;

    let port = match port {
        Some(port) => Some(
            port.parse()
                .map_err(|err| format!("--override {whole:?}: {port:?} is not a port: {err}"))?,
        ),
        None => None,
    };

    Ok((address, port))
}

/// `1.2.3.4` or `1.2.3.4/15`.
fn parse_address(raw: &str) -> Result<(IpAddr, Option<u8>), String> {
    match raw.split_once('/') {
        Some((address, prefix)) => {
            let address = address
                .parse()
                .map_err(|err| format!("--address {address:?}: {err}"))?;
            let prefix: u8 = prefix
                .parse()
                .map_err(|err| format!("--address prefix {prefix:?}: {err}"))?;
            Ok((address, Some(prefix)))
        }
        None => {
            let address = raw
                .parse()
                .map_err(|err| format!("--address {raw:?}: {err}"))?;
            Ok((address, None))
        }
    }
}

fn text<I: Iterator<Item = String>>(args: &mut I, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn number<T, I>(args: &mut I, flag: &str) -> Result<T, String>
where
    I: Iterator<Item = String>,
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = text(args, flag)?;
    raw.parse().map_err(|err| format!("{flag}: {err}"))
}

fn secs<I: Iterator<Item = String>>(args: &mut I, flag: &str) -> Result<Duration, String> {
    let seconds: u64 = number(args, flag)?;
    Ok(Duration::from_secs(seconds))
}

/// The help text, which is also what an empty `Err` from [`Options::parse`] means.
pub fn usage() -> String {
    let default_mark = watt_stack::DEFAULT_MARK;
    format!(
        "watt-daemon — full-traffic relay kernel\n\
         \n\
         usage: watt-daemon [options]\n\
         \n\
         Rules:\n\
           --hosts-url <url>        hosts-format rule source; repeatable. Default\n\
                                    is /1 (UsbEAm) only; add /2 only if you run a\n\
                                    loopback reverse proxy -- its addresses are all\n\
                                    127.0.0.1 and rules may not relay loopback\n\
           --hosts-file <path>      compile this hosts document instead, never fetching\n\
           --rules-url <url>        a JSON (`groups` shape) endpoint. Empty by\n\
                                    default: the upstream retired the only public\n\
                                    one, so the JSON lane is cache-or-builtin unless\n\
                                    an upstream aggregator is named here\n\
           --rules-file <path>      compile this JSON document instead, never fetching\n\
           --cache-dir <path>       where the downloaded copy is kept\n\
           --max-age <seconds>      age at which the cached copy is stale (default: 21600)\n\
           --tick <seconds>         how often staleness is checked (default: 60)\n\
           --fetch-program <path>   program used to fetch (default: curl)\n\
           --fetch-timeout <secs>   give up on a fetch after this long (default: 60)\n\
         \n\
         Tunnel:\n\
           --tun <name>             interface to create (default: watt0)\n\
           --address <ip[/prefix]>  address the interface claims (default: 198.18.0.1/15)\n\
           --prefix <n>             prefix length, if not given with --address\n\
           --mtu <n>                interface MTU (default: {DEFAULT_MTU})\n\
           --proxy-listen <addr>     HTTP CONNECT listener; no TUN or root required\n\
                                    (example: 127.0.0.1:1080)\n\
           --override <a[:p]=a[:p]> rewrite a destination before the rules see it;\n\
                                    repeatable. e.g. 203.0.113.10:443=127.0.0.1:8443\n\
           --protect-mark <n>       mark the kernel's own sockets so the host can\n\
                                    route them outside the tunnel. <n> is a decimal\n\
                                    or 0x-prefixed value, or the word 'default'\n\
                                    (0x{default_mark:x}). Needs CAP_NET_ADMIN and a\n\
                                    matching `ip rule`. Without it the kernel's\n\
                                    upstream traffic is captured by its own routes.\n\
         \n\
         Running:\n\
           --check                  report on the rules and exit, needs no root\n\
           --probe <domain>         look this up in the report; repeatable\n\
           --offline                never fetch, use cache or builtin rules\n\
           --quiet                  only log the periodic counters\n\
           --stats-interval <secs>  how often those are written (default: 300)\n\
           --help                   this text\n\
         \n\
         Signals:\n\
           SIGUSR1                  refresh the rules now\n\
           SIGINT, SIGTERM          stop\n\
         \n\
         Creating a TUN device needs CAP_NET_ADMIN, so running the tunnel needs\n\
         root. The interface still has to be addressed and routed by whoever\n\
         started it: the daemon deliberately does not shell out to `ip`."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn defaults_are_usable() {
        let options = parse(&[]).unwrap();
        assert_eq!(options.tun, "watt0");
        assert_eq!(options.prefix_len, 15);
        // Not just "equals the constant": the constant itself has to be a value
        // that does not silently produce a doomed fetch. An empty `rules_url`
        // means the JSON lane is skipped (see `run`), and the hosts defaults must
        // be non-empty or a bare run would load the built-in set only.
        assert_eq!(options.rules_url, "");
        assert!(
            !options.hosts_url.is_empty(),
            "a bare run must have at least one live source, not just builtin"
        );
        assert!(
            options.hosts_url.iter().all(|url| url.contains("://")),
            "default hosts sources must be network URLs, not empty strings: {:?}",
            options.hosts_url
        );
        assert_eq!(
            options.hosts_url,
            DEFAULT_HOSTS_URLS
                .iter()
                .map(|url| url.to_string())
                .collect::<Vec<_>>(),
            "a bare run must fetch exactly the default sources"
        );
        assert_eq!(options.max_age, DEFAULT_MAX_AGE);
        assert!(!options.check);
        assert!(!options.offline);
    }

    #[test]
    fn no_default_source_is_the_loopback_hijack_block() {
        // Anti-regression guard for a mistake that type-checks and looks helpful:
        // adding `/2` (the Steamcommunity 302 block) to the defaults. Every one
        // of its 862 addresses is `127.0.0.1`, and `Planner::can_relay` refuses a
        // non-overridden loopback target, so a domain that /2 names *only* is
        // reset rather than sent direct. Measured: 675 merged domains are
        // loopback-only and all 675 are /2-only. The URL is named explicitly here
        // so this test fails the day someone adds it back to `DEFAULT_HOSTS_URLS`.
        assert!(
            !DEFAULT_HOSTS_URLS
                .iter()
                .any(|url| url.ends_with("/2") || url.contains("/2?")),
            "the S302 hijack block must not be a default source: {:?}",
            DEFAULT_HOSTS_URLS
        );
        // And the parse path inherits it.
        let options = parse(&[]).unwrap();
        assert!(
            !options.hosts_url.iter().any(|url| url.ends_with("/2")),
            "a bare run must not fetch the hijack block: {:?}",
            options.hosts_url
        );
    }

    #[test]
    fn hosts_urls_accumulate_so_more_than_one_source_can_be_named() {
        // Last-wins would make naming more than one source impossible; repeating
        // the flag therefore accumulates...
        let options = parse(&[
            "--hosts-url",
            "https://a.test/1",
            "--hosts-url",
            "https://b.test/2",
        ])
        .unwrap();
        // ...and the first explicit flag drops the built-in defaults, so naming
        // sources means exactly those sources.
        assert_eq!(options.hosts_url, vec!["https://a.test/1", "https://b.test/2"]);
    }

    #[test]
    fn a_pinned_document_drops_the_default_hosts_sources() {
        // `--rules-file` means "compile this, never fetch"; it must not also
        // pull the default network hosts sources.
        let options = parse(&["--rules-file", "/tmp/rules.json"]).unwrap();
        assert!(options.hosts_url.is_empty(), "{:?}", options.hosts_url);

        let options = parse(&["--hosts-file", "/tmp/hosts.txt"]).unwrap();
        assert!(options.hosts_url.is_empty(), "{:?}", options.hosts_url);

        // But an explicit hosts URL alongside a file is honoured: the file is
        // added on top of the network sources.
        let options = parse(&[
            "--hosts-file",
            "/tmp/hosts.txt",
            "--hosts-url",
            "https://a.test/1",
        ])
        .unwrap();
        assert_eq!(options.hosts_url, vec!["https://a.test/1"]);
    }

    #[test]
    fn rules_url_is_last_wins_because_json_documents_are_not_complementary() {
        let options = parse(&[
            "--rules-url",
            "https://a.test/rules",
            "--rules-url",
            "https://b.test/rules",
        ])
        .unwrap();
        assert_eq!(options.rules_url, "https://b.test/rules");
    }

    #[test]
    fn an_address_may_carry_its_prefix() {
        let options = parse(&["--address", "10.0.0.1/24"]).unwrap();
        assert_eq!(options.address.to_string(), "10.0.0.1");
        assert_eq!(options.prefix_len, 24);

        let options = parse(&["--address", "10.0.0.1", "--prefix", "16"]).unwrap();
        assert_eq!(options.prefix_len, 16);
    }

    #[test]
    fn probes_accumulate() {
        let options = parse(&["--probe", "a.example", "--probe", "b.example"]).unwrap();
        assert_eq!(options.probe, vec!["a.example", "b.example"]);
    }

    #[test]
    fn a_missing_value_is_an_error_not_a_panic() {
        assert!(parse(&["--tun"]).is_err());
        assert!(parse(&["--max-age", "soon"]).is_err());
        assert!(parse(&["--address", "not-an-address"]).is_err());
    }

    #[test]
    fn contradictory_flags_are_refused() {
        let err = parse(&["--rules-file", "/tmp/r.json", "--offline"]).unwrap_err();
        assert!(err.contains("drop one"), "{err}");
    }

    #[test]
    fn help_is_signalled_by_an_empty_error() {
        assert_eq!(parse(&["--help"]).unwrap_err(), "");
        assert!(usage().contains("SIGUSR1"));
    }

    #[test]
    fn overrides_accumulate_and_carry_ports() {
        let options = parse(&[
            "--override",
            "203.0.113.10:443=127.0.0.1:8443",
            "--override",
            "203.0.113.11=127.0.0.1",
        ])
        .unwrap();

        assert_eq!(options.overrides.len(), 2);

        let first = &options.overrides[0];
        assert_eq!(first.match_addr.to_string(), "203.0.113.10");
        assert_eq!(first.match_port, Some(443));
        assert_eq!(first.target_addr.to_string(), "127.0.0.1");
        assert_eq!(first.target_port, Some(8443));

        // No port on either side: every port matches, and the port the client
        // asked for is the port used.
        let second = &options.overrides[1];
        assert_eq!(second.match_port, None);
        assert_eq!(second.target_port, None);
        assert_eq!(second.apply("203.0.113.11".parse().unwrap(), 9000), Some((
            "127.0.0.1".parse().unwrap(),
            9000
        )));
    }

    #[test]
    fn an_override_without_an_equals_sign_is_refused() {
        let err = parse(&["--override", "203.0.113.10"]).unwrap_err();
        assert!(err.contains("expected"), "{err}");
    }

    #[test]
    fn ipv6_literals_keep_their_colons() {
        // A bare literal has colons but no port; a bracketed one has both. Reading
        // the last colon as a port separator would turn `::1` into `:` plus `1`.
        let options = parse(&[
            "--override",
            "::1=[2001:db8::1]:443",
            "--override",
            "2001:db8::2=[::1]",
        ])
        .unwrap();

        assert_eq!(options.overrides[0].match_addr.to_string(), "::1");
        assert_eq!(options.overrides[0].match_port, None);
        assert_eq!(options.overrides[0].target_addr.to_string(), "2001:db8::1");
        assert_eq!(options.overrides[0].target_port, Some(443));

        assert_eq!(options.overrides[1].match_addr.to_string(), "2001:db8::2");
        assert_eq!(options.overrides[1].target_addr.to_string(), "::1");
        assert_eq!(options.overrides[1].target_port, None);
    }

    #[test]
    fn a_bad_override_names_the_offending_side() {
        // No colon at all: the shape itself is wrong, so say that.
        assert!(parse(&["--override", "not-an-addr=127.0.0.1"])
            .unwrap_err()
            .contains("expected <addr>"));
        // A colon, so the shape is right and the address is what is wrong.
        assert!(parse(&["--override", "not-an-addr:443=127.0.0.1"])
            .unwrap_err()
            .contains("is not an address"));
        assert!(parse(&["--override", "203.0.113.10:99999=127.0.0.1"])
            .unwrap_err()
            .contains("is not a port"));
        assert!(parse(&["--override", "=[::1]"])
            .unwrap_err()
            .contains("one side is empty"));
        assert!(parse(&["--override", "[::1=127.0.0.1"])
            .unwrap_err()
            .contains("unbalanced"));
    }
}
