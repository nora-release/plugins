use rustls::{pki_types::CertificateDer, ClientConfig, Error};
use std::sync::Arc;

/// TLS configuration for non-HTTP transports. macOS evaluates its trust
/// policies at connection time, including explicit user and administrator trust.
pub fn client_config() -> Result<ClientConfig, Error> {
    client_config_with_extra_roots(std::iter::empty())
}

/// Additional anchors belong to this client; they never change system trust or
/// another connection's certificate/hostname verification.
pub fn client_config_with_extra_roots(
    certificates: impl IntoIterator<Item = CertificateDer<'static>>,
) -> Result<ClientConfig, Error> {
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?;
    Ok(builder
        .dangerous()
        .with_custom_certificate_verifier(verifier(provider, certificates)?)
        .with_no_client_auth())
}

/// HTTP requests advertise the protocols supported by this client. The proxy
/// transport clears ALPN for its separate CONNECT tunnel in reqwest.
pub fn http_client_config(
    certificates: impl IntoIterator<Item = CertificateDer<'static>>,
) -> Result<ClientConfig, Error> {
    let mut config = client_config_with_extra_roots(certificates)?;
    config.alpn_protocols = vec![
        #[cfg(feature = "http2")]
        b"h2".to_vec(),
        b"http/1.1".to_vec(),
    ];
    Ok(config)
}

pub fn verifier(
    provider: Arc<rustls::crypto::CryptoProvider>,
    certificates: impl IntoIterator<Item = CertificateDer<'static>>,
) -> Result<Arc<dyn rustls::client::danger::ServerCertVerifier>, Error> {
    #[cfg(target_os = "macos")]
    let verifier = Arc::new(rustls_platform_verifier::Verifier::new_with_extra_roots(
        certificates,
        provider,
    )?);
    #[cfg(not(target_os = "macos"))]
    let verifier = {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for certificate in certificates {
            roots.add(certificate)?;
        }
        rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
            .build()
            .map_err(|error| Error::General(error.to_string()))?
    };
    Ok(verifier)
}
