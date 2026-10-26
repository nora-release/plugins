use super::*;
use base64::Engine;
use nora_ai::{HttpClientError, HttpStreamingResponse};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};
#[path = "gift_cache.test.rs"]
mod gift_cache;
#[path = "team_gift.test.rs"]
mod team_gift;
struct Reply {
    path: String,
    method: HttpMethod,
    status: u16,
    value: Value,
}
fn reply(path: impl AsRef<str>, value: Value) -> Reply {
    Reply {
        path: path.as_ref().to_owned(),
        method: HttpMethod::Get,
        status: 200,
        value: json!({"code":0,"data":value}),
    }
}
fn post(path: impl AsRef<str>, value: Value) -> Reply {
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
        let mut replies = self.replies.lock().unwrap();
        let index = replies
            .iter()
            .position(|reply| request.url.ends_with(&reply.path) && reply.method == request.method)
            .unwrap_or_else(|| {
                panic!(
                    "Unexpected Zcode request: {:?} {}",
                    request.method, request.url
                )
            });
        let reply = replies.remove(index).unwrap();
        drop(replies);
        self.requests.lock().unwrap().push(request);
        if reply.status == 0 {
            return Err(HttpClientError::Transport(
                "offline ZCode fixture failure".into(),
            ));
        }
        let bytes = serde_json::to_vec(&reply.value).unwrap();
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            body: Box::pin(futures::stream::once(async move { Ok(bytes.into()) })),
        })
    }
}
struct Store {
    saved: StdMutex<Vec<Value>>,
}
#[async_trait]
impl core::CredentialStore for Store {
    async fn save(&self, value: Value) -> PluginResult<()> {
        self.saved.lock().unwrap().push(value);
        Ok(())
    }
}
fn setup(replies: Vec<Reply>) -> (Zcode, Arc<Client>, Arc<Store>) {
    let client = Arc::new(Client {
        requests: StdMutex::new(Vec::new()),
        replies: StdMutex::new(replies.into()),
    });
    let store = Arc::new(Store {
        saved: StdMutex::new(Vec::new()),
    });
    let mut plugin = Zcode::default();
    plugin.core = Core::with_dependencies("zcode", client.clone(), store.clone());
    plugin
        .core
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: 1,
            plugin_id: "zcode".into(),
            plugin_directory: PathBuf::new(),
            data_directory: PathBuf::new(),
            manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
        })
        .unwrap();
    (plugin, client, store)
}
fn jwt() -> String {
    format!(
        "e30.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({"exp":core::now()/1000+3600,"sub":"user-one"})).unwrap()
        )
    )
}
fn credentials(key: &str, token: &str) -> Value {
    json!({"provider":"zcode","type":"oauth","authMethod":"zai","access":key,"uid":"user-one","state":{"site":"zai","base":Site::Zai.base(),"key":key,"jwt":token,"device":"device-one","plan":"Plan"}})
}
fn request(credentials: Value, model: &str) -> FetchRequest {
    FetchRequest{ caller_user_agent: None, constraints: Default::default(), subscription_accounts_enabled: true,account:"one".into(),credentials:Some(credentials),network_proxy:None,url:format!("{}/v1/messages",Site::Zai.base()),method:"POST".into(),headers:BTreeMap::from([("authorization".into(),"Bearer untrusted".into())]),metadata:BTreeMap::new(),body:Some(base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json!({"model":model,"max_tokens":100,"system":"my system","messages":[{"role":"user","content":"hello"}],"tools":[{"name":"read","input_schema":{"type":"object"}}],"top_p":0.5,"stop_sequences":["END"]})).unwrap())),body_file:None}
}
async fn decode(response: FetchResponse) -> Value {
    let bytes = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap()
        .concat();
    serde_json::from_slice(&bytes).unwrap()
}
fn gift_balance() -> Value {
    json!({"server_time":1000,"plans":[{"status":"active","plan_id":"gift","name":"Gift","ends_at":2000}],"balances":[{"plan_id":"gift","capabilities":["model:glm-5.3-flash"],"total_units":100,"remaining_units":80}]})
}
async fn cache_route(plugin: &Zcode, state: &Value, gift: Option<Gift>) {
    plugin.routes.lock().await.insert(
        route_key(state),
        Route {
            start: false,
            gift,
            at: core::now(),
            ttl: 600_000,
        },
    );
}
#[tokio::test]
async fn paid_fetch_preserves_tools_and_sampling_bytes_and_signs_only_the_selected_site() {
    let native = json!({"type":"message","content":[{"type":"tool_use","id":"call","name":"read","input":{}}]});
    let (plugin, client, _) = setup(vec![Reply {
        path: "/api/anthropic/v1/messages".into(),
        method: HttpMethod::Post,
        status: 200,
        value: native.clone(),
    }]);
    let request = request(credentials("key.secret", ""), "GLM-5.3");
    let original = request.decode_body().unwrap().unwrap();
    assert_eq!(decode(plugin.fetch(request).await.unwrap()).await, native);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body.as_ref().unwrap(), &original);
    assert_eq!(requests[0].headers["x-api-key"], "key.secret");
    assert_eq!(requests[0].headers["authorization"], "Bearer key.secret");
}
#[tokio::test]
async fn start_fetch_uses_native_prompt_headers_and_real_session_without_an_api_key() {
    let (plugin, client, _) = setup(vec![Reply {
        path: "/api/v1/zcode-plan/anthropic/v1/messages".into(),
        method: HttpMethod::Post,
        status: 200,
        value: json!({"type":"message","content":[{"type":"text","text":"actual"}]}),
    }]);
    let token = jwt();
    let result = decode(
        plugin
            .fetch(request(credentials("", &token), "GLM-5.3-Flash"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["content"][0]["text"], "actual");
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].headers["authorization"],
        format!("Bearer {token}")
    );
    assert!(!requests[0].headers.contains_key("x-api-key"));
    assert!(requests[0].headers["user-agent"].contains("ai-sdk/anthropic/3.0.81"));
    let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["system"].as_array().unwrap().len(), 4);
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["top_p"], 0.5);
}
#[tokio::test]
async fn trial_quota_inside_http_200_becomes_429_and_cools_only_that_gift_model() {
    let (plugin, client, _) = setup(vec![Reply {
        path: "/api/v1/zcode-plan/anthropic/v1/messages".into(),
        method: HttpMethod::Post,
        status: 200,
        value: json!({"code":1113,"message":"No resource package"}),
    }]);
    let stored = credentials("key.secret", &jwt());
    cache_route(
        &plugin,
        &stored["state"],
        readings::gift(&gift_balance(), false),
    )
    .await;
    assert_eq!(
        plugin
            .fetch(request(stored.clone(), "GLM-5.3-Flash-Trial"))
            .await
            .unwrap()
            .status,
        429
    );
    assert_eq!(
        plugin
            .fetch(request(stored, "GLM-5.3-Flash-Trial"))
            .await
            .unwrap()
            .status,
        429
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn a_trial_that_the_account_never_had_is_not_sent_to_the_paid_plan() {
    let (plugin, client, _) = setup(vec![]);
    let stored = credentials("key.secret", &jwt());
    cache_route(
        &plugin,
        &stored["state"],
        readings::gift(&gift_balance(), false),
    )
    .await;
    assert_eq!(
        plugin
            .fetch(request(stored, "nonexistent-Trial"))
            .await
            .unwrap()
            .status,
        400
    );
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn start_network_blocks_keep_the_real_failure_status_and_explain_the_vendor_check() {
    let (plugin, _, _) = setup(vec![Reply {
        path: "/api/v1/zcode-plan/anthropic/v1/messages".into(),
        method: HttpMethod::Post,
        status: 405,
        value: json!({"code":3012,"msg":"unusual activity"}),
    }]);
    let response = plugin
        .fetch(request(credentials("", &jwt()), "GLM-5.3-Flash"))
        .await
        .unwrap();
    assert_eq!(response.status, 405);
    let value = decode(response).await;
    assert_eq!(value["error"]["type"], "permission_error");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("IP block"));
}
#[tokio::test]
async fn standalone_browser_signin_resolves_a_real_key_and_plan_after_the_poll() {
    let (plugin, client, _) = setup(vec![
        post(
            "/api/v1/oauth/cli/init",
            json!({"flow_id":"flow-real","authorize_url":"https://auth.z.ai/authorize?redirect_uri=old","poll_interval_sec":1,"expires_at":core::now()/1000+300}),
        ),
        reply(
            "/api/v1/oauth/cli/poll/flow-real",
            json!({"status":"ready","zai":{"access_token":"oauth-token"},"token":jwt(),"user":{"email":"person@example.com"}}),
        ),
        post(
            "/api/auth/z/login",
            json!({"access_token":"business-token"}),
        ),
        reply(
            "/api/biz/customer/getCustomerInfo",
            json!({"organizations":[{"organizationId":"org","organizationName":"默认机构","projects":[{"projectId":"project","projectName":"默认项目","projectType":1}]}]}),
        ),
        reply("/api_keys", json!([])),
        post("/api_keys", json!({"apiKey":"key-id"})),
        reply("/api_keys/copy/key-id", json!({"secretKey":"key-secret"})),
        reply(
            "/api/biz/subscription/list",
            json!([{"status":"VALID","productName":"Pro"}]),
        ),
    ]);
    let start = plugin
        .authorize(json!({"authMethodID":"zai"}))
        .await
        .unwrap();
    let url = url::Url::parse(start["url"].as_str().unwrap()).unwrap();
    assert_eq!(
        url.query_pairs()
            .filter(|(key, _)| key == "redirect_uri")
            .count(),
        1
    );
    let result = plugin
        .callback(json!({"session":start["session"]}))
        .await
        .unwrap();
    assert_eq!(result["credentials"]["state"]["key"], "key-id.key-secret");
    assert_eq!(result["credentials"]["state"]["plan"], "Pro");
    assert_eq!(result["credentials"]["expires"], 0);
    assert_eq!(result["credentials"]["accountId"], "person@example.com");
    assert_eq!(client.requests.lock().unwrap().len(), 8);
}
#[tokio::test]
async fn team_signin_is_separate_and_uses_an_existing_team_key_without_a_personal_key() {
    let (plugin, client, _) = setup(vec![
        post("/api/auth/z/login", json!({"access_token":"business"})),
        reply(
            "/api/biz/customer/getCustomerInfo",
            json!({"organizations":[{"organizationId":"org","projects":[{"projectId":"team","projectType":2}]}]}),
        ),
        reply(
            "/api/biz/team/subscribe/product/querySubscribeDetail",
            json!({"hasSubscription":true,"status":"EFFECTIVE","memberGrantStatus":"VALID","productName":"Team Pro"}),
        ),
        reply(
            "/api_keys",
            json!([{"name":"zcode-team-api-key","keyType":2,"apiKey":"team-key"}]),
        ),
        reply("/api_keys/copy/team-key", json!({})),
    ]);
    let state = auth::sign_in(
        client.as_ref(),
        Site::Zai,
        "token",
        &jwt(),
        &plugin.device,
        true,
    )
    .await
    .unwrap();
    assert_eq!(state["org"], "org");
    assert_eq!(state["project"], "team");
    assert_eq!(state["key"], "team-key");
    let requests = client.requests.lock().unwrap();
    assert!(!requests
        .iter()
        .any(|request| request.url.ends_with("/api_keys") && request.method == HttpMethod::Post));
    let quota = requests
        .iter()
        .find(|request| request.url.ends_with("querySubscribeDetail"))
        .unwrap();
    assert_eq!(quota.headers["bigmodel-project"], "team");
}
#[tokio::test]
async fn readonly_source_key_lookup_never_creates_a_missing_key() {
    let (plugin, client, _) = setup(vec![reply("/api_keys", json!([]))]);
    assert!(auth::project_key(
        client.as_ref(),
        Site::Zai,
        "business",
        "org",
        "team",
        true,
        false,
        &plugin.device
    )
    .await
    .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(client.requests.lock().unwrap()[0].method, HttpMethod::Get);
}
#[tokio::test]
async fn usage_keeps_dual_pools_separate_and_only_advertises_read_only_claim_hints() {
    let preview_path = format!(
        "/api/v1/zcode-plan/billing/preview?app_version=3.14.3&platform={}",
        endpoint::platform()
    );
    let replies = vec![
        reply(
            "/api/monitor/usage/quota/limit",
            json!({"limits":[{"unit":3,"usage":100,"remaining":75}]}),
        ),
        reply("/api/biz/subscription/list", json!([{"status":"VALID"}])),
        reply(
            "/api/biz/customer-package-reset/list?targetType=PERSONAL",
            json!({}),
        ),
        reply(
            "/api/v1/zcode-plan/billing/balance?app_version=3.14.3",
            gift_balance(),
        ),
        reply(
            &preview_path,
            json!({"plans":[{"plan_id":"claim","name":"Trust"}]}),
        ),
    ];
    let (plugin, client, _) = setup(replies);
    let stored = credentials("key.secret", &jwt());
    cache_route(
        &plugin,
        &stored["state"],
        readings::gift(&gift_balance(), false),
    )
    .await;
    let value = plugin
        .usage(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(stored),
            network_proxy: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        value["items"][0]["notModels"],
        json!(["GLM-5.3-Flash-Trial"])
    );
    assert_eq!(value["items"][1]["models"], json!(["GLM-5.3-Flash-Trial"]));
    assert_eq!(
        value["items"].as_array().unwrap().last().unwrap()["countsForRouting"],
        false
    );
    assert!(!client
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.method == HttpMethod::Post));
}
#[tokio::test]
async fn a_business_api_outage_can_still_complete_the_real_start_plan_login() {
    let (plugin, client, _) = setup(vec![
        Reply {
            path: "/api/auth/z/login".into(),
            method: HttpMethod::Post,
            status: 503,
            value: json!({"error":"temporary"}),
        },
        reply(
            "/api/v1/zcode-plan/billing/balance?app_version=3.14.3",
            gift_balance(),
        ),
    ]);
    let state = auth::sign_in(
        client.as_ref(),
        Site::Zai,
        "token",
        &jwt(),
        &plugin.device,
        false,
    )
    .await
    .unwrap();
    assert_eq!(state["key"], "");
    assert_eq!(state["plan"], "Gift");
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn explicitly_selected_source_files_never_follow_a_changed_plan_key() {
    let directory = std::env::temp_dir().join(format!("zcode-provider-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let credentials = directory.join("credentials.json");
    let config = directory.join("config.json");
    std::fs::write(&credentials, b"{}").unwrap();
    let write_key = |key: &str| {
        std::fs::write(
            &config,
            serde_json::to_vec(
                &json!({"provider":{"builtin:zai-coding-plan":{"options":{"apiKey":key}}}}),
            )
            .unwrap(),
        )
        .unwrap()
    };
    write_key("key.secret");
    let source = json!({"sourcePath":credentials,"configPath":config});
    let stored = json!({"provider":"zcode","type":"oauth","authMethod":"source","uid":core::fingerprint(&json!("key.secret")),"site":"zai","source":source});
    let (plugin, client, _) = setup(vec![]);
    assert_eq!(
        plugin.state(&stored, client.as_ref()).await.unwrap()["key"],
        "key.secret"
    );
    write_key("other.secret");
    assert!(plugin.state(&stored, client.as_ref()).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
    assert_eq!(std::fs::read(&credentials).unwrap(), b"{}");
    std::fs::remove_dir_all(directory).unwrap();
}
#[tokio::test]
async fn live_model_rules_are_fetched_without_forwarding_account_credentials_to_the_public_config_file(
) {
    let config_path = format!(
        "/api/v1/client/configs?app_version=3.14.3&platform={}",
        endpoint::platform()
    );
    let config = json!({"revision":8,"config":{"providerConfigRules":{"providerRules":[{"providerId":"account:zai-individual-coding-plan","builtinModelIds":["actual-model"]}]},"modelConfigRules":{"modelRules":[{"modelMatch":"actual-model","config":{"properties":{"contextWindow":32768},"optionSpecs":{"maxOutputTokens":{"max":4096}}}}]}}});
    let (plugin, client, _) = setup(vec![
        reply(
            &config_path,
            json!({"configs":{"builtin_provider_config_json":"https://config.zcode.z.ai/model-config.json"}}),
        ),
        Reply {
            path: "/model-config.json".into(),
            method: HttpMethod::Get,
            status: 200,
            value: config,
        },
    ]);
    let value = plugin
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(credentials("key.secret", "")),
            network_proxy: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(value["source"], "remote");
    assert_eq!(value["models"].as_array().unwrap().len(), 1);
    assert_eq!(value["models"][0]["id"], "actual-model");
    assert_eq!(value["models"][0]["contextWindow"], 32768);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].headers.contains_key("authorization"));
}
