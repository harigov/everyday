//! A certificate this server made for itself, and the fingerprint that makes
//! it trustworthy.
//!
//! There is no certificate authority in this story and there should not be. A
//! self-hoster on a Tailscale network has no public name to get a certificate
//! for, and requiring one would make "share this vault" a task with a
//! prerequisite. So the server signs its own, and the client is told the
//! *fingerprint* out of band -- in the pairing URL, which the person carried
//! from one screen to the other. A client trusts exactly that certificate and
//! nothing else, which is a stronger promise than the public web makes: no
//! authority can issue a certificate this client would accept.
//!
//! # The names in it are stable, and deliberately not the point
//!
//! A certificate normally has to name every address it might be reached at,
//! and that is unworkable here: a machine's addresses change when it joins a
//! different wifi or a VPN comes up, and regenerating the certificate to cover
//! a new one changes its fingerprint -- which unpairs every device that pinned
//! it. A vault that stopped answering because somebody moved rooms would be a
//! bad feature.
//!
//! So the SANs are a fixed set that never depends on what the machine's
//! interfaces happen to be, and the client does not check them: it verifies
//! the certificate *itself*, byte for byte, against the fingerprint it was
//! given at pairing time. That is a stronger check than a name, not a weaker
//! one -- a name says "something an authority vouched for", and a pin says
//! "this exact key". See `client::pinned_client`, which is the other half and
//! says so too.

use everyday_service::error::{CommandError, CommandResult};
use std::net::IpAddr;
use std::path::Path;

pub const CERT_FILE: &str = "server.crt";
pub const KEY_FILE: &str = "server.key";

/// The names the certificate beside it was made for, one per line.
///
/// A sidecar rather than a re-parse of the certificate, because the question --
/// "does this still name every address we are about to answer on?" -- is asked
/// once per start and the answer is something we wrote ourselves. Reading it
/// back out of the DER would mean carrying an X.509 parser to check our own
/// work.
pub const NAMES_FILE: &str = "server.names";

/// A certificate and the key that goes with it, in PEM.
pub struct Identity {
    pub cert_pem: String,
    pub key_pem: String,
    /// Lower-case hex of the SHA-256 of the DER, which is what the pairing URL
    /// carries and what a client pins.
    pub fingerprint: String,
}

impl Identity {
    /// Load the certificate in `dir`, making one if it is not there.
    ///
    /// Regenerated when it no longer covers `names`, because a certificate that
    /// does not name the address somebody is about to use is worse than a new
    /// one: it fails at the handshake, where the error is opaque. Every paired
    /// device has to pair again after that, which is why the name list is built
    /// generously up front.
    pub fn load_or_create(dir: &Path, names: &[String]) -> CommandResult<Self> {
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);

        let names_path = dir.join(NAMES_FILE);
        let wanted = name_list(names);

        if let (Ok(cert_pem), Ok(key_pem), Ok(recorded)) = (
            std::fs::read_to_string(&cert_path),
            std::fs::read_to_string(&key_path),
            std::fs::read_to_string(&names_path),
        ) && recorded.trim() == wanted.trim()
            && let Ok(identity) = Self::from_pem(cert_pem, key_pem)
        {
            return Ok(identity);
        }

        let identity = Self::create(names)?;
        std::fs::create_dir_all(dir)
            .map_err(|e| CommandError::new("io", format!("{}: {e}", dir.display())))?;
        everyday_core::fsutil::write_atomic(&cert_path, identity.cert_pem.as_bytes(), "cert")
            .map_err(|e| CommandError::new("io", e.to_string()))?;
        everyday_core::fsutil::write_atomic(&key_path, identity.key_pem.as_bytes(), "key")
            .map_err(|e| CommandError::new("io", e.to_string()))?;
        restrict(&key_path);
        everyday_core::fsutil::write_atomic(&names_path, wanted.as_bytes(), "names")
            .map_err(|e| CommandError::new("io", e.to_string()))?;
        Ok(identity)
    }

    fn create(names: &[String]) -> CommandResult<Self> {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new())
            .map_err(|e| CommandError::new("internal", e.to_string()))?;
        params.subject_alt_names = names.iter().filter_map(|n| san(n)).collect();
        params.distinguished_name = {
            let mut dn = rcgen::DistinguishedName::new();
            dn.push(rcgen::DnType::CommonName, "Every Day");
            dn
        };
        let key =
            rcgen::KeyPair::generate().map_err(|e| CommandError::new("internal", e.to_string()))?;
        let cert =
            params.self_signed(&key).map_err(|e| CommandError::new("internal", e.to_string()))?;
        Ok(Self {
            fingerprint: fingerprint_of(cert.der()),
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        })
    }

    fn from_pem(cert_pem: String, key_pem: String) -> CommandResult<Self> {
        let der = pem_to_der(&cert_pem)?;
        Ok(Self { fingerprint: fingerprint_of(&der), cert_pem, key_pem })
    }
}

/// The names a certificate was made for, in a stable order.
fn name_list(names: &[String]) -> String {
    let mut names: Vec<&str> = names.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    format!("{}\n", names.join("\n"))
}

fn san(name: &str) -> Option<rcgen::SanType> {
    if let Ok(ip) = name.parse::<IpAddr>() {
        return Some(rcgen::SanType::IpAddress(ip));
    }
    name.to_string().try_into().ok().map(rcgen::SanType::DnsName)
}

/// The first certificate in a PEM file, as DER.
pub fn pem_to_der(pem: &str) -> CommandResult<Vec<u8>> {
    let body: String = pem
        .lines()
        .skip_while(|l| !l.starts_with("-----BEGIN CERTIFICATE"))
        .skip(1)
        .take_while(|l| !l.starts_with("-----END CERTIFICATE"))
        .collect();
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .map_err(|e| CommandError::new("invalid", format!("not a certificate: {e}")))
}

/// A DER certificate as PEM, which is what a client stores and re-reads.
pub fn der_to_pem(der: &[u8]) -> String {
    use base64::Engine;
    let body = base64::engine::general_purpose::STANDARD.encode(der);
    let wrapped: Vec<String> =
        body.as_bytes().chunks(64).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
    format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", wrapped.join("\n"))
}

/// Lower-case hex of the SHA-256 of a DER certificate.
pub fn fingerprint_of(der: &[u8]) -> String {
    let provider = rustls::crypto::ring::default_provider();
    // The SHA-256 every TLS suite in this provider already carries, rather than
    // a second hash implementation pulled in for one line.
    let sha256 = provider
        .cipher_suites
        .iter()
        .find_map(|suite| {
            let hash = suite.tls13()?.common.hash_provider;
            (hash.algorithm() == rustls::crypto::hash::HashAlgorithm::SHA256).then_some(hash)
        })
        .expect("the ring provider offers SHA-256");
    sha256.hash(der).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Take the private key's permissions down to the owner, where the platform
/// has such a thing.
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(error = %e, "could not restrict the server key's permissions");
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// The names to put in the certificate.
///
/// A fixed set, and that is the entire design: it does not depend on the
/// machine's interfaces, so the certificate is generated once and its
/// fingerprint never changes under a paired device. Clients identify the
/// server by pinning the certificate rather than by matching a name -- see the
/// module docs.
///
/// `listen` is accepted and ignored, kept so the signature says what it is
/// about and so a future build that did want a name per address has somewhere
/// obvious to put it.
pub fn local_names(listen: &std::net::SocketAddr) -> Vec<String> {
    let _ = listen;
    vec!["everyday.local".to_string(), "localhost".to_string(), "127.0.0.1".to_string()]
}

/// The addresses of this machine's own interfaces.
///
/// Read from the operating system rather than guessed. There is no portable
/// standard-library way to enumerate interfaces, and pulling a crate in for it
/// would be a dependency for one screen, so this asks the kernel the way each
/// platform allows and answers with nothing when it cannot.
pub fn interface_addresses() -> Vec<IpAddr> {
    #[cfg(target_os = "linux")]
    {
        // A connected UDP socket picks the source address the routing table
        // would use, without sending a packet. That is the address a client on
        // that network reaches us at, which is the one worth putting in the
        // certificate.
        let mut out = Vec::new();
        for probe in ["100.64.0.1:9", "10.0.0.1:9", "192.168.1.1:9", "8.8.8.8:9"] {
            if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0")
                && socket.connect(probe).is_ok()
                && let Ok(addr) = socket.local_addr()
                && !addr.ip().is_loopback()
                && !addr.ip().is_unspecified()
            {
                out.push(addr.ip());
            }
        }
        out.sort();
        out.dedup();
        out
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut out = Vec::new();
        for probe in ["100.64.0.1:9", "192.168.1.1:9", "8.8.8.8:9"] {
            if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0")
                && socket.connect(probe).is_ok()
                && let Ok(addr) = socket.local_addr()
                && !addr.ip().is_loopback()
                && !addr.ip().is_unspecified()
            {
                out.push(addr.ip());
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_certificate_is_made_once_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let first = Identity::load_or_create(dir.path(), &names).unwrap();
        let second = Identity::load_or_create(dir.path(), &names).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint, "a restart must not unpair every device");
    }

    /// Moving to a different network must not unpair every device.
    ///
    /// The failure this replaced: the SANs were built by probing the routing
    /// table, so joining a different wifi produced a different name list,
    /// which regenerated the certificate, which changed the fingerprint every
    /// paired client had pinned. The names are fixed now, and a client checks
    /// the certificate rather than the name.
    #[test]
    fn the_names_do_not_depend_on_what_network_this_machine_is_on() {
        let here: std::net::SocketAddr = "0.0.0.0:7397".parse().unwrap();
        let elsewhere: std::net::SocketAddr = "100.64.7.9:7397".parse().unwrap();
        assert_eq!(local_names(&here), local_names(&elsewhere));

        let dir = tempfile::tempdir().unwrap();
        let first = Identity::load_or_create(dir.path(), &local_names(&here)).unwrap();
        let second = Identity::load_or_create(dir.path(), &local_names(&elsewhere)).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
    }

    /// A genuinely different name list still regenerates, which is what makes
    /// the caching above a cache rather than a latch.
    #[test]
    fn a_different_name_list_gets_a_new_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let first = Identity::load_or_create(dir.path(), &["localhost".into()]).unwrap();
        let second =
            Identity::load_or_create(dir.path(), &["localhost".into(), "10.1.2.3".into()]).unwrap();
        assert_ne!(first.fingerprint, second.fingerprint);
    }

    #[test]
    fn the_fingerprint_is_of_the_certificate_a_client_will_see() {
        let dir = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(dir.path(), &["localhost".into()]).unwrap();
        let der = pem_to_der(&identity.cert_pem).unwrap();
        assert_eq!(identity.fingerprint, fingerprint_of(&der));
        assert_eq!(identity.fingerprint.len(), 64);
    }

    #[cfg(unix)]
    #[test]
    fn the_private_key_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        Identity::load_or_create(dir.path(), &["localhost".into()]).unwrap();
        let mode = std::fs::metadata(dir.path().join(KEY_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the key is readable by somebody else");
    }
}
