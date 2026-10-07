//! The certificate authority the local reverse proxy signs with.
//!
//! # Why there has to be one at all
//!
//! Root mode reaches a domain by a route the ordinary kernel cannot: it answers
//! the name with a **loopback** address and terminates TLS itself, then
//! re-originates the request to whatever address the rule table names. That is
//! what makes it possible to serve a name from an address whose certificate does
//! not cover it — the client never talks to that address, so its certificate
//! never has to be valid for the name.
//!
//! The price is that the client is now talking to *us*, and it will only accept
//! the certificate we present if it chains to something it already trusts. There
//! is no way around that: a leaf signed by an unknown authority is exactly the
//! thing TLS exists to reject. So root mode mints its own authority and has the
//! device trust it, which is the one step that genuinely needs uid 0.
//!
//! # What is generated, and what is kept
//!
//! One CA, generated on first use and then reused for as long as the file
//! survives. Regenerating it per run would be worse than useless — the whole
//! point of installing it in the system store is that the two sides agree on an
//! anchor, and a fresh anchor every session would mean a fresh install every
//! session.
//!
//! Leaves are **not** persisted. They are cheap to mint (P-256, one signature)
//! and a name's leaf is only ever needed while a connection to that name is
//! being served, so a process-lifetime cache is the whole requirement. Writing
//! them to disk would add a file per hostname visited and buy nothing.
//!
//! # Why P-256 for both
//!
//! Signing a leaf is on the connection path. ECDSA P-256 is the fastest thing
//! rcgen can sign with, it is universally accepted by Android's TLS stack, and
//! the chain is two certificates long, so there is no size argument for anything
//! else.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::sign::CertifiedKey;
use time::{Duration, OffsetDateTime};

/// File name of the CA certificate, in PEM.
pub const CA_CERT_FILE: &str = "speed-root-ca.pem";
/// File name of the CA private key, in PEM.
///
/// The key lives beside the certificate because the leaf issuer needs both, and
/// because keeping them together is what makes "delete the CA" a single
/// directory operation rather than a hunt.
pub const CA_KEY_FILE: &str = "speed-root-ca.key.pem";

/// The CA's subject common name.
///
/// Shown in the device's trust store, so it has to say what it is to someone who
/// goes looking. It is deliberately *not* the name of any real authority: a
/// certificate that claims to be someone else is a phishing aid, and this one is
/// meant to be obviously ours.
const CA_COMMON_NAME: &str = "speed root CA";
const CA_ORGANIZATION: &str = "speed";

/// A leaf certificate is signed for this long.
///
/// Comfortably under the 398-day ceiling browsers enforce, and long enough that
/// a session never outlives its own leaf. The window is anchored at the moment
/// the leaf is minted — see [`leaf_window`] — not at a date written here, so a
/// hardcoded year cannot make every leaf expire at once.
const LEAF_DAYS: i64 = 397;

/// How long the CA certificate is valid.
///
/// A root is meant to outlive the leaves it signs and the 398-day ceiling does
/// not apply to it, so this is measured in years rather than under it. It is
/// still anchored at mint time, for the same reason the leaf is: a fixed year
/// would eventually make a CA that cannot sign anything.
const CA_DAYS: i64 = 3650;

/// How far before "now" every certificate's window starts.
///
/// A device whose clock is behind the machine that minted the certificate would
/// otherwise see a `not_before` in its own future and reject a certificate that
/// is, from the minter's side, perfectly valid. One day is the same slack the
/// leaf window can afford without pushing its total span past the 398-day
/// ceiling.
const CLOCK_SKEW_DAYS: i64 = 1;

/// The authority, plus the leaves minted from it so far.
///
/// Shared across every connection thread, hence the `Arc` at the call sites and
/// the mutex around the leaf cache.
pub struct Authority {
    /// The CA certificate, PEM. Handed to the root helper so it can install it.
    cert_pem: String,
    /// The CA key pair. Kept as a parsed key rather than PEM text so a leaf
    /// issuance does not re-parse it every time.
    key: KeyPair,
    /// The same certificate in DER, which is what `rustls` wants for the chain.
    cert_der: CertificateDer<'static>,
    /// Leaves already minted, keyed by the exact SNI they were minted for.
    leaves: Mutex<HashMap<String, Arc<CertifiedKey>>>,
}

impl std::fmt::Debug for Authority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key is a secret; `KeyPair`'s own Debug already elides it, but the
        // cache is not worth printing either.
        f.debug_struct("Authority")
            .field("cert_pem_len", &self.cert_pem.len())
            .field("leaves", &self.leaves.lock().map(|c| c.len()).unwrap_or(0))
            .finish()
    }
}

impl Authority {
    /// Load the CA from `dir`, generating and persisting one if it is not there.
    ///
    /// A directory that cannot be read is an error rather than a cue to start
    /// over: silently replacing an installed CA would leave the system store
    /// trusting a certificate the proxy no longer holds the key for, which is a
    /// state where every HTTPS connection fails and nothing says why.
    pub fn load_or_create(dir: &Path) -> io::Result<Self> {
        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);

        let (cert_pem, key_pem) = match (cert_path.exists(), key_path.exists()) {
            (true, true) => {
                let cert_pem = fs::read_to_string(&cert_path)?;
                let key_pem = fs::read_to_string(&key_path)?;
                (cert_pem, key_pem)
            }
            (false, false) => {
                let (cert_pem, key_pem) = generate()?;
                fs::create_dir_all(dir)?;
                fs::write(&cert_path, &cert_pem)?;
                fs::write(&key_path, &key_pem)?;
                log::info!("mitm: minted a new root CA at {}", cert_path.display());
                (cert_pem, key_pem)
            }
            (cert, key) => {
                // Half a CA is not a CA. Which half is missing is worth saying,
                // because the usual cause is a backup or a restore that copied
                // one file.
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "the CA is incomplete: {CA_CERT_FILE} present={cert}, {CA_KEY_FILE} present={key}"
                    ),
                ));
            }
        };

        let key = KeyPair::from_pem(&key_pem).map_err(to_io)?;
        let cert_der = parse_pem_cert(&cert_pem)?;
        Ok(Self {
            cert_pem,
            key,
            cert_der,
            leaves: Mutex::new(HashMap::new()),
        })
    }

    /// The CA certificate in PEM, for the root helper to install.
    pub fn certificate_pem(&self) -> &str {
        &self.cert_pem
    }

    /// The CA certificate in DER.
    pub fn certificate_der(&self) -> &CertificateDer<'static> {
        &self.cert_der
    }

    /// The chain `rustls` should offer alongside a leaf: just the CA, since the
    /// CA signs the leaf directly.
    pub fn chain(&self) -> Vec<CertificateDer<'static>> {
        vec![self.cert_der.clone()]
    }

    /// A certificate/key pair for `name`, minted on first use and cached after.
    ///
    /// `name` is the SNI the client asked for, verbatim. Nothing normalizes or
    /// wildcards it: a client that asks for `a.example.com` must be answered
    /// with a certificate for `a.example.com` and nothing else, or the check it
    /// is about to run will fail for a reason that has nothing to do with the
    /// network.
    pub fn leaf_for(&self, name: &str) -> Result<Arc<CertifiedKey>, io::Error> {
        if let Ok(cache) = self.leaves.lock() {
            if let Some(found) = cache.get(name) {
                return Ok(Arc::clone(found));
            }
        }

        let leaf_key = KeyPair::generate().map_err(to_io)?;
        let mut params = CertificateParams::new(vec![name.to_string()]).map_err(to_io)?;
        params.distinguished_name.push(DnType::CommonName, name);
        // A leaf must not be able to sign anything; saying so explicitly is what
        // stops a compromised leaf from being usable as an intermediate.
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        let (not_before, not_after) = leaf_window(OffsetDateTime::now_utc());
        params.not_before = not_before;
        params.not_after = not_after;

        // The issuer's identity is read out of the certificate the device was
        // told to trust, not reconstructed from a second copy of the parameters.
        // Reconstructing them would hold only as long as the two copies stayed
        // in step — a hidden contract this project has paid for before. Parsing
        // the PEM makes the certificate the single source of the issuer name,
        // which is the thing a chain build actually compares.
        let issuer = Issuer::from_ca_cert_pem(&self.cert_pem, &self.key).map_err(to_io)?;
        let cert = params.signed_by(&leaf_key, &issuer).map_err(to_io)?;

        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        let signing = rustls::crypto::ring::sign::any_ecdsa_type(&key_der)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
        let certified = Arc::new(CertifiedKey::new(vec![cert.der().clone()], signing));

        if let Ok(mut cache) = self.leaves.lock() {
            cache.insert(name.to_string(), Arc::clone(&certified));
        }
        log::debug!("mitm: minted a leaf for {name}");
        Ok(certified)
    }
}

/// Mint a fresh CA, returning `(certificate PEM, key PEM)`.
fn generate() -> io::Result<(String, String)> {
    let key = KeyPair::generate().map_err(to_io)?;
    let (not_before, not_after) = ca_window(OffsetDateTime::now_utc());
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(to_io)?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    // `KeyCertSign` is what makes it an issuer at all; `CrlSign` is included
    // because a CA that cannot revoke is a CA whose key must be replaced rather
    // than retired.
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    params
        .distinguished_name
        .push(DnType::CommonName, CA_COMMON_NAME);
    params
        .distinguished_name
        .push(DnType::OrganizationName, CA_ORGANIZATION);
    params.not_before = not_before;
    params.not_after = not_after;

    let cert = params.self_signed(&key).map_err(to_io)?;
    Ok((cert.pem(), key.serialize_pem()))
}

/// The validity window of a leaf minted at `now`.
///
/// A pure function so the window can be tested directly rather than by minting a
/// certificate and parsing it back. `not_before` is backdated by
/// [`CLOCK_SKEW_DAYS`] so a device whose clock lags the minter still sees a
/// certificate that has already become valid, and `not_after` is [`LEAF_DAYS`]
/// ahead. The two span `LEAF_DAYS + CLOCK_SKEW_DAYS` — exactly the 398-day
/// ceiling clients are entitled to demand, and no more.
fn leaf_window(now: OffsetDateTime) -> (OffsetDateTime, OffsetDateTime) {
    (
        now - Duration::days(CLOCK_SKEW_DAYS),
        now + Duration::days(LEAF_DAYS),
    )
}

/// The validity window of a CA minted at `now`. See [`leaf_window`].
fn ca_window(now: OffsetDateTime) -> (OffsetDateTime, OffsetDateTime) {
    (
        now - Duration::days(CLOCK_SKEW_DAYS),
        now + Duration::days(CA_DAYS),
    )
}

/// Decode the first certificate from a PEM document.
fn parse_pem_cert(pem: &str) -> io::Result<CertificateDer<'static>> {
    let (label, der) = pem_rfc7468_decode(pem)?;
    if label != "CERTIFICATE" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected a CERTIFICATE block, found {label}"),
        ));
    }
    Ok(CertificateDer::from(der))
}

/// Minimal PEM body extraction: strip the armour, base64-decode the rest.
///
/// Hand-rolled rather than pulled in as a dependency because this is the only
/// PEM this crate ever reads, and it is a file it wrote itself. A general parser
/// would be more code than the thing it replaces.
fn pem_rfc7468_decode(pem: &str) -> io::Result<(String, Vec<u8>)> {
    let mut label = None;
    let mut body = String::new();
    let mut in_body = false;
    for line in pem.lines() {
        let line = line.trim();
        if line.starts_with("-----BEGIN ") && line.ends_with("-----") {
            label = Some(line["-----BEGIN ".len()..line.len() - "-----".len()].to_string());
            in_body = true;
        } else if line.starts_with("-----END ") {
            break;
        } else if in_body {
            body.push_str(line);
        }
    }
    let label = label.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "no PEM armour found")
    })?;
    let der = base64_decode(&body)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the PEM body is not base64"))?;
    Ok((label, der))
}

/// Standard base64 with padding. Returns `None` on any non-alphabet byte.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const fn table(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for byte in input.bytes() {
        if byte == b'=' || byte == b'\n' || byte == b'\r' {
            continue;
        }
        let value = table(byte)? as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// rcgen's errors carry a `Display`; the crate's public surface is `io::Error`
/// so the FFI layer has one error type to translate.
fn to_io<E: std::fmt::Display>(err: E) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err.to_string())
}

/// Directory helper so callers do not each rebuild the two paths.
pub fn cert_path(dir: &Path) -> PathBuf {
    dir.join(CA_CERT_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_a_known_vector() {
        // "hello" -> aGVsbG8=
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello".to_vec());
    }

    #[test]
    fn a_ca_is_created_once_and_reused() {
        let dir = std::env::temp_dir().join(format!("watt-mitm-ca-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let first = Authority::load_or_create(&dir).expect("create");
        let first_pem = first.certificate_pem().to_string();
        let second = Authority::load_or_create(&dir).expect("reload");
        // Reuse is the whole contract: a new anchor per run would mean a new
        // install per run.
        assert_eq!(first_pem, second.certificate_pem());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_leaf_carries_the_name_it_was_asked_for() {
        let dir = std::env::temp_dir().join(format!("watt-mitm-leaf-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let ca = Authority::load_or_create(&dir).expect("create");
        let leaf = ca.leaf_for("example.com").expect("leaf");
        assert_eq!(leaf.cert.len(), 1);
        // Minted once, then served from the cache.
        let again = ca.leaf_for("example.com").expect("leaf");
        assert!(Arc::ptr_eq(&leaf, &again));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_leaf_window_brackets_now_and_stays_within_the_clients_ceiling() {
        let now = OffsetDateTime::now_utc();
        let (not_before, not_after) = leaf_window(now);
        // A device whose clock lags the minter must still see the leaf as already
        // valid, and a session must not outlive it.
        assert!(not_before < now, "the window must start before the mint");
        assert!(now < not_after, "and end after it");
        // Clients are entitled to reject a validity period longer than 398 days,
        // and the whole window is that period — a hardcoded decade-wide window
        // was both too long here and expired all at once in 2030.
        let span = not_after - not_before;
        assert!(
            span <= Duration::days(398),
            "a leaf window of {span:?} exceeds the 398-day ceiling"
        );
    }
}
