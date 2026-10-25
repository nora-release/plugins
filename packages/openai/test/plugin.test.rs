use super::*;
use base64::Engine;
use nora_provider_auth::codex_source;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "nora-openai-wrapper-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn codex(&self, account: &str, marker: &str) -> (PathBuf, String) {
        let claims = json!({"exp":nora_ai::oauth::now_millis()/1000+3600,"sub":"fixture-user","nonce":marker,"https://api.openai.com/auth":{"chatgpt_account_id":account,"chatgpt_user_id":"fixture-user","chatgpt_plan_type":"pro"},"https://api.openai.com/profile":{"email":"person@example.invalid"}});
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&claims).unwrap());
        let token = format!("header.{encoded}.signature");
        let path = self.0.join("auth.json");
        std::fs::write(&path,serde_json::to_vec(&json!({"auth_mode":"chatgpt","tokens":{"account_id":account,"access_token":token,"refresh_token":"fixture-refresh"}})).unwrap()).unwrap();
        (path, token)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn plugin(directory: &Directory) -> OpenAIPlugin {
    let plugin = OpenAIPlugin::default();
    plugin
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: nora_provider_plugin_sdk::API_VERSION,
            plugin_id: "openai".into(),
            plugin_directory: directory.0.clone(),
            data_directory: directory.0.join("data"),
            manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
        })
        .await
        .unwrap();
    plugin
}
fn oauth() -> Value {
    json!({"type":"oauth","provider":"openAICodex","credentialProvider":"openAICodex","access":"fixture-chatgpt-access","extras":{"accountId":"workspace-from-credentials"}})
}

#[tokio::test]
async fn reset_context_requires_confirmation_a_saved_account_and_chatgpt_credentials() {
    let directory = Directory::new();
    let plugin = plugin(&directory).await;
    for params in [
        json!({"account":"saved","credentials":oauth()}),
        json!({"confirmed":false,"account":"saved","credentials":oauth()}),
        json!({"confirmed":true,"credentials":oauth()}),
        json!({"confirmed":true,"account":" ","credentials":oauth()}),
        json!({"confirmed":true,"account":"saved\nother","credentials":oauth()}),
        json!({"confirmed":true,"account":"saved","credentials":{"type":"apiKey","key":"fixture-api-key"}}),
        json!({"confirmed":true,"account":"saved","credentials":{"type":"oauth","provider":"xAI","access":"fixture-other-provider"}}),
    ] {
        assert!(plugin.reset_context(&params).await.is_err());
    }
    assert!(OpenAIPlugin::default()
        .reset_context(&json!({"confirmed":true,"account":"saved","credentials":oauth()}))
        .await
        .is_err());
}

#[tokio::test]
async fn reset_context_uses_the_credentials_workspace_instead_of_the_storage_record_id() {
    let directory = Directory::new();
    let plugin = plugin(&directory).await;
    let (auth, _unused_http_client) = plugin
        .reset_context(
            &json!({"confirmed":true,"account":"different-storage-record","credentials":oauth()}),
        )
        .await
        .unwrap();
    assert_eq!(auth.credential_provider_id.as_deref(), Some("openai-codex"));
    assert_eq!(auth.token.as_deref(), Some("fixture-chatgpt-access"));
    assert_eq!(
        auth.headers["chatgpt-account-id"],
        "workspace-from-credentials"
    );
}

#[tokio::test]
async fn reset_context_reads_a_current_bound_codex_token_without_copying_it_into_the_saved_reference(
) {
    let directory = Directory::new();
    let (path, _old_token) = directory.codex("fixture-workspace", "old");
    let bound = codex_source::bind(&path).unwrap();
    let saved = auth::encode(&bound, "openai-codex", true);
    let (_, new_token) = directory.codex("fixture-workspace", "new");
    let before = std::fs::read(&path).unwrap();
    let plugin = plugin(&directory).await;
    // A future expiry guarantees the source resolver does not contact OAuth.
    let (auth, _unused_http_client) = plugin
        .reset_context(&json!({"confirmed":true,"account":"saved-source","credentials":saved}))
        .await
        .unwrap();
    assert_eq!(auth.token.as_deref(), Some(new_token.as_str()));
    assert_eq!(auth.headers["chatgpt-account-id"], "fixture-workspace");
    assert_eq!(saved["access"], "");
    assert!(saved["refresh"].is_null());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn reset_context_refuses_a_local_source_that_has_switched_to_another_account() {
    let directory = Directory::new();
    let (path, _) = directory.codex("first-workspace", "first");
    let bound = codex_source::bind(&path).unwrap();
    let saved = auth::encode(&bound, "openai-codex", true);
    directory.codex("other-workspace", "other");
    let plugin = plugin(&directory).await;
    assert!(plugin
        .reset_context(&json!({"confirmed":true,"account":"saved-source","credentials":saved}))
        .await
        .is_err());
}

#[tokio::test]
async fn wrapper_keeps_api_authorization_account_profiles_and_usage_methods_available() {
    let directory = Directory::new();
    let plugin = plugin(&directory).await;
    let methods = plugin.auth_methods(CallContext::default()).await.unwrap();
    assert!(methods
        .as_array()
        .unwrap()
        .iter()
        .any(|method| method["id"] == "codexCLI"));
    let authorized = plugin
        .authorize(json!({"method":"apiKey","inputs":{"apiKey":"fixture-api-key"}}))
        .await
        .unwrap();
    assert_eq!(authorized["credentials"]["access"], "fixture-api-key");
    assert_eq!(authorized["credentials"]["type"], "apiKey");
    let context = CallContext {
        subscription_accounts_enabled: true,
        account: "saved-api".into(),
        credentials: Some(authorized["credentials"].clone()),
        network_proxy: None,
    };
    let accounts = plugin.accounts(context.clone()).await.unwrap();
    assert_eq!(accounts["accounts"][0]["id"], "saved-api");
    let usage = plugin.usage(context).await.unwrap();
    assert_eq!(usage["available"], false);
    assert_eq!(
        plugin
            .refresh(json!({"account":"saved-api","credentials":authorized["credentials"]}))
            .await
            .unwrap(),
        json!({"changed":false})
    );
}

#[tokio::test]
async fn ordinary_extension_methods_and_invalid_callbacks_are_forwarded_without_consuming() {
    let directory = Directory::new();
    let plugin = plugin(&directory).await;
    let expected = plugin
        .inner
        .call("unregisteredMethod", json!({}))
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        plugin
            .call("unregisteredMethod", json!({}))
            .await
            .unwrap_err()
            .to_string(),
        expected
    );
    assert!(plugin
        .callback(json!({"session":"not-started"}))
        .await
        .is_err());
    assert!(plugin
        .call(
            "consumeReset",
            json!({"confirmed":false,"account":"saved","credentials":oauth()})
        )
        .await
        .is_err());
}
