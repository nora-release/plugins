use super::*;

#[path = "coding_device_protocol.test.rs"]
mod protocol;

const DEVICE: &str = "11111111-2222-4333-8444-555555555555";
const PASTED_DEVICE: &str = "32bcd51b-9202-4c74-b1b1-3a6fada8681d";

fn saved(selected: Site) -> Value {
    let mut saved = credentials("id.pasted", "");
    saved["state"]["site"] = json!(selected.id());
    saved["state"]["base"] = json!(selected.base());
    saved["state"]["device"] = json!(DEVICE);
    saved
}

fn message(path: &str, status: u16) -> Reply {
    Reply {
        path: path.into(),
        method: HttpMethod::Post,
        status,
        value: if status == 200 {
            json!({"type":"message","content":[]})
        } else {
            json!({"type":"error","error":{"type":"invalid_request_error","message":"fixture refusal"}})
        },
    }
}

fn pi() -> Value {
    json!({"model":"GLM-5.3-Flash","max_tokens":16000,"stream":true,"system":[{"type":"text","text":"You are Pi.","cache_control":{"type":"ephemeral"}}],"messages":[{"role":"user","content":[{"type":"text","text":"hello","cache_control":{"type":"ephemeral"}}]}],"tools":[{"name":"read","input_schema":{"type":"object"}}],"top_p":0.5})
}

fn with_body(saved: Value, path: &str, bytes: &[u8]) -> FetchRequest {
    let selected = Site::parse(saved["state"]["site"].as_str().unwrap_or("zai")).unwrap();
    let mut req = request(saved, "GLM-5.3-Flash");
    req.url = format!("{}{path}", selected.base());
    req.body = Some(base64::engine::general_purpose::STANDARD.encode(bytes));
    req.headers.extend(BTreeMap::from([
        ("Content-Length".into(), bytes.len().to_string()),
        ("Anthropic-Version".into(), "2023-06-01".into()),
        ("Anthropic-Beta".into(), "prompt-caching-2024-07-31".into()),
    ]));
    req
}

async fn sent(saved: Value, path: &str, bytes: &[u8]) -> HttpRequest {
    let (plugin, client, store) = setup(vec![message(path, 200)]);
    let req = with_body(saved, path, bytes);
    assert_eq!(plugin.fetch(req).await.unwrap().status, 200);
    assert!(store.saved.lock().unwrap().is_empty());
    let sent = client.requests.lock().unwrap().remove(0);
    sent
}

fn body(request: &HttpRequest) -> Value {
    serde_json::from_slice(request.body.as_ref().unwrap()).unwrap()
}

fn user_id(device: &str) -> String {
    format!(
        r#"{{"device_id":{},"account_uuid":"","session_id":""}}"#,
        serde_json::to_string(device).unwrap()
    )
}

#[tokio::test]
async fn coding_messages_on_both_sites_name_the_saved_device_and_keep_the_agents_request() {
    for selected in [Site::Zai, Site::BigModel] {
        let original = pi();
        let request = sent(
            saved(selected),
            "/v1/messages",
            &serde_json::to_vec(&original).unwrap(),
        )
        .await;
        let mut marked = body(&request);
        assert_eq!(marked["metadata"]["user_id"], user_id(DEVICE));
        marked.as_object_mut().unwrap().remove("metadata");
        assert_eq!(marked, original);
        assert_eq!(request.url, format!("{}/v1/messages", selected.base()));
        assert_eq!(request.headers["authorization"], "Bearer id.pasted");
        assert_eq!(request.headers["x-api-key"], "id.pasted");
        assert_eq!(
            request.headers["anthropic-beta"],
            "prompt-caching-2024-07-31"
        );
        assert!(!request
            .headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case("content-length")));
    }
}

#[tokio::test]
async fn an_existing_user_id_keeps_its_exact_bytes_and_all_other_metadata() {
    let original = br#"{ "model": "GLM-5.3-Flash", "messages": [], "metadata": { "user_id": "  caller's session  ", "trace": [1, 2] } }"#;
    let request = sent(saved(Site::Zai), "/v1/messages", original).await;
    assert_eq!(request.body.unwrap(), original);
    let original =
        serde_json::to_vec(&json!({"messages":[],"metadata":{"user_id":"\u{0085}"}})).unwrap();
    assert_eq!(
        sent(saved(Site::Zai), "/v1/messages", &original)
            .await
            .body
            .unwrap(),
        original
    );
}

#[tokio::test]
async fn empty_or_non_string_user_ids_are_replaced_without_erasing_other_metadata() {
    for id in [
        Value::Null,
        json!(""),
        json!(" \t\n"),
        json!("\u{feff}\u{00a0}\u{2028}\u{2029}\u{3000}"),
        json!(0),
        json!([]),
        json!({}),
    ] {
        let original = json!({"model":"GLM-5.3-Flash","messages":[],"metadata":{"user_id":id,"trace":{"id":42}}});
        let request = sent(
            saved(Site::BigModel),
            "/v1/messages",
            &serde_json::to_vec(&original).unwrap(),
        )
        .await;
        assert_eq!(
            body(&request)["metadata"],
            json!({"user_id":user_id(DEVICE),"trace":{"id":42}})
        );
    }
    for metadata in [
        Value::Null,
        json!(false),
        json!(42),
        json!("text"),
        json!(["old"]),
    ] {
        let original = json!({"model":"GLM-5.3-Flash","messages":[],"metadata":metadata});
        let request = sent(
            saved(Site::Zai),
            "/v1/messages",
            &serde_json::to_vec(&original).unwrap(),
        )
        .await;
        assert_eq!(
            body(&request)["metadata"],
            json!({"user_id":user_id(DEVICE)})
        );
    }
}

#[tokio::test]
async fn request_marking_follows_the_messages_array_not_the_presence_of_a_model() {
    let request = sent(saved(Site::Zai), "/v1/messages", br#"{"messages":[]}"#).await;
    assert_eq!(
        body(&request),
        json!({"messages":[],"metadata":{"user_id":user_id(DEVICE)}})
    );
    for original in [
        br#"{"model":"GLM-5.3-Flash"}"#.as_slice(),
        br#"{"model":"GLM-5.3-Flash","messages":{}}"#,
        br#"{"model":"GLM-5.3-Flash","messages":null}"#,
        br#"[{"messages":[]}]"#,
        b"null",
        br#""messages""#,
        br#"{"\u006dessages":[]}"#,
    ] {
        assert_eq!(
            sent(saved(Site::Zai), "/v1/messages", original)
                .await
                .body
                .unwrap(),
            original
        );
    }
}

#[tokio::test]
async fn non_json_and_binary_bodies_keep_their_content_type_and_bytes() {
    for (bytes, content_type) in [
        (
            b"not json, messages".as_slice(),
            "text/plain; charset=utf-8",
        ),
        (
            b"\xff\xfe\0messages\x80".as_slice(),
            "application/octet-stream",
        ),
        (b"".as_slice(), "application/octet-stream"),
    ] {
        let (plugin, client, _) = setup(vec![message("/v1/messages", 400)]);
        let mut req = with_body(saved(Site::BigModel), "/v1/messages", bytes);
        req.headers
            .insert("Content-Type".into(), content_type.into());
        let response = plugin.fetch(req).await.unwrap();
        assert_eq!(response.status, 400);
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests[0].body.as_ref().unwrap(), bytes);
        assert_eq!(requests[0].headers["content-type"], content_type);
    }
}

#[tokio::test]
async fn count_tokens_on_both_sites_preserves_the_original_body_and_does_not_mark_it() {
    for selected in [Site::Zai, Site::BigModel] {
        let original =
            br#"{ "model":"GLM-5.3-Flash", "messages":[], "metadata":{"trace":"counting"} }"#;
        let request = sent(saved(selected), "/v1/messages/count_tokens", original).await;
        assert_eq!(
            request.url,
            format!("{}/v1/messages/count_tokens", selected.base())
        );
        assert_eq!(request.body.as_ref().unwrap(), original);
        assert!(body(&request)["metadata"].get("user_id").is_none());
    }
}

#[tokio::test]
async fn a_request_without_a_body_is_forwarded_without_inventing_metadata() {
    let (plugin, client, _) = setup(vec![message("/v1/messages", 400)]);
    let mut req = request(saved(Site::Zai), "GLM-5.3-Flash");
    req.body = None;
    assert_eq!(plugin.fetch(req).await.unwrap().status, 400);
    assert!(client.requests.lock().unwrap()[0].body.is_none());
}

#[tokio::test]
async fn devices_and_keys_stay_bound_to_each_account_when_requests_are_interleaved() {
    let (plugin, client, store) = setup(vec![
        message("/v1/messages", 200),
        message("/v1/messages", 200),
        message("/v1/messages", 200),
    ]);
    for (selected, key, device) in [
        (Site::Zai, "first.key", "first-device"),
        (Site::BigModel, "second.key", "second-device"),
        (Site::Zai, "first.key", "first-device"),
    ] {
        let mut stored = saved(selected);
        stored["uid"] = json!(key);
        stored["access"] = json!(key);
        stored["state"]["key"] = json!(key);
        stored["state"]["device"] = json!(device);
        plugin
            .fetch(with_body(
                stored,
                "/v1/messages",
                &serde_json::to_vec(&pi()).unwrap(),
            ))
            .await
            .unwrap();
        let requests = client.requests.lock().unwrap();
        let sent = requests.last().unwrap();
        assert_eq!(sent.url, format!("{}/v1/messages", selected.base()));
        assert_eq!(sent.headers["x-api-key"], key);
        assert_eq!(body(sent)["metadata"]["user_id"], user_id(device));
    }
    assert!(store.saved.lock().unwrap().is_empty());
}

#[tokio::test]
async fn saved_devices_win_and_key_only_accounts_keep_the_same_official_id_across_restarts() {
    for (key, expected) in [
        ("id.pasted", PASTED_DEVICE),
        ("id.other", "427ab39c-5522-4804-9f79-16f98bae1c1d"),
    ] {
        let mut stored = saved(Site::BigModel);
        stored["state"]["key"] = json!(key);
        stored["state"].as_object_mut().unwrap().remove("device");
        for _ in 0..2 {
            let request = sent(
                stored.clone(),
                "/v1/messages",
                &serde_json::to_vec(&pi()).unwrap(),
            )
            .await;
            assert_eq!(body(&request)["metadata"]["user_id"], user_id(expected));
        }
    }
    for device in [DEVICE, "legacy-device", "quoted-\"\\-设备"] {
        let mut stored = saved(Site::Zai);
        stored["state"]["device"] = json!(device);
        for _ in 0..2 {
            let request = sent(
                stored.clone(),
                "/v1/messages",
                &serde_json::to_vec(&pi()).unwrap(),
            )
            .await;
            assert_eq!(body(&request)["metadata"]["user_id"], user_id(device));
        }
    }
}

fn usage_replies() -> Vec<Reply> {
    vec![
        reply(
            "/api/monitor/usage/quota/limit",
            json!({"limits":[],"level":"Pro"}),
        ),
        reply("/api/biz/subscription/list", json!([])),
        reply(
            "/api/biz/customer-package-reset/list?targetType=PERSONAL",
            json!([]),
        ),
    ]
}

#[tokio::test]
async fn pasted_keys_are_assigned_the_official_device_during_authorization() {
    for selected in [Site::Zai, Site::BigModel] {
        for _ in 0..2 {
            let (plugin, client, _) = setup(usage_replies());
            let added = plugin.authorize(json!({"authMethodID":format!("apiKey-{}",selected.id()),"inputs":{"key":" id.pasted "},"subscriptionAccountsEnabled":true})).await.unwrap();
            assert_eq!(added["credentials"]["state"]["device"], PASTED_DEVICE);
            assert_eq!(added["credentials"]["state"]["key"], "id.pasted");
            assert!(client.replies.lock().unwrap().is_empty());
        }
    }
}

struct SourceDirectory(PathBuf);
impl SourceDirectory {
    fn new() -> Self {
        let dir =
            Self(std::env::temp_dir().join(format!("nora-zcode-device-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir(&dir.0).unwrap();
        std::fs::write(dir.0.join("credentials.json"), b"{}").unwrap();
        dir.write_key("id.pasted");
        dir
    }

    fn write_key(&self, key: &str) {
        std::fs::write(
            self.0.join("config.json"),
            serde_json::to_vec(
                &json!({"provider":{"builtin:zai-coding-plan":{"options":{"apiKey":key}}}}),
            )
            .unwrap(),
        )
        .unwrap();
    }

    fn inputs(&self) -> Value {
        json!({"sourcePath":self.0.join("credentials.json"),"configPath":self.0.join("config.json")})
    }
}

impl Drop for SourceDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn source_authorization_keeps_a_device_across_restarts_and_never_changes_the_bound_key() {
    let dir = SourceDirectory::new();
    let (plugin, _, _) = setup(usage_replies());
    let added = plugin.authorize(json!({"authMethodID":"source","inputs":dir.inputs(),"subscriptionAccountsEnabled":true})).await.unwrap();
    let stored = added["credentials"].clone();
    assert_eq!(stored["device"], plugin.device);
    for _ in 0..2 {
        let (restarted, client, store) = setup(vec![message("/v1/messages", 200)]);
        let state = restarted.selected_state(&stored).await.unwrap();
        assert_eq!(state["device"], plugin.device);
        assert_eq!(
            restarted
                .fetch(with_body(
                    stored.clone(),
                    "/v1/messages",
                    &serde_json::to_vec(&pi()).unwrap()
                ))
                .await
                .unwrap()
                .status,
            200
        );
        assert_eq!(
            body(&client.requests.lock().unwrap()[0])["metadata"]["user_id"],
            user_id(&plugin.device)
        );
        assert!(store.saved.lock().unwrap().is_empty());
    }
    dir.write_key("other.key");
    assert!(plugin.selected_state(&stored).await.is_err());
    assert_eq!(
        std::fs::read(dir.0.join("credentials.json")).unwrap(),
        b"{}"
    );
}

#[tokio::test]
async fn legacy_key_sources_without_a_saved_device_use_the_official_stable_key_id() {
    let dir = SourceDirectory::new();
    let stored = json!({"provider":"zcode","type":"oauth","authMethod":"source","source":dir.inputs(),"uid":core::fingerprint(&json!("id.pasted")),"site":"zai"});
    for _ in 0..2 {
        let (plugin, _, _) = setup(vec![]);
        assert_eq!(
            plugin.selected_state(&stored).await.unwrap()["device"],
            PASTED_DEVICE
        );
    }
}

#[tokio::test]
async fn arbitrary_endpoints_methods_and_invalid_body_encoding_never_reach_the_network() {
    let (plugin, client, _) = setup(vec![]);
    for url in [
        "https://attacker.invalid/api/anthropic/v1/messages",
        "https://api.z.ai.attacker.invalid/api/anthropic/v1/messages",
        "https://attacker.invalid@api.z.ai/api/anthropic/v1/messages",
        "http://api.z.ai/api/anthropic/v1/messages",
        "https://api.z.ai/api/anthropic/v1/messages?redirect=evil",
        "https://api.z.ai/api/anthropic/v1/messages#fragment",
        "https://api.z.ai/api/anthropic/v1/messages/count_tokens/extra",
        "https://api.z.ai/api/anthropic/v1/messages/../count_tokens",
        "https://api.z.ai/api/anthropic/v1/messages/%2e%2e/count_tokens",
        "https://api.z.ai/api/anthropic/v1/models",
    ] {
        let mut req = request(saved(Site::Zai), "GLM-5.3-Flash");
        req.url = url.into();
        assert!(plugin.fetch(req).await.is_err(), "{url}");
    }
    for method in ["GET", "PUT", "DELETE"] {
        let mut req = request(saved(Site::Zai), "GLM-5.3-Flash");
        req.method = method.into();
        assert!(plugin.fetch(req).await.is_err());
    }
    let mut req = request(saved(Site::Zai), "GLM-5.3-Flash");
    req.body = Some("not base64 %".into());
    assert!(plugin.fetch(req).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn marking_does_not_turn_an_upstream_refusal_or_transport_failure_into_success() {
    for status in [400, 401, 403, 429, 500] {
        let reply = message("/v1/messages", status);
        let expected = reply.value.clone();
        let (plugin, client, store) = setup(vec![reply]);
        let response = plugin
            .fetch(with_body(
                saved(Site::Zai),
                "/v1/messages",
                &serde_json::to_vec(&pi()).unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status, status);
        assert_eq!(decode(response).await, expected);
        assert_eq!(
            body(&client.requests.lock().unwrap()[0])["metadata"]["user_id"],
            user_id(DEVICE)
        );
        assert!(store.saved.lock().unwrap().is_empty());
    }
    let (plugin, client, store) = setup(vec![message("/v1/messages", 0)]);
    assert!(plugin
        .fetch(with_body(
            saved(Site::Zai),
            "/v1/messages",
            &serde_json::to_vec(&pi()).unwrap()
        ))
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert!(store.saved.lock().unwrap().is_empty());
}
