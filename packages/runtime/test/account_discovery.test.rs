use super::*;
use std::path::PathBuf;

#[derive(Debug)]
struct DiscoveredAccount {
    fail: bool,
    missing_identity: bool,
}

#[async_trait]
impl ProviderAdapter for DiscoveredAccount {
    fn is_source(&self, credentials: &OAuthCredentials) -> bool {
        credentials.extras.contains_key("noraAccountSource")
    }

    async fn discover_accounts(
        &self,
        _client: &dyn HttpClient,
        _proxy: Option<&NetworkProxy>,
    ) -> PluginResult<Vec<OAuthCredentials>> {
        if self.fail {
            return Err(PluginError::transport(
                "The local sign-in could not be verified.",
            ));
        }
        let mut extras = BTreeMap::from([
            (
                "noraAccountSource".into(),
                json!({"kind":"fixtureCLI","path":"/local/auth.json"}).into(),
            ),
            ("email".into(), JsonValue::string("person@example.test")),
        ]);
        if !self.missing_identity {
            extras.insert("accountIdentity".into(), json!({"issuer":"https://identity.example.test","subject":"member-1","scope":"workspace-a"}).into());
        }
        Ok(vec![OAuthCredentials::new(
            OAuthProviderId::XAI,
            "",
            None,
            None,
            extras,
        )])
    }
}

async fn fixture(adapter: impl ProviderAdapter) -> BundledProvider {
    let plugin = BundledProvider::new(adapter);
    plugin.initialize(InitializeParams { subscription_accounts_enabled: true,
        installed_manifest: None,
        runtime_config: serde_json::Value::Null,
        api_version: API_VERSION,
        plugin_id: "fixture".into(),
        plugin_directory: PathBuf::new(),
        data_directory: PathBuf::new(),
        manifest: json!({"id":"fixture","name":"Fixture","api":"openai-responses","baseURL":"https://api.example.test","config":{"keyProvider":"fixture-key","oauthProvider":"fixture"}}),
    }).await.unwrap();
    plugin
}

fn context() -> CallContext {
    CallContext {
        subscription_accounts_enabled: true,
        account: String::new(),
        credentials: None,
        network_proxy: None,
        ..Default::default()
    }
}

#[tokio::test]
async fn discovery_returns_a_stable_identity_and_keeps_the_live_local_source_reference() {
    let plugin = fixture(DiscoveredAccount {
        fail: false,
        missing_identity: false,
    })
    .await;
    let result = plugin.discover_accounts(context()).await.unwrap();
    assert_eq!(result["accounts"].as_array().unwrap().len(), 1);
    let account = &result["accounts"][0];
    assert_eq!(account["sourceID"], "fixtureCLI");
    assert_eq!(
        account["credentials"]["accountIdentity"]["subject"],
        "member-1"
    );
    assert_eq!(
        account["credentials"]["accountIdentity"]["scope"],
        "workspace-a"
    );
    assert_eq!(
        account["credentials"]["extras"]["noraAccountSource"]["path"],
        "/local/auth.json"
    );
    assert_eq!(account["credentials"]["access"], "");
    assert!(account["credentials"]["refresh"].is_null());
}

#[tokio::test]
async fn a_discovery_failure_is_not_reported_as_an_empty_signed_out_list() {
    let plugin = fixture(DiscoveredAccount {
        fail: true,
        missing_identity: false,
    })
    .await;
    assert!(plugin.discover_accounts(context()).await.is_err());
    let plugin = fixture(DiscoveredAccount {
        fail: false,
        missing_identity: true,
    })
    .await;
    assert!(plugin.discover_accounts(context()).await.is_err());
    let plugin = fixture(crate::GenericProvider).await;
    assert_eq!(
        plugin.discover_accounts(context()).await.unwrap(),
        json!({"accounts":[]})
    );
}

#[tokio::test]
async fn account_enrichment_returns_private_credentials_separately_from_the_public_profile() {
    #[derive(Debug)]
    struct Profile;
    #[async_trait]
    impl ProviderAdapter for Profile {
        async fn enrich_account(
            &self,
            credentials: &mut OAuthCredentials,
            _client: &dyn HttpClient,
            _proxy: Option<&NetworkProxy>,
        ) -> PluginResult<()> {
            credentials.extras.insert(
                "accountIdentity".into(),
                json!({"issuer":"https://identity.example.test","subject":"member-1"}).into(),
            );
            credentials
                .extras
                .insert("email".into(), JsonValue::string("person@example.test"));
            Ok(())
        }
    }
    let result = fixture(Profile).await.accounts(CallContext { subscription_accounts_enabled: true,
        account:"saved-account".into(), network_proxy:None,
        credentials:Some(json!({"provider":"xAI","type":"oauth","access":"private-token","refresh":"private-refresh"})),
        ..Default::default()
    }).await.unwrap();
    assert_eq!(
        result["credentials"]["accountIdentity"]["subject"],
        "member-1"
    );
    assert_eq!(result["credentials"]["access"], "private-token");
    assert_eq!(result["accounts"][0]["email"], "person@example.test");
    let public = result["accounts"].to_string();
    assert!(!public.contains("private-token"));
    assert!(!public.contains("private-refresh"));
    assert!(!public.contains("member-1"));
}
