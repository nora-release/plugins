use super::*;
#[test]
fn target_and_https_proxy_certificate_checks_are_independent() {
    let certificate =
        rcgen::generate_simple_self_signed(vec!["target.invalid".into(), "proxy.invalid".into()])
            .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verified = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
        .build()
        .unwrap();
    let scoped = TargetVerifier {
        hosts: BTreeSet::from(["target.invalid".into()]),
        verified,
    };
    let check = |name: &str| {
        scoped.verify_server_cert(
            certificate.cert.der(),
            &[],
            &ServerName::try_from(name.to_string()).unwrap(),
            &[],
            UnixTime::now(),
        )
    };
    assert!(check("target.invalid").is_ok());
    assert!(check("proxy.invalid").is_err());
    assert!(check("unrelated.invalid").is_err());
}
#[test]
fn trustworthy_https_proxy_keeps_normal_verification() {
    let certificate = rcgen::generate_simple_self_signed(vec!["proxy.invalid".into()]).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.cert.der().clone()).unwrap();
    let verified = WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()
    .unwrap();
    let scoped = TargetVerifier {
        hosts: BTreeSet::from(["target.invalid".into()]),
        verified,
    };
    assert!(scoped
        .verify_server_cert(
            certificate.cert.der(),
            &[],
            &ServerName::try_from("proxy.invalid").unwrap(),
            &[],
            UnixTime::now()
        )
        .is_ok());
    assert!(scoped
        .verify_server_cert(
            certificate.cert.der(),
            &[],
            &ServerName::try_from("other.invalid").unwrap(),
            &[],
            UnixTime::now()
        )
        .is_err());
}
#[tokio::test]
async fn real_https_proxy_verifies_its_own_certificate_while_target_is_unchecked() {
    let target = crate::tls_support::Server::start("{}").await;
    let proxy = crate::tls_support::Server::proxy(target.address).await;
    let proxy_config = NetworkProxy::Proxy {
        url: proxy.url("127.0.0.1", ""),
    };
    let untrusted = configuration(
        BTreeSet::from(["target.invalid".into()]),
        Some(&proxy_config),
    )
    .unwrap();
    let request = |tls| {
        nora_provider_plugin_sdk::http_client_builder(Some(&proxy_config))
            .unwrap()
            .use_preconfigured_tls(tls)
            .build()
            .unwrap()
            .get(target.url("target.invalid", "/probe"))
            .send()
    };
    assert!(request(untrusted).await.is_err());
    assert!(proxy.requests.lock().unwrap().is_empty());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(proxy.certificate.clone()).unwrap();
    let trusted = with_roots(
        BTreeSet::from(["target.invalid".into()]),
        Some(&proxy_config),
        roots,
    )
    .unwrap();
    assert_eq!(request(trusted).await.unwrap().status(), 200);
    assert_eq!(proxy.requests.lock().unwrap().len(), 1);
    assert_eq!(target.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn https_proxy_sharing_target_dns_requires_both_certificates_verified() {
    let target = crate::tls_support::Server::start("{}").await;
    let proxy = crate::tls_support::Server::proxy(target.address).await;
    let proxy_config = NetworkProxy::Proxy {
        url: proxy.url("shared.nora-proxy.test", ""),
    };
    let mut roots = rustls::RootCertStore::empty();
    roots.add(proxy.certificate.clone()).unwrap();
    let tls = with_roots(
        BTreeSet::from(["shared.nora-proxy.test".into()]),
        Some(&proxy_config),
        roots,
    )
    .unwrap();
    let response = nora_provider_plugin_sdk::http_client_builder(Some(&proxy_config))
        .unwrap()
        .resolve("shared.nora-proxy.test", proxy.address)
        .use_preconfigured_tls(tls)
        .build()
        .unwrap()
        .get(target.url("shared.nora-proxy.test", "/probe"))
        .send()
        .await;
    assert!(response.is_err());
    assert_eq!(proxy.requests.lock().unwrap().len(), 1);
    assert!(target.requests.lock().unwrap().is_empty());
}
