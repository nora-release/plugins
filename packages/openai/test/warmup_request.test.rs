use super::*;
#[test]
fn cheapest_model_low_effort_and_fallbacks_match_official() {
    let cases = [
        (
            json!([{"id":"flagship","cost":{"input":10,"output":30}},{"id":"luna","cost":{"input":1,"output":2},"reasoningEffortLevels":["medium","low"]}]),
            json!({}),
            ("luna", Some("low")),
        ),
        (
            json!([{"id":"first"},{"id":"gpt-luna"},{"id":"gpt-mini","efforts":["minimal","high"]}]),
            json!({}),
            ("gpt-mini", Some("minimal")),
        ),
        (
            json!([{"id":"first"},{"id":"gpt-luna"}]),
            json!({}),
            ("gpt-luna", None),
        ),
        (
            json!([{"id":"first"},{"id":"second"}]),
            json!({}),
            ("first", None),
        ),
        (
            json!([{"id":"first"},{"id":"second"}]),
            json!({"models":{"second":{"price":{"input":1,"output":2}}}}),
            ("second", None),
        ),
        (
            json!([{"id":"a","price":{"input":0,"output":0}},{"id":"b","price":{"input":1,"output":1}}]),
            json!({}),
            ("b", None),
        ),
    ];
    for (models, prices, want) in cases {
        assert_eq!(pick(models.as_array().unwrap(), &prices).unwrap(), want);
    }
    assert!(pick(&[], &Value::Null).is_err());
}

#[tokio::test]
async fn normalized_models_obtain_real_cached_prices_without_unknown_fields_or_zero_shadowing() {
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Directory(
        std::env::temp_dir().join(format!("nora-warmup-prices-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(&root.0).unwrap();
    std::fs::write(
        root.0.join("models.json"),
        json!({"openai":{"id":"openai","models":{
            "gpt-6-sol":{"id":"gpt-6-sol","name":"Sol","cost":{"input":10,"output":30}},
            "gpt-6-small":{"id":"gpt-6-small","name":"Luna","cost":{"input":1,"output":2}}
        }}})
        .to_string(),
    )
    .unwrap();
    let plugin = OpenAIPlugin::default();
    plugin
        .initialize(InitializeParams {
            api_version: 2,
            subscription_accounts_enabled: true,
            plugin_id: "openai".into(),
            plugin_directory: root.0.clone(),
            data_directory: root.0.clone(),
            manifest: serde_json::from_str(include_str!("../nora-plugin.json")).unwrap(),
            installed_manifest: None,
            runtime_config: json!({}),
        })
        .await
        .unwrap();
    let models = ["gpt-6-sol", "gpt-6-small"].map(|id| {
        serde_json::to_value(nora_ai::Model::new(id, "chatgpt-codex", "openai")).unwrap()
    });
    let prices = plugin.warmup_prices(&models).await.unwrap();
    assert_eq!(prices["models"]["gpt-6-small"]["price"]["input"], 1.0);
    assert_eq!(pick(&models, &prices).unwrap().0, "gpt-6-small");
    let known_free = json!([{"id":"gpt-mini","cost":{"input":0,"output":0},"info":{"pricingKnown":true}},{"id":"gpt-6-sol"}]);
    let misleading_prices = json!({"models":{"gpt-mini":{"price":{"input":0.1,"output":0.1}},"gpt-6-sol":{"price":{"input":10,"output":30}}}});
    assert_eq!(
        pick(known_free.as_array().unwrap(), &misleading_prices)
            .unwrap()
            .0,
        "gpt-6-sol"
    );
}
#[test]
fn warmup_request_keeps_selected_account_proxy_and_tiny_input() {
    let request=request(&json!({"account":"saved-a","credentials":{"access":"fake"},"networkProxy":{"mode":"proxy","url":"http://127.0.0.1:1234"}}),"gpt-cheap",Some("low"), "Codex fixture instructions").unwrap();
    assert_eq!(request.account, "saved-a");
    assert_eq!(
        request.url,
        "https://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(request.method, "POST");
    assert_eq!(request.credentials.unwrap()["access"], "fake");
    assert!(request.network_proxy.is_some());
    assert_eq!(
        serde_json::from_slice::<Value>(
            &base64::engine::general_purpose::STANDARD
                .decode(request.body.unwrap())
                .unwrap()
        )
        .unwrap(),
        json!({
            "model":"gpt-cheap",
            "input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}],
            "instructions":"Codex fixture instructions","reasoning":{"effort":"low","summary":"auto"},
            "store":false,"stream":true,"tool_choice":"auto","parallel_tool_calls":true,
            "text":{"verbosity":"medium"},"include":["reasoning.encrypted_content"],
            "prompt_cache_key":"c12dc832-a353-6f7c-97db-6d52e8e861f1"
        })
    );
}
#[tokio::test]
async fn reads_success_to_end_and_never_turns_error_or_broken_stream_into_success() {
    assert_eq!(
        consume(FetchResponse::bytes(
            200,
            Default::default(),
            b"event: response.completed\n\n".to_vec()
        ))
        .await
        .unwrap()["outcome"],
        "warmed"
    );
    assert!(consume(FetchResponse::bytes(
        429,
        Default::default(),
        b"private error".to_vec()
    ))
    .await
    .unwrap_err()
    .message
    .contains("429"));
    let response = FetchResponse {
        status: 200,
        headers: Default::default(),
        body: Box::pin(futures::stream::iter([Err(PluginError::transport(
            "read failed",
        ))])),
    };
    assert!(consume(response).await.is_err());
}

#[tokio::test]
async fn cancelling_warmup_drops_the_response_stream() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    struct Pending(Arc<AtomicBool>);
    impl Drop for Pending {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    impl futures::Stream for Pending {
        type Item = PluginResult<Vec<u8>>;
        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Pending
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let response = FetchResponse {
        status: 200,
        headers: Default::default(),
        body: Box::pin(Pending(dropped.clone())),
    };
    let task = tokio::spawn(consume(response));
    tokio::task::yield_now().await;
    task.abort();
    let _ = task.await;
    assert!(dropped.load(Ordering::SeqCst));
}
#[tokio::test]
async fn the_real_request_builder_signs_and_shapes_one_tiny_request_over_loopback_http() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let mut definition =
        Definition::parse(serde_json::from_str(include_str!("../nora-plugin.json")).unwrap())
            .unwrap();
    definition.adapter = std::sync::Arc::new(OpenAI);
    let params = json!({"account":"saved-a","credentials":{"type":"oauth","provider":"openAICodex","credentialProvider":"openAICodex","access":"fixture-only-access","expires":4070908800000_i64,"extras":{"accountId":"workspace-a"}}});
    let built = nora_provider::transport::build_request(
        &definition,
        request(
            &params,
            "gpt-cheap",
            Some("low"),
            "Codex fixture instructions",
        )
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(built.url, "https://chatgpt.com/backend-api/codex/responses");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        while !raw.ends_with(b"\r\n\r\n") {
            raw.push(socket.read_u8().await.unwrap());
        }
        let headers = String::from_utf8(raw).unwrap().to_ascii_lowercase();
        assert!(headers.starts_with("post /backend-api/codex/responses http/1.1"));
        assert!(headers.contains("authorization: bearer fixture-only-access"));
        assert!(headers.contains("chatgpt-account-id: workspace-a"));
        let size = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let mut body = vec![0; size];
        socket.read_exact(&mut body).await.unwrap();
        if headers.contains("content-encoding: zstd") {
            body = zstd::stream::decode_all(body.as_slice()).unwrap();
        }
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["model"], "gpt-cheap");
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["instructions"], "Codex fixture instructions");
        assert_eq!(
            body["input"],
            json!([{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}])
        );
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .await
            .unwrap();
    });
    // Only the socket destination changes; these are the actual finalized headers and bytes.
    let client = nora_provider_plugin_sdk::http_client(Some(&NetworkProxy::Direct)).unwrap();
    let mut send = client
        .post(format!("http://{address}/backend-api/codex/responses"))
        .body(built.body.unwrap());
    for (key, value) in built.headers {
        send = send.header(key, value);
    }
    assert_eq!(send.send().await.unwrap().status().as_u16(), 200);
    server.await.unwrap();
}

#[test]
fn warmup_reasoning_none_absent_and_ultra_follow_codex_body() {
    for (effort, expected, include) in [
        (None, None, true),
        (Some("none"), Some("none"), false),
        (Some("ultra"), Some("max"), true),
    ] {
        let request = request(&json!({"account":"saved"}), "model", effort, "fixture").unwrap();
        let body: Value = serde_json::from_slice(
            &base64::engine::general_purpose::STANDARD
                .decode(request.body.unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["reasoning"]["effort"].as_str(), expected);
        assert_eq!(body.get("include").is_some(), include);
        assert_eq!(
            body["reasoning"]["summary"].as_str(),
            effort.filter(|s| *s != "none").map(|_| "auto")
        );
    }
}
