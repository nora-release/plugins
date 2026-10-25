use super::*;
use crate::test_support::{Client, Directory};
async fn fixture() -> (Directory, Arc<Client>, Remote) {
    let directory = Directory::new();
    let client = Arc::new(Client::default());
    let provider = Remote {
        client: Some(client.clone()),
        ..Remote::default()
    };
    provider
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: 1,
            plugin_id: "remote-magpie".into(),
            plugin_directory: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            data_directory: directory.0.clone(),
            manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
        })
        .await
        .unwrap();
    (directory, client, provider)
}
#[tokio::test]
async fn initialization_neither_installs_accounts_nor_reads_any_desktop_cli() {
    let (directory, client, provider) = fixture().await;
    assert_eq!(
        provider.accounts(CallContext::default()).await.unwrap(),
        json!([])
    );
    let models = provider.models(CallContext::default()).await.unwrap();
    assert_eq!(models["source"], "preset");
    assert_eq!(models["models"], json!([]));
    assert!(client.requests.lock().unwrap().is_empty());
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 0);
}
#[tokio::test]
async fn explicit_endpoint_and_key_are_checked_against_the_real_models_api_before_being_saved() {
    let (_directory, client, provider) = fixture().await;
    client.push(
        200,
        json!({"data":[{"id":"relay/model","native_endpoints":["/v1/messages"]}]}),
    );
    let result=provider.authorize(json!({"inputs":{"endpoint":"remote.test:3572/v1","apiKey":"gateway-secret","name":"Office"}})).await.unwrap();
    assert_eq!(result["credentials"]["key"], "gateway-secret");
    assert_eq!(
        result["credentials"]["extras"]["endpoint"],
        "http://remote.test:3572"
    );
    assert!(result["credentials"].get("email").is_none());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, "http://remote.test:3572/v1/models");
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer gateway-secret"
    );
}
#[tokio::test]
async fn rejected_credentials_do_not_produce_a_successful_authorization() {
    let (_directory, client, provider) = fixture().await;
    client.push(401, json!({"error":{"message":"bad key"}}));
    let error = provider
        .authorize(json!({"inputs":{"endpoint":"remote.test:3572","apiKey":"bad-key"}}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "authentication_required");
}
#[tokio::test]
async fn a_real_empty_authoritative_list_clears_old_remote_permissions() {
    let (_directory, client, provider) = fixture().await;
    client.push(200, json!({"data":[]}));
    let account = Account::inputs(&json!({"endpoint":"remote.test:3572","apiKey":"key"})).unwrap();
    let result = provider
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: "local-key".into(),
            credentials: Some(account.saved()),
            network_proxy: None,
        })
        .await
        .unwrap();
    assert_eq!(result["models"], json!([]));
    assert_eq!(result["source"], "remote");
    assert_eq!(result["authoritativeEmpty"], true);
    assert_eq!(result["allowUnknownModels"], false);
}
#[tokio::test]
async fn malformed_model_payloads_never_claim_an_authoritative_empty_success() {
    let (_directory, client, provider) = fixture().await;
    client.push(200, json!({"models":[]}));
    let account = Account::inputs(&json!({"endpoint":"remote.test:3572","apiKey":"key"})).unwrap();
    assert!(provider
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: "local-key".into(),
            credentials: Some(account.saved()),
            network_proxy: None
        })
        .await
        .is_err());
}
#[tokio::test]
async fn per_account_network_policy_is_validated_before_the_remote_is_contacted() {
    let (_directory, client, provider) = fixture().await;
    let account = Account::inputs(&json!({"endpoint":"remote.test:3572","apiKey":"key"})).unwrap();
    assert!(provider
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: "local-key".into(),
            credentials: Some(account.saved()),
            network_proxy: Some(NetworkProxy::Proxy {
                url: "file:///tmp/not-a-proxy".into()
            })
        })
        .await
        .is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}
