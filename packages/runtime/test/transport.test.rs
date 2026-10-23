use super::*;
use crate::xai_provider::grok_model_selection;
use serde_json::json;
fn definition(id: &str) -> Definition {
    let mut definition = Definition::parse(
        serde_json::from_slice(
            &std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .join(id)
                    .join("nora-provider.json"),
            )
            .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    definition.adapter = crate::test_adapter(id);
    definition
}
fn request(url: &str, credentials: Value) -> FetchRequest {
    FetchRequest {
        account: "private".into(),
        network_proxy: None,
        metadata: BTreeMap::new(),
        body_file: None,
        credentials: Some(credentials),
        url: url.into(),
        method: "POST".into(),
        headers: BTreeMap::from([("authorization".into(), "Bearer nora-plugin".into())]),
        body: None,
    }
}
#[tokio::test]
async fn api_key_is_applied_only_inside_the_plugin() {
    let outgoing = build_request(
        &definition("openai"),
        request(
            "https://api.openai.com/v1/responses",
            json!({"type":"apiKey","access":"private-key"}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(outgoing.headers["Authorization"], "Bearer private-key");
    assert!(!outgoing
        .headers
        .values()
        .any(|value| value.contains("nora-plugin")));
    assert!(build_request(
        &definition("openai"),
        request(
            "https://unrelated.invalid/v1/responses",
            json!({"type":"apiKey","access":"private-key"})
        )
    )
    .await
    .is_err());
}

#[tokio::test]
async fn kimi_requests_get_the_provider_identity_header_inside_the_plugin() {
    let outgoing = build_request(
        &definition("kimi-coding"),
        request(
            "https://api.kimi.com/coding/v1/messages",
            json!({"type":"apiKey","access":"private-kimi-key"}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(outgoing.headers["User-Agent"], "KimiCLI/1.5");
    assert_eq!(outgoing.headers["Authorization"], "Bearer private-kimi-key");
}

#[tokio::test]
async fn anthropic_relays_send_both_key_headers_unless_the_account_chooses_one() {
    for id in [
        "kimi-coding",
        "minimax",
        "minimax-cn",
        "opencode",
        "xiaomi",
        "zai",
    ] {
        let definition = definition(id);
        let mut definition = definition;
        if !definition.config.profiles.is_empty() {
            definition = crate::profiles::configured(definition, None).unwrap();
        }
        let anthropic =
            crate::discovery::configured_bases(&definition, Some(&json!({"type":"apiKey"})))
                .into_iter()
                .find(|base| base.api == "anthropic-messages");
        assert!(
            anthropic.is_some() || definition.api == "anthropic-messages",
            "{id}"
        );
        assert_eq!(
            definition.config.api_key_auth,
            Some(AuthScheme::Bearer),
            "{id}"
        );
        let outgoing = build_request(
            &definition,
            request(
                &format!(
                    "{}/v1/messages",
                    anthropic
                        .as_ref()
                        .map(|base| base.base_url.as_str())
                        .unwrap_or(&definition.base_url)
                        .trim_end_matches('/')
                ),
                json!({"type":"apiKey","access":"private-compatible-key"}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            outgoing.headers["Authorization"], "Bearer private-compatible-key",
            "{id}"
        );
        assert_eq!(
            outgoing.headers["x-api-key"], "private-compatible-key",
            "{id}"
        );
    }
}

#[tokio::test]
async fn anthropic_uses_one_key_header_and_custom_relays_default_to_both() {
    let custom = Definition::parse(json!({
        "id":"custom-anthropic","name":"Custom Anthropic","api":"anthropic-messages",
        "baseURL":"https://custom.invalid","config":{"keyProvider":"custom"}
    }))
    .unwrap();
    for definition in [definition("anthropic"), custom] {
        let outgoing = build_request(
            &definition,
            request(
                &format!("{}/v1/messages", definition.base_url.trim_end_matches('/')),
                json!({"type":"apiKey","access":"private-header-key"}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(outgoing.headers["x-api-key"], "private-header-key");
        let bearer = outgoing
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"));
        assert_eq!(
            bearer.map(|(_, value)| value.as_str()),
            (definition.id != "anthropic").then_some("Bearer private-header-key")
        );
    }
}

#[tokio::test]
async fn copilot_rewrites_v1_and_keeps_its_identity_headers() {
    let request = build_request(
        &definition("github-copilot"),
        request(
            "https://api.individual.githubcopilot.com/v1/responses",
            json!({"type":"oauth","access":"chat-token","credentialProvider":"gitHubCopilot"}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        request.url,
        "https://api.individual.githubcopilot.com/responses"
    );
    assert_eq!(request.headers["copilot-integration-id"], "vscode-chat");
}

#[tokio::test]
async fn anthropic_subscription_cannot_replay_oauth_through_generic_http() {
    let incoming = request(
        "https://api.anthropic.com/v1/messages",
        json!({"type":"oauth","access":"oauth-access","credentialProvider":"anthropic"}),
    );
    let error = build_request(&definition("anthropic"), incoming)
        .await
        .unwrap_err();
    assert_eq!(error.code, "subscription_requires_claude_cli");
    assert!(!error.message.contains("oauth-access"));
}

#[tokio::test]
async fn google_replaces_the_placeholder_query_key_and_bedrock_signs_privately() {
    let google=build_request(&definition("google-gemini"),request("https://generativelanguage.googleapis.com/v1beta/models/gemini:streamGenerateContent?key=nora-plugin&alt=sse",json!({"type":"apiKey","access":"google-secret"}))).await.unwrap();
    assert!(google.url.contains("key=google-secret"));
    assert!(!google.url.contains("nora-plugin"));
    assert!(redact(google.url.clone(), &request_secrets(&google)).contains("key=[redacted]"));
    let aws = build_request(
        &definition("amazon-bedrock"),
        request(
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/fake/converse-stream",
            json!({"type":"apiKey","access":"AKIA_PRIVATE:aws-private-secret:aws-session"}),
        ),
    )
    .await
    .unwrap();
    assert!(aws.headers["authorization"].contains("Credential=AKIA_PRIVATE/"));
    assert!(!aws.headers["authorization"].contains("aws-private-secret"));
    assert_eq!(aws.headers["x-amz-security-token"], "aws-session");
}

#[tokio::test]
async fn bedrock_requests_follow_a_changed_provider_region() {
    let mut definition = definition("amazon-bedrock");
    definition.base_url = "eu-west-1".into();
    let credentials = json!({"type":"apiKey","access":"AKIA_PRIVATE:aws-private-secret","extras":{"baseUrl":"us-east-1"}});
    let outgoing = build_request(
        &definition,
        request(
            "https://bedrock-runtime.eu-west-1.amazonaws.com/model/fake/converse-stream",
            credentials.clone(),
        ),
    )
    .await
    .unwrap();
    assert!(outgoing.headers["authorization"].contains("/eu-west-1/bedrock/aws4_request"));
    assert!(build_request(
        &definition,
        request(
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/fake/converse-stream",
            credentials,
        ),
    )
    .await
    .is_err());
}

#[tokio::test]
async fn grok_source_error_redaction_uses_the_live_token_instead_of_saved_reference() {
    let directory = std::env::temp_dir().join(format!(
        "nora-provider-source-error-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let auth_file = directory.join("auth.json");
    std::fs::write(&auth_file,serde_json::to_vec(&json!({"https://auth.x.ai":{"key":"live-source-private-token","user_id":"person","oidc_issuer":"https://auth.x.ai","expires_at":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339()}})).unwrap()).unwrap();
    let credentials = json!({"type":"grokCLI","credentialProvider":"xAI","access":"","extras":{"noraAccountSource":{"kind":"grokCLI","homeDirectory":directory,"authFilePath":auth_file,"executablePath":null},"noraGrokCLIAccountUserID":"person","userId":"person"}});
    let outgoing = build_request(
        &definition("xai"),
        request(
            "https://cli-chat-proxy.grok.com/v1/responses",
            credentials.clone(),
        ),
    )
    .await
    .unwrap();
    assert!(!credentials
        .to_string()
        .contains("live-source-private-token"));
    assert_eq!(
        outgoing.headers["Authorization"],
        "Bearer live-source-private-token"
    );
    assert_eq!(
        redact(
            "denied Bearer live-source-private-token".into(),
            &request_secrets(&outgoing)
        ),
        "denied [redacted]"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn grok_subscription_metadata_is_not_sent_to_the_upstream() {
    use base64::Engine;
    use nora_ai::{HttpClient, ReqwestHttpClient};
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    let payload = json!({
        "model":"grok-build-0.1",
        "metadata":{"session_id":"nora-session"},
        "input":[{"role":"user","content":[{"type":"input_text","text":"Hello"}]}],
        "tools":[{"type":"function","name":"save","parameters":{"type":"object","properties":{"metadata":{"type":"object"}}}}],
        "stream":true
    });
    let mut incoming = request(
        "https://cli-chat-proxy.grok.com/v1/responses",
        json!({"type":"oauth","credentialProvider":"xAI","access":"test-grok-access"}),
    );
    incoming.body = Some(
        base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&payload).unwrap()),
    );
    let mut outgoing = build_request(&definition("xai"), incoming).await.unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "No upstream request received");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("Cannot accept upstream request: {error}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        let (header_end, content_length) = loop {
            let count = socket.read(&mut chunk).unwrap();
            assert!(count > 0, "Upstream request ended before its body");
            bytes.extend_from_slice(&chunk[..count]);
            let Some(header_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .unwrap()
                .1
                .trim()
                .parse::<usize>()
                .unwrap();
            if bytes.len() >= header_end + 4 + content_length {
                break (header_end, content_length);
            }
        };
        let body: Value =
            serde_json::from_slice(&bytes[header_end + 4..header_end + 4 + content_length])
                .unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
        body
    });
    outgoing.url = format!("http://{address}/v1/responses");
    let response = ReqwestHttpClient::new().stream(outgoing).await.unwrap();
    assert_eq!(response.status, 200);
    let sent = server.join().unwrap();
    assert!(sent.get("metadata").is_none());
    for key in ["model", "input", "tools", "stream"] {
        assert_eq!(sent[key], payload[key], "{key}");
    }
}

#[tokio::test]
async fn grok_api_key_requests_preserve_metadata() {
    use base64::Engine;
    let payload = json!({"model":"grok-4","metadata":{"user":"work"}});
    let mut incoming = request(
        "https://api.x.ai/v1/responses",
        json!({"type":"apiKey","access":"private-key"}),
    );
    incoming.body = Some(
        base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&payload).unwrap()),
    );
    let outgoing = build_request(&definition("xai"), incoming).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(outgoing.body.as_ref().unwrap()).unwrap(),
        payload
    );
}

#[tokio::test]
async fn errors_redact_live_credentials_across_chunks_and_response_headers() {
    let response = HttpStreamingResponse {
        status: 401,
        headers: BTreeMap::from([
            ("content-length".into(), "123".into()),
            ("x-debug".into(), "Bearer source-secret".into()),
        ]),
        body: Box::pin(futures::stream::iter(vec![
            Ok(b"refused Bearer sour".to_vec().into()),
            Ok(b"ce-secret".to_vec().into()),
        ])),
    };
    let mut response = finish_response(
        response,
        vec!["Bearer source-secret".into(), "source-secret".into()],
    )
    .await
    .unwrap();
    assert_eq!(response.status, 401);
    assert!(!response.headers.contains_key("content-length"));
    assert_eq!(response.headers["x-debug"], "[redacted]");
    assert_eq!(
        response.body.next().await.unwrap().unwrap(),
        b"refused [redacted]"
    );
    assert!(response.body.next().await.is_none());
}
#[tokio::test]
async fn declared_protocol_bases_are_allowed_but_custom_addresses_do_not_keep_the_old_origins() {
    let mut definition = definition("kimi-coding");
    let outgoing = build_request(
        &definition,
        request(
            "https://api.kimi.com/coding/v1/chat/completions",
            json!({"type":"apiKey","access":"private-key"}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(outgoing.headers["Authorization"], "Bearer private-key");
    assert!(build_request(
        &definition,
        request(
            "https://api.kimi.com/account/private",
            json!({"type":"apiKey","access":"private-key"})
        )
    )
    .await
    .is_err());
    definition.base_url = "https://custom.example.test/provider".into();
    assert!(build_request(
        &definition,
        request(
            "https://api.kimi.com/coding/v1/chat/completions",
            json!({"type":"apiKey","access":"private-key"})
        )
    )
    .await
    .is_err());
    assert!(build_request(
        &definition,
        request(
            "https://custom.example.test/provider/v1/messages",
            json!({"type":"apiKey","access":"private-key"})
        )
    )
    .await
    .is_ok());
}

#[tokio::test]
async fn grok_context_window_variants_use_the_community_plugin_headers() {
    use base64::Engine;
    let payload =
        json!({"model":"grok-4.7@500k","prompt_cache_key":"conversation-123","input":"hello"});
    let mut incoming = request(
        "https://cli-chat-proxy.grok.com/v1/responses",
        json!({"type":"oauth","credentialProvider":"xAI","access":"private-key"}),
    );
    incoming.body = Some(
        base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&payload).unwrap()),
    );
    let outgoing = build_request(&definition("xai"), incoming).await.unwrap();
    let body: Value = serde_json::from_slice(outgoing.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["model"], "grok-4.7");
    assert_eq!(outgoing.headers["x-grok-model-override"], "grok-4.7");
    assert_eq!(outgoing.headers["x-grok-context-window"], "500000");
    assert_eq!(outgoing.headers["x-grok-conv-id"], "conversation-123");
    assert_eq!(grok_model_selection("grok@131072"), ("grok", Some(131072)));
    for id in [
        "grok@0",
        "grok@-1",
        "grok@oops",
        "grok@18446744073709551615k",
    ] {
        assert_eq!(grok_model_selection(id), (id, None));
    }
}
