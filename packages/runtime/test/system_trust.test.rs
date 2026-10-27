use super::*;
#[path = "system_tls_support.rs"]
mod tls;
#[tokio::test]
async fn pinned_sdk_checks_certificates_for_both_target_and_https_proxy() {
    tls::check_http_trust(|proxy, roots| {
        let policy = proxy
            .map(|url| NetworkProxy::Proxy { url: url.into() })
            .unwrap_or(NetworkProxy::Direct);
        nora_provider_network::http_client_builder(Some(&policy))
            .unwrap()
            .use_preconfigured_tls(nora_provider_network::tls::http_client_config(roots).unwrap())
    })
    .await;
}

#[tokio::test]
async fn actual_runtime_client_rejects_untrusted_certificate() {
    let ca = tls::Authority::new();
    let server = tls::Server::start(&ca, &["localhost"], false, b"untrusted").await;
    let client = Client::new(None, Some(&NetworkProxy::Direct), false).unwrap();
    assert!(client
        .request(HttpRequest::new(
            nora_ai::HttpMethod::Get,
            server.url("localhost")
        ))
        .await
        .is_err());
}

#[path = "system_trust_dependencies.test.rs"]
mod dependencies;
