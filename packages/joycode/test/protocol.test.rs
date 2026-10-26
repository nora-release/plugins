use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use futures::stream;
use nora_ai::{HttpClientError, HttpStreamingResponse};
use std::{collections::VecDeque, sync::Mutex as SyncMutex};

#[derive(Default)]
struct Client {
    requests: SyncMutex<Vec<HttpRequest>>,
    replies: SyncMutex<BTreeMap<String, VecDeque<(u16, Vec<Vec<u8>>)>>>,
}
impl Client {
    fn reply(&self, function: &str, status: u16, value: Value) {
        self.parts(function, status, vec![value.to_string().into_bytes()]);
    }
    fn parts(&self, function: &str, status: u16, parts: Vec<Vec<u8>>) {
        self.replies
            .lock()
            .unwrap()
            .entry(function.into())
            .or_default()
            .push_back((status, parts));
    }
    fn info(&self, user: &str) {
        self.reply("joycode_userInfo",200,json!({"code":0,"data":{"userId":user,"userName":format!("Name {user}"),"tenant":"tenant","orgFullName":"Organization","pk":"private-marker"}}));
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let function = url::Url::parse(&request.url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "functionId")
            .unwrap()
            .1
            .to_string();
        self.requests.lock().unwrap().push(request);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .get_mut(&function)
            .and_then(VecDeque::pop_front)
            .ok_or_else(|| {
                HttpClientError::Transport("private-marker unexpected request".into())
            })?;
        Ok(HttpStreamingResponse {
            status: reply.0,
            headers: BTreeMap::from([
                ("content-type".into(), "text/event-stream".into()),
                ("content-length".into(), "99".into()),
            ]),
            body: Box::pin(stream::iter(
                reply.1.into_iter().map(|part| Ok(part.into())),
            )),
        })
    }
}
fn credentials(key: &str, tenant: &str) -> Value {
    json!({"type":"apiKey","key":key,"metadata":{"tenant":tenant,"loginType":LOGIN_TYPE}})
}
async fn provider(client: Arc<Client>) -> JoyCode {
    let provider = JoyCode {
        client: Some(client),
        ..Default::default()
    };
    provider.initialize(serde_json::from_value(json!({"apiVersion":1,"pluginID":"joycode","pluginDirectory":"/tmp/joycode-fixture","dataDirectory":"/tmp/joycode-fixture-data","manifest":serde_json::from_str::<Value>(include_str!("../nora-provider.json")).unwrap()})).unwrap()).await.unwrap();
    provider
}
fn context(credentials: Value) -> CallContext {
    serde_json::from_value(json!({"account":"one","credentials":credentials})).unwrap()
}
fn fetch(key: &str, tenant: &str) -> FetchRequest {
    serde_json::from_value(json!({"account":"one","credentials":credentials(key,tenant),"url":format!("{BASE}/v1/chat/completions"),"method":"POST","headers":{"authorization":"caller-secret","content-type":"application/json","ptkey":"untrusted"},"body":STANDARD.encode(json!({"model":"JoyAI-Code-1.6","messages":[{"role":"user","content":"question"}],"tools":[{"type":"function","function":{"name":"read","parameters":{"type":"object"}}}],"stream":true,"userId":"untrusted"}).to_string())})).unwrap()
}
async fn bytes(mut response: FetchResponse) -> (u16, BTreeMap<String, String>, Vec<u8>) {
    let mut bytes = Vec::new();
    while let Some(part) = response.body.next().await {
        bytes.extend(part.unwrap());
    }
    (response.status, response.headers, bytes)
}

#[test]
fn protocol_signatures_and_encoded_tenant_headers_match_the_ide() {
    let url = url::Url::parse(&signed_url("chat_completions", 1700000000000, SIGN_KEY)).unwrap();
    let fields = url.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields["sign"],
        "e3cb11e84cd47a763ffc55f08168cb8c1fc5e32cc8e17378f74585ac9cf88ebb"
    );
    let headers = headers_for(&credentials("private-ticket", "测试 租户!"), None).unwrap();
    assert_eq!(headers["ptkey"], "private-ticket");
    assert_eq!(
        headers["tenant"],
        "%E6%B5%8B%E8%AF%95%20%E7%A7%9F%E6%88%B7!"
    );
    assert_eq!(headers["logintype"], LOGIN_TYPE);
    assert!(headers_for(&credentials("bad\nkey", "tenant"), None).is_err());
}
#[test]
fn models_use_current_multipliers_and_tenant_rules_without_inventing_free_pricing() {
    let list = json!([{"chatApiModel":"a","label":"A","features":["function_call","vision"],"maxTotalTokens":123,"respMaxTokens":45},{"chatApiModel":"b","features":[],"billingRate":"0.3x"},{"chatApiModel":"hidden","hidden":true},{"chatApiModel":"image","modelFunctionType":"image"}]);
    let rates = json!({"data":{"billingRate":{"excludeTenants":["JD"],"billingRates":[{"chatApiModel":"a","billingRate":"0x"},{"chatApiModel":"b","billingRate":"0.6x"},{"chatApiModel":"not-listed","billingRate":"0.2x"}]}}});
    let models = models::parse(&list, Some(&rates), "personal", &[]);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].info.free, Some(true));
    assert_eq!(
        models[0]
            .info
            .credit_multiplier
            .as_ref()
            .and_then(serde_json::Number::as_f64),
        Some(0.0)
    );
    assert!(models[0].input.contains(&nora_ai::InputModality::Image));
    assert_eq!(models[0].context_window, 123);
    assert_eq!(models[0].max_tokens, 45);
    assert_eq!(
        models[1]
            .info
            .credit_multiplier
            .as_ref()
            .and_then(serde_json::Number::as_f64),
        Some(0.3)
    );
    for tenant in ["JD", ""] {
        let models = models::parse(&list, Some(&rates), tenant, &[]);
        assert_eq!(models[0].info.free, None);
        assert_eq!(models[0].info.credit_multiplier, None);
    }
    assert_eq!(
        models::parse(&list, None, "personal", &[])[0]
            .info
            .credit_multiplier,
        None
    );
    for value in [
        json!("NaN"),
        json!("-1x"),
        json!("1e2"),
        json!("x"),
        json!("1..2x"),
    ] {
        assert_eq!(models::multiplier(&value), None);
    }
}
#[tokio::test]
async fn live_model_failure_keeps_presets_but_expired_credentials_do_not_become_a_fallback() {
    let client = Arc::new(Client::default());
    let provider = provider(client.clone()).await;
    assert_eq!(
        provider
            .models(context(credentials("key", "tenant")))
            .await
            .unwrap()["source"],
        "preset"
    );
    client.reply(
        "joycode_modelList",
        200,
        json!({"code":401,"msg":"private-ticket"}),
    );
    client.reply("plugin_config", 200, json!({"code":0,"data":{}}));
    assert_eq!(
        provider
            .models(context(credentials("private-ticket", "tenant")))
            .await
            .err()
            .unwrap()
            .code,
        "authentication_required"
    );
}
#[test]
fn quota_preserves_each_pool_zero_usage_expiry_and_never_blocks_all_models() {
    let item = json!({"title":"套餐积分","badge":"团队版","used":0,"total":10000,"remain":10000,"usedPercent":0,"expireText":"有效期至 2026年11月11日"});
    let one = usage::parse(&json!({"usageItems":[item.clone()]})).unwrap();
    assert_eq!(one["balance"], "10,000 积分");
    assert_eq!(one["items"][0]["usedPercent"], 0);
    assert!(one["items"][0]["display"]
        .as_str()
        .unwrap()
        .contains("2026年11月11日"));
    assert!(one["items"][0].get("resetsAt").is_none());
    let mut exhausted = item.clone();
    exhausted["usedPercent"] = json!(100);
    let many = usage::parse(&json!({"usageItems":[exhausted,item]})).unwrap();
    assert!(many.get("balance").is_none());
    assert!(many["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["countsForRouting"] == false));
    for bad in [
        json!({}),
        json!({"usageItems":[]}),
        json!({"usageItems":[{"title":"quota"}]}),
    ] {
        assert!(usage::parse(&bad).is_err());
    }
}
#[tokio::test]
async fn quota_business_auth_http_and_transport_errors_are_safe_and_keep_permission_denials_signed_in(
) {
    for (status, value, expired) in [
        (
            200,
            json!({"code":9,"msg":"套餐不可用","data":{"private":"private-marker"}}),
            false,
        ),
        (200, json!({"code":401,"msg":"expired"}), true),
        (401, json!({"private":"private-marker"}), true),
        (403, json!({"private":"private-marker"}), false),
        (
            200,
            json!({"code":9,"msg":{"private":"private-marker"}}),
            false,
        ),
    ] {
        let client = Arc::new(Client::default());
        client.reply("get_newpoint_ide", status, value);
        let provider = provider(client).await;
        let error = provider
            .usage(context(credentials("private-ticket", "测试租户")))
            .await
            .err()
            .unwrap();
        assert_eq!(error.code == "authentication_required", expired);
        assert!(
            !error.message.contains("private-marker") && !error.message.contains("private-ticket")
        );
    }
}
#[tokio::test]
async fn chat_preserves_raw_sse_tools_and_authenticated_identity_with_account_isolation() {
    let client = Arc::new(Client::default());
    client.info("first-user");
    client.info("second-user");
    let sse=b": heartbeat\n\ndata: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]}}]}\n\ndata: [DONE]\n\n".to_vec();
    for _ in 0..3 {
        client.parts(
            "chat_completions",
            200,
            vec![sse[..4].to_vec(), sse[4..].to_vec()],
        );
    }
    let provider = provider(client.clone()).await;
    for key in ["one-key", "one-key", "two-key"] {
        let (status, headers, body) =
            bytes(provider.fetch(fetch(key, "tenant")).await.unwrap()).await;
        assert_eq!(status, 200);
        assert_eq!(body, sse);
        assert!(!headers.contains_key("content-length"));
    }
    let requests = client.requests.lock().unwrap();
    let chats = requests
        .iter()
        .filter(|request| request.url.contains("functionId=chat_completions"))
        .collect::<Vec<_>>();
    assert_eq!(chats.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.contains("functionId=joycode_userInfo"))
            .count(),
        2
    );
    for (index, request) in chats.iter().enumerate() {
        let body: Value = serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
        assert_eq!(
            body["userId"],
            if index < 2 {
                "first-user"
            } else {
                "second-user"
            }
        );
        assert_eq!(body["language"], "UNKNOWN");
        assert_eq!(body["tools"][0]["function"]["name"], "read");
        assert_eq!(
            request.headers["ptkey"],
            if index < 2 { "one-key" } else { "two-key" }
        );
        assert!(!request.headers.contains_key("authorization"));
    }
}
#[tokio::test]
async fn mislabeled_json_chat_errors_expire_only_401_and_never_expose_private_payloads() {
    for (value, status, code) in [
        (
            json!({"error":{"code":"AI_GRAY_ACCESS_DENIED","message":"denied"},"data":{"private":"private-marker"}}),
            403,
            json!("AI_GRAY_ACCESS_DENIED"),
        ),
        (
            json!({"code":9,"msg":"积分已用完","data":{"private":"private-marker"}}),
            502,
            json!(9),
        ),
        (json!({"code":401,"msg":"private-ticket"}), 401, json!(401)),
        (
            json!({"data":{"private":"private-marker"}}),
            502,
            json!("invalid_response"),
        ),
    ] {
        let client = Arc::new(Client::default());
        client.info("user");
        let raw = value.to_string();
        client.parts(
            "chat_completions",
            200,
            vec![raw[..2].as_bytes().to_vec(), raw[2..].as_bytes().to_vec()],
        );
        let provider = provider(client).await;
        let (actual, headers, body) = bytes(
            provider
                .fetch(fetch("private-ticket", "tenant"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(actual, status);
        assert_eq!(
            headers["x-magpie-sign-in"],
            if status == 401 { "expired" } else { "kept" }
        );
        let body: String = String::from_utf8(body).unwrap();
        assert!(!body.contains("private-ticket") && !body.contains("private-marker"));
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
            code
        );
    }
}
#[tokio::test]
async fn empty_success_is_rejected_and_a_plain_json_completion_is_preserved() {
    let client = Arc::new(Client::default());
    client.info("user");
    client.parts("chat_completions", 200, vec![]);
    client.reply(
        "chat_completions",
        200,
        json!({"choices":[{"message":{"content":"ok"}}]}),
    );
    let provider = provider(client).await;
    assert_eq!(
        bytes(provider.fetch(fetch("key", "tenant")).await.unwrap())
            .await
            .0,
        502
    );
    let (status, headers, body) =
        bytes(provider.fetch(fetch("key", "tenant")).await.unwrap()).await;
    assert_eq!(status, 200);
    assert_eq!(headers["content-type"], "application/json");
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["choices"][0]["message"]["content"],
        "ok"
    );
}
#[test]
fn flatten_is_opt_in_and_discards_tools_only_when_requested() {
    let flat = models::flatten(
        &json!({"model":"m","messages":[{"role":"system","content":"system"},{"role":"user","content":[{"type":"text","text":"hi"},{"type":"image_url"}]}],"tools":[{}],"stream":false}),
    );
    assert!(flat.get("tools").is_none());
    assert_eq!(flat["messages"][0]["content"], "system");
    assert_eq!(flat["messages"][1]["content"], "hi[image omitted]");
    assert_eq!(flat["max_tokens"], 64000);
}

async fn callback_get(url: &url::Url) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", url.port().unwrap()))
        .await
        .unwrap();
    let path = format!("{}?{}", url.path(), url.query().unwrap_or(""));
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8(bytes).unwrap()
}
fn callback_url(flow: &Value) -> url::Url {
    let login = url::Url::parse(text(flow, "url")).unwrap();
    let fields = login.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
    let mut callback =
        url::Url::parse(&format!("http://127.0.0.1:{}/", fields["authPort"])).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("authKey", &fields["authKey"])
        .append_pair("authPort", &fields["authPort"])
        .append_pair("pt_key", "test-ticket")
        .append_pair("tenant", "tenant")
        .append_pair("loginType", LOGIN_TYPE);
    callback
}
#[tokio::test]
async fn browser_sign_in_uses_real_loopback_csrf_validation_and_verified_account_identity() {
    let client = Arc::new(Client::default());
    client.info("verified-user");
    let provider = provider(client.clone()).await;
    let flow = provider.authorize(json!({"method":"oauth"})).await.unwrap();
    let url = callback_url(&flow);
    let mut bad = url.clone();
    let mut values = bad.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
    values.insert("authKey".into(), "wrong".into());
    bad.set_query(None);
    bad.query_pairs_mut().extend_pairs(&values);
    let html = callback_get(&bad).await;
    assert!(html.starts_with("HTTP/1.1 404"));
    assert!(client.requests.lock().unwrap().is_empty());
    let html = callback_get(&url).await;
    assert!(html.contains("登录成功"));
    assert!(!html.contains("private-marker") && !html.contains("test-ticket"));
    let result = provider
        .callback(json!({"session":flow["session"]}))
        .await
        .unwrap();
    assert_eq!(
        result["credentials"]["accountIdentity"],
        json!({"issuer":BASE,"subject":"verified-user","scope":"tenant"})
    );
    assert_eq!(result["credentials"]["name"], "Name verified-user");
    assert_eq!(result["credentials"]["metadata"]["loginType"], LOGIN_TYPE);
    assert!(provider
        .callback(json!({"session":flow["session"]}))
        .await
        .is_err());
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", url.port().unwrap()))
            .await
            .is_err()
    );
}
#[tokio::test]
async fn browser_failure_settles_without_leaking_account_fields_or_tickets() {
    for (status, value) in [
        (
            200,
            json!({"code":7,"data":{"userId":"test","pk":"private-marker"}}),
        ),
        (200, json!({"code":0,"data":{"pk":"private-marker"}})),
        (200, json!({"code":"0","data":{"userId":"test"}})),
        (401, json!({"code":0,"data":{"userId":"test"}})),
    ] {
        let client = Arc::new(Client::default());
        client.reply("joycode_userInfo", status, value);
        let provider = provider(client).await;
        let flow = provider.authorize(json!({"method":"oauth"})).await.unwrap();
        let html = callback_get(&callback_url(&flow)).await;
        assert!(html.contains("登录失败"));
        assert!(!html.contains("private-marker") && !html.contains("test-ticket"));
        let error = provider
            .callback(json!({"session":flow["session"]}))
            .await
            .err()
            .unwrap();
        assert!(
            !error.message.contains("private-marker") && !error.message.contains("test-ticket")
        );
    }
}
#[tokio::test]
async fn manual_ticket_sign_in_checks_identity_and_partitions_same_user_by_verified_tenant() {
    let client = Arc::new(Client::default());
    client.reply(
        "joycode_userInfo",
        200,
        json!({"code":0,"data":{"userId":"same","tenant":"tenant-a"}}),
    );
    client.reply(
        "joycode_userInfo",
        200,
        json!({"code":0,"data":{"userId":"same","tenant":"tenant-b"}}),
    );
    let provider = provider(client).await;
    let a = provider
        .authorize(json!({"method":"apiKey","inputs":{"apiKey":"key-a"}}))
        .await
        .unwrap();
    let b = provider
        .authorize(json!({"method":"apiKey","inputs":{"apiKey":"key-b"}}))
        .await
        .unwrap();
    assert_ne!(
        a["credentials"]["accountIdentity"],
        b["credentials"]["accountIdentity"]
    );
    assert_eq!(a["credentials"]["accountIdentity"]["subject"], "same");
    assert!(provider
        .authorize(json!({"method":"apiKey","inputs":{"apiKey":"bad\nkey"}}))
        .await
        .is_err());
}
#[tokio::test]
async fn dropping_the_replayed_sse_cancels_its_actual_upstream_stream() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Held {
        dropped: Arc<AtomicBool>,
        sent: bool,
    }
    impl Drop for Held {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }
    impl futures::Stream for Held {
        type Item = Result<bytes::Bytes, HttpClientError>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            if self.sent {
                return std::task::Poll::Pending;
            }
            self.sent = true;
            std::task::Poll::Ready(Some(Ok(b"data: [DONE]\n\n".to_vec().into())))
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let response = HttpStreamingResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: Box::pin(Held {
            dropped: dropped.clone(),
            sent: false,
        }),
    };
    let replay = response::normalize(response, &[]).await.unwrap();
    assert!(!dropped.load(Ordering::SeqCst));
    drop(replay);
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn an_expired_ticket_after_cached_identity_does_not_trigger_internal_refresh_or_erase_permission_errors(
) {
    let client = Arc::new(Client::default());
    client.info("owner");
    client.parts("chat_completions", 200, vec![b"data: [DONE]\n\n".to_vec()]);
    client.reply("chat_completions", 200, json!({"code":401,"msg":"expired"}));
    client.reply(
        "chat_completions",
        403,
        json!({"error":{"message":"denied"}}),
    );
    let provider = provider(client.clone()).await;
    assert_eq!(
        bytes(provider.fetch(fetch("key", "tenant")).await.unwrap())
            .await
            .0,
        200
    );
    let expired = bytes(provider.fetch(fetch("key", "tenant")).await.unwrap()).await;
    assert_eq!(expired.0, 401);
    assert_eq!(expired.1["x-magpie-sign-in"], "expired");
    let denied = bytes(provider.fetch(fetch("key", "tenant")).await.unwrap()).await;
    assert_eq!(denied.0, 403);
    assert_eq!(denied.1["x-magpie-sign-in"], "kept");
    assert_eq!(
        client
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.contains("functionId=joycode_userInfo"))
            .count(),
        1
    );
    assert_eq!(
        provider
            .refresh(json!({"credentials":credentials("key","tenant")}))
            .await
            .unwrap(),
        json!({"changed":false})
    );
}
