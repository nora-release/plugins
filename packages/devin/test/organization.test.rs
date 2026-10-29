use super::*;
use crate::fixture::{self, Client, Reply};

fn status(org: &str, name: &str, team: &str, remaining: u64) -> Value {
    json!({"userStatus":{"email":"m@x.dev","name":"sky","teamId":team,"teamsTier":"TEAMS_TIER_DEVIN_MAX","planStatus":{"planInfo":{"planName":"Max","billingStrategy":"BILLING_STRATEGY_QUOTA","devinInfo":{"orgId":org,"accountDisplayName":name}},"dailyQuotaRemainingPercent":remaining,"weeklyQuotaRemainingPercent":remaining}}})
}

fn legacy(key: &str) -> Value {
    json!({"type":"api","authMethod":"browser","key":key,"accountId":"m@x.dev","label":"m@x.dev","metadata":{"email":"m@x.dev","plan":"Devin Max","server":network::SERVER}})
}

async fn signin(key: &str, response: Value) -> Value {
    let client = Client::new(vec![Reply::json(response)]);
    let account = auth::finish(client.as_ref(), key.into(), network::SERVER.into(), None)
        .await
        .unwrap();
    let requests = client.requests.lock().unwrap();
    let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["metadata"]["apiKey"], key);
    account.credentials()
}

async fn plugin(client: Arc<Client>) -> Devin {
    plugin_with_persistence(client, Arc::new(fixture::Persistence::default())).await
}

async fn plugin_with_persistence(
    client: Arc<Client>,
    persistence: Arc<dyn nora_provider::account_import::CredentialPersistence>,
) -> Devin {
    let plugin = Devin {
        client: Some(client),
        persistence,
        ..Default::default()
    };
    plugin
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: Value::Null,
            api_version: 2,
            plugin_id: "devin".into(),
            plugin_directory: Default::default(),
            data_directory: Default::default(),
            manifest: serde_json::from_str(include_str!("../nora-plugin.json")).unwrap(),
        })
        .await
        .unwrap();
    plugin
}

#[tokio::test]
async fn organizations_under_one_email_are_named_and_identified_apart() {
    let first = signin(
        "private-org-1",
        status("org_1", "opt", "devin-team$account-1", 80),
    )
    .await;
    let second = signin(
        "private-org-2",
        status("org_2", "lab", "devin-team$account-2", 25),
    )
    .await;
    assert_eq!(first["email"], "m@x.dev");
    assert_eq!(first["displayName"], "m@x.dev · opt");
    assert_eq!(second["displayName"], "m@x.dev · lab");
    assert_eq!(first["metadata"]["orgId"], "org_1");
    assert_eq!(first["metadata"]["teamId"], "devin-team$account-1");
    assert_eq!(second["metadata"]["orgId"], "org_2");
    assert_eq!(first["organizationId"], "org_1");
    assert_eq!(second["organizationId"], "org_2");
    assert_ne!(first["organizationId"], second["organizationId"]);
    assert!(first.get("accountIdentity").is_none());
}

#[tokio::test]
async fn same_organization_relogin_and_display_rename_keep_the_identity() {
    let first = signin("private-old", status("org_1", "opt", "team-1", 80)).await;
    let mut renamed = status("org_1", "New name", "team-1", 80);
    renamed["userStatus"]["email"] = json!("M@X.DEV");
    let second = signin("private-new", renamed).await;
    assert_eq!(first["organizationId"], "org_1");
    assert_eq!(first["organizationId"], second["organizationId"]);
    assert_eq!(second["displayName"], "M@X.DEV · New name");
    assert_ne!(first["key"], second["key"]);
}

#[tokio::test]
async fn organization_name_and_team_fallbacks_match_the_reference() {
    for (response, display, scope, team) in [
        (
            json!({"userStatus":{"email":" m@x.dev ","teamId":" team-1 ","teamName":" Status team ","planStatus":{"planInfo":{"teamName":" Plan team "}}}}),
            "m@x.dev · Plan team",
            Some("team-1"),
            Some("team-1"),
        ),
        (
            json!({"userStatus":{"email":"m@x.dev","teamName":"Status team","teamId":"team-2"}}),
            "m@x.dev · Status team",
            Some("team-2"),
            Some("team-2"),
        ),
        (
            json!({"userStatus":{"email":"m@x.dev","teamId":"team-3","planStatus":{"planInfo":{"devinInfo":{"orgId":"org-3","accountDisplayName":"M@X.DEV"}}}}}),
            "m@x.dev",
            Some("org-3"),
            Some("team-3"),
        ),
        (
            json!({"userStatus":{"email":"m@x.dev"}}),
            "m@x.dev",
            None,
            None,
        ),
        (
            json!({"userStatus":{"name":"handle","teamId":"team-4"}}),
            "handle",
            Some("team-4"),
            Some("team-4"),
        ),
    ] {
        let saved = signin("private-key", response).await;
        assert_eq!(saved["displayName"], display);
        assert_eq!(saved["organizationId"].as_str(), scope);
        assert_eq!(saved["metadata"]["teamId"].as_str(), team);
        assert_eq!(
            Account::parse(&saved).unwrap().credentials()["metadata"],
            saved["metadata"]
        );
    }
}

#[tokio::test]
async fn refresh_enriches_an_old_signin_with_its_organization_and_returns_the_same_profile() {
    let client = Client::new(vec![Reply::json(status("org_1", "opt", "team-1", 80))]);
    let plugin = plugin(client.clone()).await;
    let refreshed = plugin
        .refresh(json!({"credentials":legacy("private-org-1")}))
        .await
        .unwrap();
    let saved = &refreshed["credentials"];
    assert_eq!(saved["key"], "private-org-1");
    assert_eq!(saved["organizationId"], "org_1");
    let profile = plugin
        .accounts(CallContext {
            account: "chosen".into(),
            credentials: Some(saved.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(profile[0]["id"], "chosen");
    assert_eq!(profile[0]["displayName"], "m@x.dev · opt");
    assert_eq!(profile[0]["email"], "m@x.dev");
    assert_eq!(profile[0]["orgId"], "org_1");
    assert_eq!(profile[0]["teamId"], "team-1");
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn usage_returns_live_organization_details_and_keeps_quota_pools_separate() {
    let client = Client::new(vec![
        Reply::json(status("org_1", "opt", "team-1", 80)),
        Reply::bytes(fixture::configs()),
        Reply::json(status("org_2", "lab", "team-2", 25)),
        Reply::bytes(fixture::configs()),
    ]);
    let plugin = plugin(client.clone()).await;
    for (id, key, org, display, used) in [
        ("first", "private-org-1", "org_1", "m@x.dev · opt", 20.0),
        ("second", "private-org-2", "org_2", "m@x.dev · lab", 75.0),
    ] {
        let value = plugin
            .usage(CallContext {
                account: id.into(),
                credentials: Some(legacy(key)),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(value["account"]["id"], id);
        assert_eq!(value["account"]["email"], "m@x.dev");
        assert_eq!(value["account"]["displayName"], display);
        assert_eq!(value["account"]["orgId"], org);
        assert_eq!(value["items"][0]["usedPercent"], used);
    }
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for (index, key) in [(0, "private-org-1"), (2, "private-org-2")] {
        let body: Value = serde_json::from_slice(requests[index].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["metadata"]["apiKey"], key);
    }
}

#[tokio::test]
async fn account_listing_enriches_and_saves_an_older_profile() {
    let client = Client::new(vec![Reply::json(status("org_1", "opt", "team-1", 80))]);
    let saved = Arc::new(fixture::Persistence::default());
    let plugin = plugin_with_persistence(client.clone(), saved.clone()).await;
    let profile = plugin
        .accounts(CallContext {
            account: "chosen".into(),
            credentials: Some(legacy("private-one")),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(profile[0]["email"], "m@x.dev");
    assert_eq!(profile[0]["displayName"], "m@x.dev · opt");
    assert_eq!(profile[0]["orgId"], "org_1");
    let saved = saved.0.lock().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["key"], "private-one");
    assert_eq!(saved[0]["metadata"]["orgId"], "org_1");
    assert_eq!(saved[0]["metadata"]["teamId"], "team-1");
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn source_key_rotation_and_organization_switch_update_the_same_account() {
    let path = fixture::temp("organization-source.toml");
    let contents =
        "windsurf_api_key='private-rotated'\napi_server_url='https://enterprise.example.test'\n";
    std::fs::write(&path, contents).unwrap();
    let mut initial = signin("private-original", status("org_1", "opt", "team-1", 80)).await;
    initial.as_object_mut().unwrap().remove("key");
    initial["type"] = json!("oauth");
    initial["authMethod"] = json!("cliSource");
    initial["source"] = json!({"path":path,"kind":"file"});
    let client = Client::new(vec![
        Reply::json(status("org_2", "lab", "team-2", 25)),
        Reply::json(status("org_2", "New lab", "team-2", 20)),
        Reply::bytes(fixture::configs()),
    ]);
    let persisted = Arc::new(fixture::Persistence::default());
    let plugin = plugin_with_persistence(client.clone(), persisted.clone()).await;
    let refreshed = plugin
        .refresh(json!({"credentials":initial}))
        .await
        .unwrap();
    let saved = &refreshed["credentials"];
    assert_eq!(saved["accountId"], "m@x.dev");
    assert_eq!(saved["displayName"], "m@x.dev · lab");
    assert_eq!(saved["organizationId"], "org_2");
    assert_eq!(saved["metadata"]["orgId"], "org_2");
    assert_eq!(saved["metadata"]["teamId"], "team-2");
    assert_eq!(saved["endpoint"], "https://enterprise.example.test");
    assert_eq!(saved["source"], initial["source"]);
    assert!(!saved.to_string().contains("private-"));
    let value = plugin
        .usage(CallContext {
            account: "chosen".into(),
            credentials: Some(saved.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(value["account"]["id"], "chosen");
    assert_eq!(value["account"]["displayName"], "m@x.dev · New lab");
    assert_eq!(value["items"][0]["usedPercent"], 80.0);
    let persisted = persisted.0.lock().unwrap();
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0]["displayName"], "m@x.dev · New lab");
    assert_eq!(persisted[0]["source"], initial["source"]);
    assert!(!persisted[0].to_string().contains("private-"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for index in [0, 1] {
        assert!(requests[index]
            .url
            .starts_with("https://enterprise.example.test/"));
        let body: Value = serde_json::from_slice(requests[index].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["metadata"]["apiKey"], "private-rotated");
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn partial_status_refreshes_the_visible_name_without_erasing_known_organization_ids() {
    let saved = signin("private-one", status("org_1", "opt", "team-1", 80)).await;
    let client = Client::new(vec![Reply::json(json!({"userStatus":{"email":"m@x.dev"}}))]);
    let plugin = plugin(client).await;
    let refreshed = plugin.refresh(json!({"credentials":saved})).await.unwrap();
    let saved = &refreshed["credentials"];
    assert_eq!(saved["displayName"], "m@x.dev");
    assert_eq!(saved["email"], "m@x.dev");
    assert_eq!(saved["metadata"]["orgId"], "org_1");
    assert_eq!(saved["metadata"]["teamId"], "team-1");
    assert_eq!(saved["metadata"]["plan"], "Devin Max");
}

struct UnavailablePersistence;
#[async_trait]
impl nora_provider::account_import::CredentialPersistence for UnavailablePersistence {
    async fn save(&self, _: Value) -> PluginResult<()> {
        Err(PluginError::transport("Profile could not be saved"))
    }
}

#[tokio::test]
async fn profile_save_failure_does_not_hide_successfully_read_usage() {
    let client = Client::new(vec![
        Reply::json(status("org_1", "opt", "team-1", 80)),
        Reply::bytes(fixture::configs()),
    ]);
    let plugin = plugin_with_persistence(client, Arc::new(UnavailablePersistence)).await;
    let usage = plugin
        .usage(CallContext {
            account: "chosen".into(),
            credentials: Some(legacy("private-one")),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(usage["account"]["displayName"], "m@x.dev · opt");
    assert_eq!(usage["items"][0]["usedPercent"], 20.0);
}

#[tokio::test]
async fn an_owned_account_keeps_its_saved_profile_when_quota_status_omits_identity() {
    let saved = signin("private-one", status("org_1", "opt", "team-1", 80)).await;
    let mut quota = status("org_1", "opt", "team-1", 40);
    let account = quota["userStatus"].as_object_mut().unwrap();
    account.remove("email");
    account.remove("name");
    let client = Client::new(vec![Reply::json(quota), Reply::bytes(fixture::configs())]);
    let persisted = Arc::new(fixture::Persistence::default());
    let plugin = plugin_with_persistence(client, persisted.clone()).await;
    let usage = plugin
        .usage(CallContext {
            account: "chosen".into(),
            credentials: Some(saved),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(usage["account"]["email"], "m@x.dev");
    assert_eq!(usage["account"]["displayName"], "m@x.dev · opt");
    assert_eq!(usage["account"]["orgId"], "org_1");
    assert_eq!(usage["items"][0]["usedPercent"], 60.0);
    assert!(persisted.0.lock().unwrap().is_empty());
}
