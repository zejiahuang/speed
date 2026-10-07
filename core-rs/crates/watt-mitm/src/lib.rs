//! Root-mode TLS interception: a local reverse proxy with an authority of its
//! own.
//!
//! # Where this sits
//!
//! Speed has two ways to reach a name today, and neither of them terminates TLS.
//! The tunnel carries packets and the no-root proxy carries bytes; both leave the
//! client's TLS connection intact, which is what keeps them from needing a
//! certificate authority at all. The cost of that restraint is a hard limit: a
//! name can only be served from an address whose certificate covers it, because
//! the client — not us — is the one checking.
//!
//! Root mode is the deliberate exception. It terminates the client's TLS
//! connection, presents a certificate minted on the spot for the name that was
//! asked for, and then opens a *second* TLS connection to whatever address the
//! rule table names. The client's certificate check is then satisfied by us and
//! the origin's certificate stops being a constraint on routing.
//!
//! # The three pieces, and which one needs root
//!
//! 1. **Names are rewritten to a loopback address** in the system hosts file.
//!    This is what makes traffic arrive here without any client cooperating: it
//!    is a name-resolution change, not a proxy setting, so a client that ignores
//!    its proxy configuration — or speaks QUIC, or is a native app with no proxy
//!    support at all — is still covered.
//! 2. **A certificate authority is installed in the system trust store.** The
//!    client will only accept the leaf we present if it chains to something the
//!    device already trusts, and no amount of cleverness changes that.
//! 3. **This proxy runs inside the app process.** It needs no privilege of its
//!    own; it is reachable only because of (1).
//!
//! Only (1) and (2) need uid 0. That is the whole reason root mode is a mode
//! rather than the default: two file writes outside the app's own storage.
//!
//! # What root mode gives up
//!
//! The origin is no longer authenticated by TLS. The second connection does not
//! verify the certificate it is offered — it cannot, since reaching an address
//! whose certificate does not cover the name is the entire purpose. Inside this
//! proxy's traffic the origin is trusted because the rule table said to trust it,
//! and for no other reason. See [`proxy`] for the full argument.
//!
//! # Layout
//!
//! - [`ca`] mints and caches the authority and the per-name leaves.
//! - [`proxy`] is the listener: accept, handshake, route, dial, handshake,
//!   relay.

mod ca;
mod proxy;

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use watt_rules::RuleSet;

pub use crate::ca::{cert_path, Authority, CA_CERT_FILE, CA_KEY_FILE};
pub use crate::proxy::{serve_until, start, Proxy, Stats, StatsSnapshot};

/// The address intercepted names are rewritten to in the system hosts file.
///
/// Deliberately **not** `127.0.0.1`. Rewriting to `127.0.0.1` would make every
/// rewritten name indistinguishable from a genuine connection to a local
/// service, so the redirect that carries the traffic to this proxy would also
/// capture those. A dedicated loopback address keeps the blast radius to the
/// names the rewrite actually covers.
///
/// This is exported because the root helper has to write exactly this address
/// into hosts, and the two sides agreeing on it is not something either can
/// check at runtime.
pub const HOSTS_ADDRESS: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);

/// The port intercepted connections arrive on, before any redirect.
pub const INTERCEPT_PORT: u16 = 443;

/// Bind `bind`, serve `rules`, and sign with the authority kept in `ca_dir`.
///
/// The authority is loaded if `ca_dir` already holds one and minted if it does
/// not, so the certificate stays the same across restarts — which matters,
/// because the device has been told to trust it and a fresh anchor every run
/// would mean a fresh install every run.
///
/// The authority is returned beside the proxy rather than kept inside it because
/// the caller is the one that has to install its certificate, and the PEM it
/// needs is only reachable through the authority.
pub fn start_with_ca(
    bind: SocketAddr,
    rules: RuleSet,
    ca_dir: &Path,
) -> io::Result<(Proxy, Arc<Authority>)> {
    let ca = Arc::new(Authority::load_or_create(ca_dir)?);
    let proxy = start(bind, rules, Arc::clone(&ca))?;
    Ok((proxy, ca))
}
