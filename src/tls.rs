//! Which servers `cww` trusts.
//!
//! The operating system's certificate store (the macOS keychains, the
//! Windows certificate store, or the distribution's CA bundle on Linux),
//! plus Mozilla's root certificates for machines whose store is missing or
//! empty. The OS store is what makes private CAs work: a company's TLS
//! proxy, a self-hosted Chat with Work behind an internal CA, or a local
//! development CA such as Caddy's. `SSL_CERT_FILE` and `SSL_CERT_DIR`
//! replace the OS store, as in OpenSSL.
//!
//! Plain `http://` is still refused for anything but loopback; see
//! `auth::ServerUrl`. Verification itself is rustls/webpki, in process.

use std::sync::{Arc, OnceLock};

use rustls::pki_types::CertificateDer;

static ROOTS: OnceLock<Vec<CertificateDer<'static>>> = OnceLock::new();

/// The trusted root certificates, loaded once per process.
pub fn root_certificates() -> &'static [CertificateDer<'static>] {
    ROOTS.get_or_init(|| {
        let native = rustls_native_certs::load_native_certs();
        for error in &native.errors {
            tracing::debug!("skipping part of the OS certificate store: {error}");
        }
        tracing::debug!(
            "trusting {} certificates from the OS store and Mozilla's roots",
            native.certs.len()
        );
        native
            .certs
            .into_iter()
            .chain(webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned())
            .collect()
    })
}

/// The rustls configuration for the WebSocket tunnel.
pub fn client_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    let (added, ignored) = roots.add_parsable_certificates(root_certificates().iter().cloned());
    tracing::debug!("{added} trusted roots, {ignored} unusable");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}

/// The same roots, for the pairing and token requests.
pub fn ureq_config() -> ureq::tls::TlsConfig {
    let certs: Vec<ureq::tls::Certificate<'static>> = root_certificates()
        .iter()
        .map(|c| ureq::tls::Certificate::from_der(c.as_ref()))
        .collect();
    ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::new_with_certs(&certs))
        .build()
}

#[cfg(test)]
mod tests {
    #[test]
    fn mozilla_roots_are_always_there() {
        assert!(super::root_certificates().len() >= webpki_root_certs::TLS_SERVER_ROOT_CERTS.len());
    }
}
