//! Which certificate the browser is shown.
//!
//! A browser cannot pin a fingerprint the way a paired copy of this app does
//! (see `crate::tls`), so the certificate has to be one it already trusts for
//! the name in its address bar. Tailscale issues exactly that for a
//! machine's tailnet name, and keeps it renewed.
//!
//! The listener never holds a certificate directly. It holds a
//! [`Resolver`], which hands rustls whatever was loaded last, so a renewed
//! certificate is in use from the next handshake on, with no restart and no
//! dropped connection.
//!
//! [`Certificate`] is a tagged enum in `web.json` for the same reason
//! `Network` is: a self-generated certificate -- `crate::tls::Identity`
//! grown into a small local CA, for a network Tailscale does not cover -- is
//! one more variant of [`Certificate`] and one more arm in [`load`].

use everyday_service::error::{CommandError, CommandResult};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

use super::network::{Gate, Place};

/// Where the certificate comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Certificate {
    /// The one tailscaled issues for this machine's tailnet name.
    #[default]
    Tailscale,
}

/// What was loaded, for the settings pane.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Loaded {
    pub names: Vec<String>,
    pub at: jiff::Timestamp,
}

/// Hands rustls the certificate loaded last.
#[derive(Debug, Default)]
pub struct Resolver {
    current: RwLock<Option<Arc<CertifiedKey>>>,
}

impl Resolver {
    pub fn set(&self, key: Arc<CertifiedKey>) {
        *self.current.write().unwrap() = Some(key);
    }

    pub fn is_ready(&self) -> bool {
        self.current.read().unwrap().is_some()
    }
}

impl ResolvesServerCert for Resolver {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.current.read().unwrap().clone()
    }
}

/// Fetch a certificate for `place` from `source`.
pub async fn load(
    source: &Certificate,
    gate: &Gate,
    place: &Place,
) -> CommandResult<(Arc<CertifiedKey>, Loaded)> {
    match source {
        Certificate::Tailscale => {
            let name = place.names.first().ok_or_else(|| {
                CommandError::new("unreachable", "there is no name to issue a certificate for")
            })?;
            let pem = gate.tailscale()?.cert_pair(name).await?;
            let key = certified_key(&pem)?;
            Ok((key, Loaded { names: vec![name.clone()], at: jiff::Timestamp::now() }))
        }
    }
}

/// A PEM document holding a private key and its certificate chain, in
/// either order, as rustls wants it.
pub fn certified_key(pem: &str) -> CommandResult<Arc<CertifiedKey>> {
    let certs = crate::rustls_pemfile_certs(pem)?;
    let key = crate::rustls_pemfile_key(pem)?;
    let provider = rustls::crypto::ring::default_provider();
    let signing = provider
        .key_provider
        .load_private_key(key)
        .map_err(|e| CommandError::new("invalid", format!("that private key is unusable: {e}")))?;
    Ok(Arc::new(CertifiedKey::new(certs, signing)))
}

/// The TLS settings every web listener shares: HTTP/2 and 1.1, and whatever
/// certificate `resolver` holds at the moment of each handshake.
pub fn server_config(resolver: Arc<Resolver>) -> CommandResult<rustls::ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| CommandError::new("internal", e.to_string()))?
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_and_chain_in_one_document_load_in_either_order() {
        let generated =
            rcgen::generate_simple_self_signed(vec!["box.tail1.ts.net".into()]).unwrap();
        let cert = generated.cert.pem();
        let key = generated.signing_key.serialize_pem();
        // tailscaled's `type=pair` puts the key first.
        assert!(certified_key(&format!("{key}{cert}")).is_ok());
        assert!(certified_key(&format!("{cert}{key}")).is_ok());
        assert!(certified_key(&cert).is_err(), "a certificate alone has no key");
    }

    #[test]
    fn the_resolver_hands_out_whatever_was_set_last() {
        let resolver = Resolver::default();
        assert!(!resolver.is_ready());
        let generated = rcgen::generate_simple_self_signed(vec!["a".into()]).unwrap();
        let pem = format!("{}{}", generated.signing_key.serialize_pem(), generated.cert.pem());
        resolver.set(certified_key(&pem).unwrap());
        assert!(resolver.is_ready());
    }
}
