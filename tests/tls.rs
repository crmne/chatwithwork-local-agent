//! cww trusts the OS certificate store, so a server whose certificate comes
//! from a private CA (a company proxy, a self-hosted install, a local
//! development CA) works once that CA is trusted by the system.
//!
//! `SSL_CERT_FILE` stands in for the OS store here: rustls-native-certs
//! reads it instead of the platform store when it is set. This file is its
//! own test binary, so setting the variable affects nothing else.

use std::sync::Arc;
use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct PrivateServer {
    port: u16,
    ca_pem: String,
}

/// An HTTPS server on 127.0.0.1 with a certificate for `localhost` from a
/// fresh private CA. It answers every request with a small JSON error.
async fn private_server() -> PrivateServer {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "cww test CA");
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .signed_by(&leaf_key, &ca)
        .unwrap();

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
        )
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let _ = tls.read(&mut buf).await;
                let body = r#"{"error":"invalid_request"}"#;
                let response = format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = tls.write_all(response.as_bytes()).await;
                let _ = tls.shutdown().await;
            });
        }
    });
    PrivateServer {
        port,
        ca_pem: ca.pem(),
    }
}

async fn handshake(config: Arc<rustls::ClientConfig>, port: u16) -> std::io::Result<()> {
    let stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let connector = tokio_rustls::TlsConnector::from(config);
    let name = ServerName::try_from("localhost").unwrap();
    connector.connect(name, stream).await.map(|_| ())
}

#[tokio::test]
async fn trusts_a_private_ca_from_the_os_store() {
    let server = private_server().await;

    // Mozilla's roots alone don't know this CA.
    let mut mozilla = rustls::RootCertStore::empty();
    mozilla.add_parsable_certificates(webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mozilla_only = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(mozilla)
        .with_no_client_auth();
    assert!(
        handshake(Arc::new(mozilla_only), server.port)
            .await
            .is_err()
    );

    // Put the CA in the "OS store" before cww loads it.
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("ca.pem");
    std::fs::write(&bundle, &server.ca_pem).unwrap();
    // SAFETY: this test binary has no other threads reading the environment
    // yet, and cww loads the store once, lazily, below.
    unsafe { std::env::set_var("SSL_CERT_FILE", &bundle) };

    // The tunnel's configuration trusts it...
    handshake(cww::tls::client_config(), server.port)
        .await
        .expect("the tunnel trusts a CA from the OS store");

    // ...and so do the pairing and token requests.
    let port = server.port;
    let status = tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(cww::tls::ureq_config())
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        agent
            .get(&format!("https://localhost:{port}/local_agent/token"))
            .call()
            .map(|r| r.status().as_u16())
    })
    .await
    .unwrap()
    .expect("the pairing client trusts a CA from the OS store");
    assert_eq!(status, 400);
}
