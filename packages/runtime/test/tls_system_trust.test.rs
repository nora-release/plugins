use super::*;
#[path = "system_tls_support.rs"]
mod tls;

fn client(
    hosts: &[&str],
    proxy: Option<&NetworkProxy>,
    roots: Vec<CertificateDer<'static>>,
) -> reqwest::Client {
    let config =
        configuration_with_extra_roots(hosts.iter().map(|v| v.to_string()).collect(), proxy, roots)
            .unwrap();
    let mut builder = nora_provider_plugin_sdk::http_client_builder(proxy).unwrap();
    if let Some(NetworkProxy::Proxy { url }) = proxy {
        let url = url::Url::parse(url).unwrap();
        builder = builder.resolve(
            url.host_str().unwrap(),
            format!("127.0.0.1:{}", url.port().unwrap())
                .parse()
                .unwrap(),
        );
    }
    builder
        .timeout(std::time::Duration::from_secs(5))
        .use_preconfigured_tls(config)
        .build()
        .unwrap()
}

#[tokio::test]
async fn platform_validation_checks_the_https_proxy_even_for_an_unchecked_target() {
    let ca = tls::Authority::new();
    let proxy_ca = tls::Authority::new();
    let target = tls::Server::start(&ca, &["localhost"], false, b"target").await;
    let proxy = tls::Server::proxy(&proxy_ca, target.address).await;
    let policy = NetworkProxy::Proxy {
        url: proxy.proxy_url(),
    };
    let url = target.url("unchecked.invalid");
    assert!(client(&["unchecked.invalid"], Some(&policy), vec![])
        .get(&url)
        .send()
        .await
        .is_err());
    assert_eq!(
        client(&["unchecked.invalid"], Some(&policy), vec![proxy_ca.der()])
            .get(&url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "target"
    );
    assert!(client(&[], Some(&policy), vec![proxy_ca.der()])
        .get(&url)
        .send()
        .await
        .is_err());
    assert!(client(&["unchecked.invalid"], Some(&policy), vec![])
        .get(&url)
        .send()
        .await
        .is_err());
}

#[tokio::test]
async fn platform_validation_cannot_relax_a_proxy_sharing_the_target_hostname() {
    let ca = tls::Authority::new();
    let proxy_ca = tls::Authority::new();
    let target = tls::Server::start(&ca, &["shared.nora.test"], false, b"target").await;
    let proxy = tls::Server::proxy(&proxy_ca, target.address).await;
    let policy = NetworkProxy::Proxy {
        url: format!("https://shared.nora.test:{}", proxy.address.port()),
    };
    let url = target.url("shared.nora.test");
    assert!(
        client(&["shared.nora.test"], Some(&policy), vec![proxy_ca.der()])
            .get(&url)
            .send()
            .await
            .is_err()
    );
    assert_eq!(
        client(
            &["shared.nora.test"],
            Some(&policy),
            vec![ca.der(), proxy_ca.der()]
        )
        .get(&url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap(),
        "target"
    );
}

#[test]
fn invalid_platform_anchor_fails_without_disabling_verification() {
    assert!(configuration_with_extra_roots(
        BTreeSet::new(),
        None,
        [CertificateDer::from(b"invalid".to_vec())]
    )
    .is_err());
}
