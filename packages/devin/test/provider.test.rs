use super::*;
use crate::fixture::{self, Client, Reply};
use crate::protobuf::Pb;
fn initialize() -> InitializeParams {
    InitializeParams {
        api_version: 1,
        plugin_id: "devin".into(),
        plugin_directory: std::env::temp_dir(),
        data_directory: std::env::temp_dir(),
        manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
    }
}
async fn plugin(client: Arc<Client>) -> Devin {
    let plugin = Devin {
        client: Some(client),
        ..Default::default()
    };
    plugin.initialize(initialize()).await.unwrap();
    plugin
}
#[tokio::test]
async fn initialization_does_not_install_execute_or_import_any_cli_account() {
    let client = Client::new(vec![]);
    let p = plugin(client.clone()).await;
    assert_eq!(p.accounts(CallContext::default()).await.unwrap(), json!([]));
    assert_eq!(
        p.auth_methods(CallContext::default())
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        p.models(CallContext::default()).await.unwrap()["models"],
        json!([])
    );
    assert!(client.requests.lock().unwrap().is_empty());
    let mut wrong = initialize();
    wrong.manifest["baseURL"] = json!("https://custom.test/v1");
    assert!(p.initialize(wrong).await.is_err());
}
#[tokio::test]
async fn live_models_come_from_real_protobuf_not_a_static_success() {
    let client = Client::new(vec![Reply::bytes(fixture::configs())]);
    let p = plugin(client.clone()).await;
    let value = p
        .models(CallContext {
            account: "one".into(),
            credentials: Some(fixture::saved()),
            network_proxy: None,
        })
        .await
        .unwrap();
    assert_eq!(value["source"], "remote");
    assert!(value["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == "swe-2"));
    let requests = client.requests.lock().unwrap();
    assert!(requests[0].url.ends_with("/GetCliModelConfigs"));
    assert_eq!(requests[0].headers["content-type"], "application/proto");
    let metadata = crate::protobuf::fields(requests[0].body.as_ref().unwrap()).unwrap();
    let metadata = crate::protobuf::fields(metadata[0].data).unwrap();
    assert_eq!(
        metadata
            .iter()
            .find(|f| f.key == 3)
            .unwrap()
            .text()
            .unwrap(),
        "private-devin-key"
    );
}
#[tokio::test]
async fn actual_fetch_uses_native_connect_and_does_not_forward_caller_auth() {
    let client = Client::new(vec![
        Reply::bytes(fixture::configs()),
        Reply::stream(&[Pb::new().text(3, "answer").int(5, 2)], Some(json!({}))),
    ]);
    let p = plugin(client.clone()).await;
    let response = p
        .fetch(fixture::fetch(
            json!({"model":"swe-2","messages":[{"role":"user","content":"question"}]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert!(fixture::body(response).await.contains("answer"));
    let requests = client.requests.lock().unwrap();
    let r = &requests[1];
    assert_eq!(r.url, format!("{}{}", network::SERVER, network::CHAT));
    assert_eq!(r.headers["authorization"], "Basic private-devin-key");
    assert_eq!(r.headers["content-type"], "application/connect+proto");
    assert!(!r.headers.contains_key("cookie"));
    assert!(!r.headers.contains_key("connect-accept-encoding"));
    let frame = r.body.as_ref().unwrap();
    let bytes = if frame[0] & 1 != 0 {
        protobuf::inflate(&frame[5..]).unwrap()
    } else {
        frame[5..].to_vec()
    };
    let fields = protobuf::fields(&bytes).unwrap();
    assert_eq!(
        fields.iter().find(|f| f.key == 21).unwrap().text().unwrap(),
        "swe-2-high"
    );
}
#[tokio::test]
async fn reasoning_metadata_selects_real_native_variant() {
    let client = Client::new(vec![
        Reply::bytes(fixture::configs()),
        Reply::stream(&[Pb::new().text(3, "answer")], Some(json!({}))),
    ]);
    let p = plugin(client.clone()).await;
    let mut request =
        fixture::fetch(json!({"model":"swe-2","messages":[{"role":"user","content":"question"}]}));
    request
        .metadata
        .insert("reasoningEffort".into(), "medium".into());
    p.fetch(request).await.unwrap();
    let requests = client.requests.lock().unwrap();
    let frame = requests[1].body.as_ref().unwrap();
    let fields = protobuf::fields(&frame[5..]).unwrap();
    assert_eq!(
        fields.iter().find(|f| f.key == 21).unwrap().text().unwrap(),
        "swe-2-medium"
    );
}
#[tokio::test]
async fn read_only_source_uses_verified_identity_and_never_saves_its_private_token() {
    let path = fixture::temp("source.toml");
    let source =
        "windsurf_api_key = 'private-source-key'\napi_server_url = 'https://server.codeium.com'\n";
    std::fs::write(&path, source).unwrap();
    let client = Client::new(vec![
        Reply::json(fixture::me()),
        Reply::json(fixture::me()),
        Reply::bytes(fixture::configs()),
    ]);
    let p = plugin(client.clone()).await;
    let result = p
        .authorize(json!({"method":"cliSource","inputs":{"sourcePath":path}}))
        .await
        .unwrap();
    assert_eq!(result["credentials"]["accountId"], "one@example.test");
    assert!(result["credentials"].get("key").is_none());
    assert!(!result["credentials"]
        .to_string()
        .contains("private-source-key"));
    p.models(CallContext {
        account: "one".into(),
        credentials: Some(result["credentials"].clone()),
        network_proxy: None,
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    std::fs::remove_file(path).unwrap();
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn two_accounts_never_share_the_wrong_model_catalog() {
    let client = Client::new(vec![
        Reply::bytes(fixture::configs()),
        Reply::bytes(
            Pb::new()
                .bytes(
                    1,
                    Pb::new()
                        .text(1, "other")
                        .text(22, "other-high")
                        .bytes(23, Pb::new().text(23, "other")),
                )
                .0,
        ),
    ]);
    let p = plugin(client.clone()).await;
    let first = Account::parse(&fixture::saved()).unwrap();
    let mut second = first.clone();
    second.key = "private-account-two-key".into();
    let one = p.listing(client.as_ref(), &first, false).await.unwrap();
    let two = p.listing(client.as_ref(), &second, false).await.unwrap();
    assert_eq!(two[0].id, "other-high");
    assert_ne!(one[0].id, two[0].id);
    let cached = p.listing(client.as_ref(), &first, false).await.unwrap();
    assert_eq!(cached[0].id, one[0].id);
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn native_model_image_restriction_is_explicit_415() {
    let client = Client::new(vec![Reply::bytes(fixture::configs())]);
    let p = plugin(client.clone()).await;
    let response=p.fetch(fixture::fetch(json!({"model":"no-image","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,YQ=="}}]}]}))).await.unwrap();
    assert_eq!(response.status, 415);
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn http_errors_and_model_discovery_failures_do_not_leak_account_data() {
    let client = Client::new(vec![Reply::status(
        401,
        json!({"code":"unauthenticated","message":"private-key prompt"}),
    )]);
    let p = plugin(client.clone()).await;
    let error = p
        .models(CallContext {
            account: "one".into(),
            credentials: Some(fixture::saved()),
            network_proxy: None,
        })
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "authentication_required");
    assert!(!error.message.contains("private"));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn usage_is_live_and_daily_weekly_quotas_exclude_free_models() {
    let client = Client::new(vec![
        Reply::json(fixture::me()),
        Reply::bytes(fixture::configs()),
    ]);
    let p = plugin(client.clone()).await;
    let value = p
        .usage(CallContext {
            account: "one".into(),
            credentials: Some(fixture::saved()),
            network_proxy: None,
        })
        .await
        .unwrap();
    assert_eq!(value["items"][1]["usedPercent"], 100.0);
    assert!(value["items"][1]["modelIDs"]
        .as_array()
        .unwrap()
        .iter()
        .all(|id| !id.as_str().unwrap().starts_with("swe-2")));
    assert!(client.requests.lock().unwrap()[0]
        .url
        .ends_with("/GetUserStatus"));
}
#[tokio::test]
async fn bad_proxy_never_falls_back_to_direct_before_private_requests() {
    let client = Client::new(vec![]);
    let p = plugin(client.clone()).await;
    let mut init = initialize();
    init.manifest["config"]["networkProxy"] = json!({"mode":"proxy","url":"file:///private/proxy"});
    assert!(p.initialize(init).await.is_err());
    assert!(p
        .params_client(&json!({"networkProxy":{"mode":"proxy","url":"http://proxy.test/private"}}))
        .is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}
