use super::mixed_accounts_tests::Endpoint;
use super::*;
use crate::fixtures::package;
use base64::Engine;
use nora_ai::AuthScheme;
use std::path::PathBuf;
fn manifest(plan: &str, api: &str, native: &str) -> Value {
    let header = serde_json::to_value(AuthScheme::ApiKeyHeader {
        name: "x-plan-key".into(),
    })
    .unwrap();
    json!({"id":"profile-fixture","name":"Profile Fixture","api":"openai-completions","baseURL":format!("{native}/native/v1"),"authMethods":[{"id":"apiKey","type":"api","label":"API key"},{"id":"oauth","type":"oauth","label":"Sign in"}],"config":{"keyProvider":"api-key","connectionOptions":true,"headers":{"X-Native":"native-identity"},"options":[{"id":"testOption","type":"boolean","default":false}],"testOption":false,"profiles":[{"id":"plan","name":"Plan","bases":[{"api":"openai-completions","baseURL":format!("{plan}/plan/v1")}],"apiKeyAuth":header},{"id":"api","name":"API","bases":[{"api":"openai-completions","baseURL":format!("{api}/api/v1")}],"apiKeyAuth":"bearer"}]}})
}
async fn plugin(original: Value, manifest: Value, patch: Value) -> BundledProvider {
    let plugin = BundledProvider::default();
    plugin
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            api_version: API_VERSION,
            plugin_id: "profile-fixture".into(),
            plugin_directory: PathBuf::new(),
            data_directory: std::env::temp_dir().join("nora-profile-connections"),
            manifest,
            installed_manifest: Some(original),
            runtime_config: patch,
        })
        .await
        .unwrap();
    plugin
}
fn credentials(profile: &str) -> Value {
    json!({"type":"apiKey","access":"profile-private","extras":{"profile":profile}})
}
fn context(credentials: Value) -> CallContext {
    CallContext {
        subscription_accounts_enabled: true,
        account: "fixture".into(),
        credentials: Some(credentials),
        network_proxy: None,
        ..Default::default()
    }
}
fn request(credentials: Value, url: String) -> FetchRequest {
    FetchRequest {
        caller_user_agent: None,
        constraints: Default::default(),
        subscription_accounts_enabled: true,
        account: "fixture".into(),
        credentials: Some(credentials),
        network_proxy: None,
        url,
        method: "POST".into(),
        headers: BTreeMap::new(),
        metadata: BTreeMap::new(),
        body: Some(
            base64::engine::general_purpose::STANDARD
                .encode(br#"{"model":"live-model","messages":[]}"#),
        ),
        body_file: None,
    }
}

#[tokio::test]
async fn local_profiles_list_models_and_send_each_protocol_to_the_custom_server() {
    for id in ["ollama", "omlx", "mlx-serve", "lmstudio"] {
        let endpoint = Endpoint::start().await;
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join(id)
                    .join("nora-plugin.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let plugin = plugin(manifest.clone(), manifest, json!({})).await;
        let connected = plugin.authorize(json!({"authMethodID":"apiKey", "inputs":{"endpoint":format!("{}/v1",endpoint.base)}})).await.unwrap();
        let credentials = connected["credentials"].clone();
        let result = plugin.models(context(credentials.clone())).await.unwrap();
        assert_eq!(result["source"], "remote", "{id}: {result}");
        assert_eq!(
            result["models"][0]["baseUrl"],
            format!("{}/v1", endpoint.base)
        );
        let mut paths = vec!["/v1/chat/completions"];
        if id != "lmstudio" {
            paths.push("/v1/messages");
        }
        if matches!(id, "omlx" | "mlx-serve") {
            paths.push("/v1/responses");
        }
        for path in &paths {
            assert_eq!(
                plugin
                    .fetch(request(
                        credentials.clone(),
                        format!("{}{path}", endpoint.base)
                    ))
                    .await
                    .unwrap()
                    .status,
                200,
                "{id} {path}"
            );
        }
        let captured = endpoint.requests.lock().unwrap();
        assert!(
            captured
                .iter()
                .filter(|request| request.starts_with("GET "))
                .filter_map(|request| request.split_whitespace().nth(1))
                .any(|target| target.split('?').next() == Some("/v1/models")),
            "{id}: {captured:?}"
        );
        for path in paths {
            assert!(
                captured
                    .iter()
                    .any(|request| request.starts_with(&format!("POST {path} "))),
                "{id} {path}: {captured:?}"
            );
        }
    }
}

#[tokio::test]
async fn adding_and_changing_an_additional_api_keeps_the_primary_request_working() {
    let primary = Endpoint::start().await;
    let additional = Endpoint::start().await;
    let changed = Endpoint::start().await;
    let original = package(json!({
        "id":"profile-fixture","name":"Fixture","api":"openai-completions",
        "baseURL":format!("{}/chat/v1",primary.base),
        "config":{"keyProvider":"api-key","connectionOptions":true}
    }));
    let value = json!({"type":"apiKey","access":"private-fixture-key"});
    for extra in [&additional, &changed] {
        let address = format!("{}/anthropic/v1", extra.base);
        let plugin = plugin(
            original.clone(),
            original.clone(),
            json!({
                "endpoints":{"anthropic-messages":address}
            }),
        )
        .await;
        let models = plugin.models(context(value.clone())).await.unwrap();
        assert_eq!(models["models"][0]["api"], "openai-completions");
        assert_eq!(
            models["models"][0]["baseUrl"],
            format!("{}/chat/v1", primary.base)
        );
        for url in [
            format!("{}/chat/v1/chat/completions", primary.base),
            format!("{address}/messages"),
        ] {
            let response = plugin.fetch(request(value.clone(), url)).await.unwrap();
            assert_eq!(response.status, 200);
        }
    }
    assert_eq!(
        primary
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.starts_with("POST /chat/v1/chat/completions "))
            .count(),
        2
    );
    for endpoint in [&additional, &changed] {
        assert_eq!(
            endpoint
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.starts_with("POST /anthropic/v1/messages "))
                .count(),
            1
        );
    }

    let cleared = plugin(original.clone(), original, json!({"endpoints":{}})).await;
    assert_eq!(
        cleared
            .fetch(request(
                value.clone(),
                format!("{}/chat/v1/chat/completions", primary.base)
            ))
            .await
            .unwrap()
            .status,
        200
    );
    assert!(cleared
        .fetch(request(
            value,
            format!("{}/anthropic/v1/messages", additional.base)
        ))
        .await
        .is_err());
}

#[tokio::test]
async fn default_and_auto_headers_preserve_each_profiles_real_endpoint_and_authentication() {
    let plan = Endpoint::start().await;
    let api = Endpoint::start().await;
    let native = Endpoint::start().await;
    let original = package(manifest(&plan.base, &api.base, &native.base));
    for patch in [json!({}), json!({"authHeader":"auto"})] {
        let plugin = plugin(original.clone(), original.clone(), patch).await;
        for (profile, endpoint, header) in [
            ("plan", &plan, "x-plan-key: profile-private"),
            ("api", &api, "authorization: Bearer profile-private"),
        ] {
            let value = credentials(profile);
            let definition = plugin
                .configuration()
                .unwrap()
                .account_definition(Some(&value))
                .unwrap();
            let path = if profile == "plan" { "plan" } else { "api" };
            assert_eq!(definition.base_url, format!("{}/{path}/v1", endpoint.base));
            let models = plugin.models(context(value.clone())).await.unwrap();
            assert_eq!(models["source"], "remote", "{models}");
            plugin
                .fetch(request(
                    value,
                    format!("{}/{path}/v1/chat/completions", endpoint.base),
                ))
                .await
                .unwrap();
            let sent = endpoint.requests.lock().unwrap();
            assert!(sent
                .last()
                .unwrap()
                .to_ascii_lowercase()
                .contains(&header.to_ascii_lowercase()));
        }
    }
    assert_eq!(plan.requests.lock().unwrap().len(), 4);
    assert_eq!(api.requests.lock().unwrap().len(), 4);
    assert!(native.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn explicit_user_endpoints_and_headers_apply_after_both_profiles_while_sign_ins_keep_vendor_defaults(
) {
    let plan = Endpoint::start().await;
    let api = Endpoint::start().await;
    let native = Endpoint::start().await;
    let custom = Endpoint::start().await;
    let original = manifest(&plan.base, &api.base, &native.base);
    let patch = json!({"baseURL":format!("{}/base/v1",custom.base),"api":"anthropic-messages","authHeader":"X-Draft-Key","endpoints":{"anthropic-messages":format!("{}/endpoint/v1",custom.base)},"headers":{"X-Draft-Key":"header-private"},"extraBody":{"!tier":"custom-tier"},"modelsURL":format!("{}/catalog?token=query-private",custom.base),"balanceURL":format!("{}/balance",custom.base),"balancePath":"$balance"});
    let mut configured = original.clone();
    configured["baseURL"] = patch["baseURL"].clone();
    configured["api"] = patch["api"].clone();
    configured["config"]["headers"] = patch["headers"].clone();
    configured["config"]["testOption"] = json!(true);
    configured["config"]["apiKeyAuth"] = serde_json::to_value(AuthScheme::ApiKeyHeader {
        name: "X-Draft-Key".into(),
    })
    .unwrap();
    for key in [
        "authHeader",
        "modelsURL",
        "balanceURL",
        "balancePath",
        "extraBody",
    ] {
        configured["config"][key] = patch[key].clone();
    }
    configured["config"]["modelDiscovery"] = json!({"defaultBaseURL":configured["baseURL"],"bases":[{"api":"anthropic-messages","baseURL":patch["endpoints"]["anthropic-messages"]}]});
    let plugin = plugin(package(original), package(configured), patch).await;
    for profile in ["plan", "api"] {
        let value = credentials(profile);
        let definition = plugin
            .configuration()
            .unwrap()
            .account_definition(Some(&value))
            .unwrap();
        assert_eq!(definition.base_url, format!("{}/base/v1", custom.base));
        assert_eq!(definition.api, "anthropic-messages");
        assert!(
            matches!(definition.config.api_key_auth,Some(AuthScheme::ApiKeyHeader{ref name}) if name=="X-Draft-Key")
        );
        let models = plugin.models(context(value.clone())).await.unwrap();
        assert_eq!(models["source"], "remote", "{models}");
        assert_eq!(
            models["models"][0]["baseUrl"],
            format!("{}/endpoint/v1", custom.base)
        );
        let usage = plugin.usage(context(value.clone())).await.unwrap();
        assert_eq!(usage["balances"][0]["amount"], 2.0);
        plugin
            .fetch(request(
                value,
                format!("{}/endpoint/v1/messages", custom.base),
            ))
            .await
            .unwrap();
    }
    for kind in ["oauth", "fixtureCLI"] {
        let value = json!({"type":kind,"access":"sign-in-private","extras":{"profile":"api"}});
        let definition = plugin
            .configuration()
            .unwrap()
            .account_definition(Some(&value))
            .unwrap();
        assert_eq!(definition.base_url, format!("{}/native/v1", native.base));
        assert_eq!(definition.api, "openai-completions");
        assert_eq!(definition.config.extra["testOption"], true);
        assert!(definition.config.models_url.is_empty());
        let models = plugin.models(context(value.clone())).await.unwrap();
        assert_eq!(models["source"], "remote", "{models}");
        plugin
            .fetch(request(
                value,
                format!("{}/native/v1/chat/completions", native.base),
            ))
            .await
            .unwrap();
    }
    let sent = custom.requests.lock().unwrap();
    assert_eq!(sent.len(), 6);
    for request in sent.iter() {
        assert!(request
            .to_ascii_lowercase()
            .contains("x-draft-key: header-private"));
        assert!(!request.contains("sign-in-private"));
    }
    assert!(sent[2].contains("custom-tier"));
    assert!(sent[5].contains("custom-tier"));
    let sent = native.requests.lock().unwrap();
    assert_eq!(sent.len(), 4);
    for request in sent.iter() {
        assert!(request.contains("sign-in-private"));
        assert!(request
            .to_ascii_lowercase()
            .contains("x-native: native-identity"));
        assert!(!request.contains("header-private"));
        assert!(!request.contains("custom-tier"));
    }
    assert!(plan.requests.lock().unwrap().is_empty());
    assert!(api.requests.lock().unwrap().is_empty());
}
