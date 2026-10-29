use super::*;
#[test]
fn endpoint_scope_matches_host_and_port_without_extending_to_model_or_balance_urls() {
    assert_eq!(
        host("https://EXAMPLE.com./v1"),
        Some(("example.com".into(), 443))
    );
    assert_eq!(
        host("https://example.com:444/v1"),
        Some(("example.com".into(), 444))
    );
    assert_eq!(host("http://example.com"), None);
}

fn definition(base: &str) -> Definition {
    Definition::from_fields(serde_json::json!({"id":"fixture","name":"Fixture","api":"openai-completions","baseURL":base,"config":{"keyProvider":"fixture-key","customTemplate":true,"insecureTLS":true}})).unwrap()
}
#[tokio::test]
async fn self_signed_override_only_applies_to_custom_api_host_port_and_each_instance() {
    let server = crate::tls_support::Server::start("{}").await;
    let other = crate::tls_support::Server::start("{}").await;
    let mut definition = definition(&server.url("127.0.0.1", "/v1"));
    let target = server.url("127.0.0.1", "/v1/models");
    let same = raw_for_url(&definition, None, true, &target).unwrap();
    assert!(same.get(&target).send().await.is_ok());
    let cross = server.url("localhost", "/models");
    assert!(raw_for_url(&definition, None, true, &cross)
        .unwrap()
        .get(&cross)
        .send()
        .await
        .is_err());
    let port = other.url("127.0.0.1", "/models");
    assert!(raw_for_url(&definition, None, true, &port)
        .unwrap()
        .get(&port)
        .send()
        .await
        .is_err());
    assert!(raw_for_url(&definition, None, false, &target)
        .unwrap()
        .get(&target)
        .send()
        .await
        .is_err());
    definition.config.insecure_tls = false;
    assert!(raw_for_url(&definition, None, true, &target)
        .unwrap()
        .get(&target)
        .send()
        .await
        .is_err());
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn same_host_model_and_balance_overrides_work_while_cross_host_self_signed_stays_verified() {
    let server =
        crate::tls_support::Server::start(r#"{"data":[{"id":"live-model"}],"balance":7}"#).await;
    let mut definition = definition(&server.url("127.0.0.1", "/v1"));
    definition.config.models_url = server.url("127.0.0.1", "/private/models");
    definition.config.balance_url = server.url("127.0.0.1", "/private/balance");
    definition.config.balance_path = "$balance".into();
    let credentials = serde_json::json!({"type":"apiKey","access":"private-key"});
    let (_, resolved) = crate::auth::resolve(&definition, &credentials)
        .await
        .unwrap();
    let client = Client::new(Some(&definition), None, true).unwrap();
    let usage = crate::endpoint_overrides::balance(&definition, &credentials, &resolved, &client)
        .await
        .unwrap();
    assert_eq!(usage["balances"][0]["amount"], 7.0);
    definition.config.balance_url =
        server.url("localhost", "/private/balance?secret=private-query");
    let error = crate::endpoint_overrides::balance(&definition, &credentials, &resolved, &client)
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("private-query"));
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}
