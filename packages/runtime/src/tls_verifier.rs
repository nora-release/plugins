use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};
use rustls::{
    client::{
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        WebPkiServerVerifier,
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, Error, SignatureScheme,
};
use std::{collections::BTreeSet, sync::Arc};

/// TLS for a target may be relaxed without relaxing the HTTPS proxy's TLS.
/// The outer client still chooses this configuration by host *and* port.
#[derive(Debug)]
struct TargetVerifier {
    hosts: BTreeSet<String>,
    verified: Arc<WebPkiServerVerifier>,
}
impl ServerCertVerifier for TargetVerifier {
    fn verify_server_cert(
        &self,
        leaf: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        if self.hosts.contains(
            server
                .to_str()
                .trim_end_matches('.')
                .to_ascii_lowercase()
                .as_str(),
        ) {
            return Ok(ServerCertVerified::assertion());
        }
        self.verified
            .verify_server_cert(leaf, intermediates, server, ocsp, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verified
            .verify_tls12_signature(message, cert, signature)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verified
            .verify_tls13_signature(message, cert, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.verified.supported_verify_schemes()
    }
}

pub(crate) fn configuration(
    hosts: BTreeSet<String>,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    with_roots(hosts, proxy, roots)
}
fn with_roots(
    mut hosts: BTreeSet<String>,
    proxy: Option<&NetworkProxy>,
    roots: rustls::RootCertStore,
) -> PluginResult<rustls::ClientConfig> {
    // A proxy sharing the target's DNS name cannot be distinguished by a TLS
    // verifier (it receives no port). In that case both connections verify.
    if let Some(NetworkProxy::Proxy { url }) = proxy {
        if let Ok(url) = url::Url::parse(url) {
            if url.scheme() == "https" {
                if let Some(host) = url.host_str() {
                    hosts.remove(&host.trim_end_matches('.').to_ascii_lowercase());
                }
            }
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verified = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|_| invalid())?;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| invalid())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(TargetVerifier { hosts, verified }))
        .with_no_client_auth();
    Ok(config)
}
fn invalid() -> PluginError {
    PluginError::transport("The provider TLS client could not be initialized.")
}

#[cfg(test)]
#[path = "../test/tls_verifier.test.rs"]
mod tests;
