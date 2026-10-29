use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use futures::StreamExt;
use nora_ai::{HttpClientError, HttpStreamingResponse};
use std::{collections::VecDeque, path::PathBuf, sync::Mutex as StdMutex};

struct Reply {
    path: String,
    method: HttpMethod,
    status: u16,
    headers: BTreeMap<String, String>,
    value: Value,
}
fn reply(path: &str, value: Value) -> Reply {
    Reply {
        path: path.into(),
        method: HttpMethod::Get,
        status: 200,
        headers: BTreeMap::new(),
        value,
    }
}
fn post(path: &str, value: Value) -> Reply {
    Reply {
        method: HttpMethod::Post,
        ..reply(path, value)
    }
}
struct Client {
    requests: StdMutex<Vec<HttpRequest>>,
    replies: StdMutex<VecDeque<Reply>>,
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("No fixture response remains");
        assert!(
            request.url.ends_with(&reply.path),
            "{} != {}",
            request.url,
            reply.path
        );
        assert_eq!(request.method, reply.method);
        self.requests.lock().unwrap().push(request);
        let bytes = serde_json::to_vec(&reply.value).unwrap();
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: reply.headers,
            body: Box::pin(futures::stream::once(async move { Ok(bytes.into()) })),
        })
    }
}
struct Store;
#[async_trait]
impl core::CredentialStore for Store {
    async fn save(&self, _: Value) -> PluginResult<()> {
        panic!("An API-key provider must never invent a token rotation")
    }
}
fn manifest(family: DecisionFamily) -> Value {
    let value = match family {
        DecisionFamily::Vercel => include_str!("../../vercel-jev/nora-plugin.json"),
        DecisionFamily::Cloudflare => include_str!("../../cloudflare-jev/nora-plugin.json"),
        DecisionFamily::Bailian => include_str!("../../bailian-decision/nora-plugin.json"),
        DecisionFamily::Qwen => include_str!("../../qwen-decision/nora-plugin.json"),
    };
    serde_json::from_str(value).unwrap()
}
fn setup(family: DecisionFamily, replies: Vec<Reply>) -> (DecisionProvider, Arc<Client>) {
    let client = Arc::new(Client {
        requests: StdMutex::new(Vec::new()),
        replies: StdMutex::new(replies.into()),
    });
    let mut provider = DecisionProvider::new(family);
    provider.core = Core::with_dependencies(family.id(), client.clone(), Arc::new(Store));
    provider
        .core
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: 2,
            plugin_id: family.id().into(),
            plugin_directory: PathBuf::new(),
            data_directory: PathBuf::new(),
            manifest: manifest(family),
        })
        .unwrap();
    (provider, client)
}
fn credentials(family: DecisionFamily, base: &str) -> Value {
    json!({"provider":family.id(),"type":"apiKey","access":"private-decision-key","baseUrl":base})
}
fn question(model: &str) -> Value {
    json!({"model":model,"state":{"message":"Choose a route","previous_intent":"coding"},"questions":{"q":{"type":"choice","criteria":{"code":"Coding","other":"Other"}}},"providerOptions":{"gateway":{"zeroDataRetention":true}}})
}
fn request(family: DecisionFamily, base: &str, model: &str) -> FetchRequest {
    FetchRequest {
        caller_user_agent: None,
        constraints: Default::default(),
        subscription_accounts_enabled: true,
        account: "saved-account".into(),
        credentials: Some(credentials(family, base)),
        network_proxy: None,
        url: format!("{base}/systemone"),
        method: "POST".into(),
        headers: BTreeMap::from([
            (
                "authorization".into(),
                "Bearer untrusted-client-secret".into(),
            ),
            ("cookie".into(), "untrusted-cookie".into()),
        ]),
        metadata: BTreeMap::from([("sessionID".into(), "private-session-id".into())]),
        body: Some(STANDARD.encode(serde_json::to_vec(&question(model)).unwrap())),
        body_file: None,
    }
}
fn answer() -> Value {
    json!({"answers":{"q":{"choice":"code","confidence":0.8,"probabilities":{"code":0.9,"other":0.1}}},"usage":{"input_tokens":23,"output_tokens":0},"provider_metadata":{"gateway":{"cost":"0.001"}}})
}
async fn body(mut response: FetchResponse) -> Value {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.next().await {
        bytes.extend(chunk.unwrap());
    }
    serde_json::from_slice(&bytes).unwrap()
}
fn context(credentials: Value) -> CallContext {
    CallContext {
        subscription_accounts_enabled: true,
        account: "saved-account".into(),
        credentials: Some(credentials),
        network_proxy: None,
        ..Default::default()
    }
}

#[tokio::test]
async fn vercel_authentication_is_checked_against_the_actual_typesafe_model_api() {
    let (provider, client) = setup(
        DecisionFamily::Vercel,
        vec![reply(
            "/typesafe/v1/models",
            json!({"models":[{"name":"typesafe-ai/jev"}]}),
        )],
    );
    let result = provider
        .authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":" private-decision-key "}}))
        .await
        .unwrap();
    assert_eq!(result["credentials"]["access"], "private-decision-key");
    assert_eq!(
        result["credentials"]["baseUrl"],
        "https://ai-gateway.vercel.sh/typesafe/v1"
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer private-decision-key"
    );
    assert_eq!(requests[0].method, HttpMethod::Get);
}

#[tokio::test]
async fn refused_keys_do_not_produce_successful_credentials_or_expose_key_text() {
    let mut refused = reply(
        "/typesafe/v1/models",
        json!({"message":"private-decision-key refused"}),
    );
    refused.status = 401;
    let (provider, _) = setup(DecisionFamily::Vercel, vec![refused]);
    let error = provider
        .authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"private-decision-key"}}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "authentication_required");
    assert!(!error.message.contains("private-decision-key"));
}

#[tokio::test]
async fn vercel_discovery_is_authoritative_and_keeps_real_model_capabilities_and_prices() {
    let (provider, client) = setup(
        DecisionFamily::Vercel,
        vec![
            reply(
                "/typesafe/v1/models",
                json!({"data":[{"id":"custom-live-decision","name":"Live","context_window":65536,"pricing":{"input":"0.000000042","output":"0"}},{"id":"custom-live-decision"}]}),
            ),
            reply("/typesafe/v1/models", json!({"data":[]})),
        ],
    );
    let creds = credentials(
        DecisionFamily::Vercel,
        "https://ai-gateway.vercel.sh/typesafe/v1",
    );
    let value = provider.models(context(creds.clone())).await.unwrap();
    assert_eq!(value["source"], "remote");
    assert_eq!(value["models"].as_array().unwrap().len(), 1);
    assert_eq!(value["models"][0]["id"], "custom-live-decision");
    assert_eq!(value["models"][0]["info"]["decision"], true);
    assert_eq!(value["models"][0]["info"]["chat"], false);
    assert!((value["models"][0]["cost"]["input"].as_f64().unwrap() - 0.042).abs() < 0.000001);
    assert!(provider.models(context(creds)).await.unwrap()["models"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn vercel_native_fetch_keeps_real_questions_answers_extensions_and_usage() {
    let base = "https://ai-gateway.vercel.sh/typesafe/v1";
    let (provider, client) = setup(
        DecisionFamily::Vercel,
        vec![post("/typesafe/v1/systemone", answer())],
    );
    let result = body(
        provider
            .fetch(request(DecisionFamily::Vercel, base, "typesafe-ai/jev"))
            .await
            .unwrap(),
    )
    .await;
    let mut expected = answer();
    expected["usage"]["cost"] = json!({"total":0.001});
    assert_eq!(result, expected);
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap(),
        question("typesafe-ai/jev")
    );
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer private-decision-key"
    );
    assert!(!requests[0].headers.contains_key("cookie"));
    assert!(!format!("{:?}", requests[0]).contains("untrusted-client-secret"));
    assert!(!format!("{:?}", requests[0]).contains("private-session-id"));
}

#[tokio::test]
async fn vercel_balance_is_actual_lifetime_spend_without_an_invented_reset_or_budget_percentage() {
    let (provider, client) = setup(
        DecisionFamily::Vercel,
        vec![reply(
            "/v1/credits",
            json!({"balance":"5.25","total_used":"12.75"}),
        )],
    );
    let value = provider
        .usage(context(credentials(
            DecisionFamily::Vercel,
            "https://ai-gateway.vercel.sh/typesafe/v1",
        )))
        .await
        .unwrap();
    assert_eq!(value["available"], true);
    assert_eq!(value["balance"], "$5.25");
    assert_eq!(value["items"][0]["used"], 12.75);
    assert_eq!(value["items"][0]["countsForRouting"], false);
    assert!(value["items"][0]["usedPercent"].is_null());
    assert!(value["items"][0].get("resetsAt").is_none());
    assert_eq!(
        client.requests.lock().unwrap()[0].url,
        "https://ai-gateway.vercel.sh/v1/credits"
    );
}

#[tokio::test]
async fn cloudflare_selects_the_tokens_actual_first_account_then_binds_credentials_to_it() {
    let (provider, client) = setup(
        DecisionFamily::Cloudflare,
        vec![
            reply(
                "/client/v4/accounts",
                json!({"success":true,"result":[{"id":"account-real"},{"id":"other-account"}]}),
            ),
            reply(
                "/accounts/account-real/ai/models/search?per_page=100&page=1",
                json!({"success":true,"result":[{"name":"typesafe/jev"}]}),
            ),
            post(
                "/accounts/account-real/ai/run",
                json!({"success":true,"result":answer()}),
            ),
        ],
    );
    let result = provider
        .authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"private-decision-key"}}))
        .await
        .unwrap();
    assert_eq!(result["credentials"]["accountId"], "account-real");
    let base = result["credentials"]["baseUrl"].as_str().unwrap();
    let mut call = request(DecisionFamily::Cloudflare, base, wire::CF_JEV);
    call.credentials = Some(result["credentials"].clone());
    assert_eq!(body(provider.fetch(call).await.unwrap()).await, answer());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let payload: Value = serde_json::from_slice(requests[2].body.as_ref().unwrap()).unwrap();
    assert_eq!(payload["model"], wire::CF_JEV);
    assert_eq!(
        payload["input"]["questions"],
        question(wire::CF_JEV)["questions"]
    );
    assert!(payload["input"].get("model").is_none());
}

#[tokio::test]
async fn cloudflare_explicit_account_works_without_permission_to_list_accounts() {
    let (provider, client) = setup(
        DecisionFamily::Cloudflare,
        vec![reply(
            "/accounts/explicit/ai/models/search?per_page=100&page=1",
            json!({"success":true,"result":[{"name":"@cf/cloudflare/clef-flash"}]}),
        )],
    );
    let result=provider.authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"private-decision-key","accountId":"explicit"}})).await.unwrap();
    assert_eq!(
        result["credentials"]["baseUrl"],
        "https://api.cloudflare.com/client/v4/accounts/explicit/ai/run"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert!(provider.authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"private-decision-key","accountId":"other","endpoint":"https://api.cloudflare.com/client/v4/accounts/explicit/ai/run"}})).await.is_err());
}

#[tokio::test]
async fn a_cloudflare_token_without_account_list_permission_gets_an_actionable_error() {
    let mut response = reply(
        "/client/v4/accounts",
        json!({"success":false,"errors":[{"code":10000}]}),
    );
    response.status = 403;
    let (provider, client) = setup(DecisionFamily::Cloudflare, vec![response]);
    let error = provider
        .authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"private-decision-key"}}))
        .await
        .unwrap_err();
    assert!(error.message.contains("account ID"));
    assert!(error.message.contains("Workers AI"));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cloudflare_directory_paginates_real_results_and_does_not_add_absent_presets() {
    let (provider, client) = setup(
        DecisionFamily::Cloudflare,
        vec![
            reply(
                "/accounts/explicit/ai/models/search?per_page=100&page=1",
                json!({"success":true,"result":[{"name":"@cf/meta/chat"},{"name":"typesafe/jev"}],"result_info":{"total_pages":2}}),
            ),
            reply(
                "/accounts/explicit/ai/models/search?per_page=100&page=2",
                json!({"success":true,"result":[{"name":"@cf/cloudflare/clef"}],"result_info":{"total_pages":2}}),
            ),
        ],
    );
    let value = provider
        .models(context(credentials(
            DecisionFamily::Cloudflare,
            "https://api.cloudflare.com/client/v4/accounts/explicit/ai/run",
        )))
        .await
        .unwrap();
    assert_eq!(value["source"], "remote");
    let models = value["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0]["id"], wire::CF_JEV);
    assert_eq!(models[1]["id"], wire::CF_CLEF);
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn clef_fetch_uses_the_saved_account_its_native_path_and_real_reply_usage() {
    let base = "https://api.cloudflare.com/client/v4/accounts/explicit/ai/run";
    let (provider, client) = setup(
        DecisionFamily::Cloudflare,
        vec![post(
            "/accounts/explicit/ai/run/@cf/cloudflare/clef",
            json!({"success":true,"result":answer()}),
        )],
    );
    assert_eq!(
        body(
            provider
                .fetch(request(DecisionFamily::Cloudflare, base, wire::CF_CLEF))
                .await
                .unwrap()
        )
        .await,
        answer()
    );
    let requests = client.requests.lock().unwrap();
    let payload: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(payload["model"], "clef");
    assert_eq!(payload["state"], question(wire::CF_CLEF)["state"]);
    assert!(payload.get("input").is_none());
}

#[tokio::test]
async fn bailian_checks_the_selected_workspace_or_plan_by_a_real_native_question() {
    for (method, inputs, base) in [
        (
            "cn-beijing",
            json!({"apiKey":"private-decision-key","workspaceId":"ws-one"}),
            "https://ws-one.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        ),
        (
            "ap-southeast-1",
            json!({"apiKey":"private-decision-key","workspaceId":"ws-two"}),
            "https://ws-two.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        ),
        (
            "token-plan",
            json!({"apiKey":"private-decision-key"}),
            "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        ),
    ] {
        let (provider, client) = setup(
            DecisionFamily::Bailian,
            vec![post(
                "/compatible-mode/v1/systemone",
                json!({"answers":{"ok":{"type":"noul","noul":0.99}},"usage":{"input_tokens":12,"output_tokens":0}}),
            )],
        );
        let result = provider
            .authorize(json!({"authMethodID":method,"inputs":inputs}))
            .await
            .unwrap();
        assert_eq!(result["credentials"]["baseUrl"], base);
        let models = provider
            .models(context(result["credentials"].clone()))
            .await
            .unwrap();
        assert_eq!(models["source"], "preset");
        assert_eq!(models["models"][0]["id"], "decision-model-preview");
        assert_eq!(models["models"][0]["contextWindow"], 65536);
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, format!("{base}/systemone"));
        assert_eq!(
            serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap()["model"],
            "decision-model-preview"
        );
    }
}

#[tokio::test]
async fn qwen_uses_the_actual_qwen_platform_key_and_endpoint_not_bailians_chat_catalog() {
    let base = "https://maas.qianwenaiapi.com/compatible-mode/v1";
    let (provider, client) = setup(
        DecisionFamily::Qwen,
        vec![
            post(
                "/compatible-mode/v1/systemone",
                json!({"answers":{"ok":{"type":"noul","noul":0.99}}}),
            ),
            post("/compatible-mode/v1/systemone", answer()),
        ],
    );
    let credentials = provider
        .authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"private-decision-key"}}))
        .await
        .unwrap()["credentials"]
        .clone();
    assert_eq!(credentials["provider"], "qwen-decision");
    assert_eq!(credentials["baseUrl"], base);
    let mut call = request(DecisionFamily::Qwen, base, "decision-model-preview");
    call.credentials = Some(credentials.clone());
    assert_eq!(body(provider.fetch(call).await.unwrap()).await, answer());
    assert_eq!(
        provider.usage(context(credentials)).await.unwrap()["available"],
        false
    );
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn api_keys_are_never_refreshed_or_given_invented_quota_windows() {
    for (family, base) in [
        (
            DecisionFamily::Cloudflare,
            "https://api.cloudflare.com/client/v4/accounts/explicit/ai/run",
        ),
        (
            DecisionFamily::Bailian,
            "https://ws.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        ),
        (
            DecisionFamily::Qwen,
            "https://maas.qianwenaiapi.com/compatible-mode/v1",
        ),
    ] {
        let (provider, client) = setup(family, vec![]);
        let credentials = credentials(family, base);
        assert_eq!(
            provider
                .refresh(json!({"credentials":credentials}))
                .await
                .unwrap()["changed"],
            false
        );
        let value = provider.usage(context(credentials)).await.unwrap();
        assert_eq!(value["available"], false);
        assert!(value["items"].as_array().unwrap().is_empty());
        assert!(client.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn foreign_origins_chat_paths_query_strings_and_wrong_account_credentials_are_rejected_before_network(
) {
    let base = "https://ai-gateway.vercel.sh/typesafe/v1";
    let (provider, client) = setup(DecisionFamily::Vercel, vec![]);
    for url in [
        "https://foreign.example/typesafe/v1/systemone",
        "https://ai-gateway.vercel.sh/typesafe/v1/chat/completions",
        "https://ai-gateway.vercel.sh/typesafe/v1/systemone?key=foreign",
        "https://user:pass@ai-gateway.vercel.sh/typesafe/v1/systemone",
    ] {
        let mut call = request(DecisionFamily::Vercel, base, "typesafe-ai/jev");
        call.url = url.into();
        assert!(provider.fetch(call).await.is_err());
    }
    let mut call = request(DecisionFamily::Vercel, base, "typesafe-ai/jev");
    call.credentials = Some(credentials(DecisionFamily::Cloudflare, base));
    assert!(provider.fetch(call).await.is_err());
    let mut call = request(DecisionFamily::Vercel, base, "typesafe-ai/jev");
    call.credentials = Some(json!({"type":"oauth","access":"other-token"}));
    assert!(provider.fetch(call).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn retry_hints_and_status_survive_upstream_errors_without_credentials_or_private_headers() {
    let mut failure = post(
        "/typesafe/v1/systemone",
        json!({"detail":{"message":"private-decision-key exhausted"}}),
    );
    failure.status = 429;
    failure.headers = BTreeMap::from([
        ("retry-after".into(), "9".into()),
        ("set-cookie".into(), "private-cookie".into()),
    ]);
    let (provider, _) = setup(DecisionFamily::Vercel, vec![failure]);
    let response = provider
        .fetch(request(
            DecisionFamily::Vercel,
            "https://ai-gateway.vercel.sh/typesafe/v1",
            "typesafe-ai/jev",
        ))
        .await
        .unwrap();
    assert_eq!(response.status, 429);
    assert_eq!(response.headers["retry-after"], "9");
    assert!(!response.headers.contains_key("set-cookie"));
    assert!(!body(response)
        .await
        .to_string()
        .contains("private-decision-key"));
}

#[tokio::test]
async fn saved_vercel_evaluation_configuration_is_adapted_without_using_a_chat_route() {
    let base = "https://ai-gateway.vercel.sh/v4/ai";
    let (provider, client) = setup(
        DecisionFamily::Vercel,
        vec![post(
            "/v4/ai/evaluation-model",
            json!({"answers":{"q":{"type":"choice","choice":"code","probabilities":{"code":0.9,"other":0.1}}},"usage":{"inputTokens":27,"outputTokens":0}}),
        )],
    );
    let value = body(
        provider
            .fetch(request(DecisionFamily::Vercel, base, "typesafe-ai/jev"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(value["usage"]["input_tokens"], 27);
    assert_eq!(value["answers"]["q"]["confidence"], 0.8);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].headers["ai-model-id"], "typesafe-ai/jev");
    assert_eq!(
        requests[0].headers["ai-evaluation-model-specification-version"],
        "4"
    );
    assert!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap())
            .unwrap()
            .get("model")
            .is_none()
    );
}

#[tokio::test]
async fn systemone_state_can_be_any_present_json_value() {
    for state in [
        json!("text"),
        json!([{"event":"test"}]),
        Value::Null,
        json!(false),
    ] {
        let base = "https://ai-gateway.vercel.sh/typesafe/v1";
        let (provider, client) = setup(
            DecisionFamily::Vercel,
            vec![post("/typesafe/v1/systemone", answer())],
        );
        let mut value = question("typesafe-ai/jev");
        value["state"] = state.clone();
        let mut call = request(DecisionFamily::Vercel, base, "typesafe-ai/jev");
        call.body = Some(STANDARD.encode(serde_json::to_vec(&value).unwrap()));
        provider.fetch(call).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(
                client.requests.lock().unwrap()[0].body.as_ref().unwrap()
            )
            .unwrap()["state"],
            state
        );
    }
}

#[tokio::test]
async fn host_owned_body_files_are_verified_and_private_sources_never_enter_the_upstream_body() {
    let base = "https://ai-gateway.vercel.sh/typesafe/v1";
    let (provider, client) = setup(
        DecisionFamily::Vercel,
        vec![post("/typesafe/v1/systemone", answer())],
    );
    let value = question("typesafe-ai/jev");
    let path = std::env::temp_dir().join(format!(
        "nora-decision-body-{}-{}.json",
        std::process::id(),
        core::now()
    ));
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let mut call = request(DecisionFamily::Vercel, base, "typesafe-ai/jev");
    call.body = None;
    call.body_file = Some(nora_provider_plugin_sdk::RequestBodyFile::from_path(&path).unwrap());
    provider.fetch(call).await.unwrap();
    std::fs::remove_file(path).unwrap();
    let payload: Value =
        serde_json::from_slice(client.requests.lock().unwrap()[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(payload, value);
    assert!(payload.get("bodyFile").is_none());
    assert!(!payload.to_string().contains("nora-decision-body"));
    for key in ["bodyFile", "body_file"] {
        let mut value = question("typesafe-ai/jev");
        value[key] = json!({"path":"/private/file"});
        let mut call = request(DecisionFamily::Vercel, base, "typesafe-ai/jev");
        call.body = Some(STANDARD.encode(serde_json::to_vec(&value).unwrap()));
        assert!(provider.fetch(call).await.is_err());
    }
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_model_probe_that_returns_no_actual_answers_never_completes_authorization() {
    for value in [json!({"choices":[]}), json!({"answers":{}})] {
        let (provider, _) = setup(
            DecisionFamily::Qwen,
            vec![post("/compatible-mode/v1/systemone", value)],
        );
        assert!(provider
            .authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"private-decision-key"}}))
            .await
            .is_err());
    }
}
