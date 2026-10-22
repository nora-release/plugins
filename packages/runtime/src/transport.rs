use crate::{auth, definition::Definition, discovery};
use futures::StreamExt;
use nora_ai::{
    AuthScheme, AwsSigV4, AwsSigV4Credentials, HttpMethod, HttpRequest, HttpStreamingResponse,
};
use nora_provider_plugin_sdk::{FetchRequest, FetchResponse, PluginError, PluginResult};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn request_secrets(request: &HttpRequest) -> Vec<String> {
    let mut secrets = Vec::new();
    for (name, value) in &request.headers {
        let name = name.to_ascii_lowercase();
        if ["auth", "key", "token", "secret"]
            .iter()
            .any(|part| name.contains(part))
        {
            secrets.push(value.clone());
            if value
                .get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer "))
            {
                secrets.push(value[7..].into());
            }
        }
    }
    if let Ok(url) = url::Url::parse(&request.url) {
        for (name, value) in url.query_pairs() {
            if ["key", "token", "secret"]
                .iter()
                .any(|part| name.to_ascii_lowercase().contains(part))
            {
                secrets.push(value.into_owned());
            }
        }
    }
    let escaped = secrets
        .iter()
        .map(|secret| url::form_urlencoded::byte_serialize(secret.as_bytes()).collect::<String>())
        .collect::<Vec<_>>();
    secrets.extend(escaped);
    secrets.retain(|secret| !secret.is_empty());
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    secrets.dedup();
    secrets
}

pub fn redact(mut value: String, secrets: &[String]) -> String {
    for secret in secrets {
        value = value.replace(secret, "[redacted]");
    }
    value
}

pub async fn finish_response(
    response: HttpStreamingResponse,
    secrets: Vec<String>,
) -> PluginResult<FetchResponse> {
    let mut headers = response
        .headers
        .into_iter()
        .map(|(name, value)| (name, redact(value, &secrets)))
        .collect::<BTreeMap<_, _>>();
    if response.status >= 400 {
        let body = nora_ai::collect_http_body_text_limited(response.body, 64 * 1024)
            .await
            .map_err(|_| PluginError::transport("The provider error response stopped."))?;
        headers.retain(|name, _| {
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "content-length" | "content-encoding"
            )
        });
        return Ok(FetchResponse::bytes(
            response.status,
            headers,
            redact(body, &secrets).into_bytes(),
        ));
    }
    Ok(FetchResponse {
        status: response.status,
        headers,
        body: Box::pin(response.body.map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|_| PluginError::transport("The provider stream stopped."))
        })),
    })
}

pub async fn build_request(
    definition: &Definition,
    request: FetchRequest,
) -> PluginResult<HttpRequest> {
    let value = request
        .credentials
        .as_ref()
        .ok_or_else(|| PluginError::authentication("Connect a provider account."))?;
    let (credentials, mut resolved) = auth::resolve(definition, value).await?;
    let mut target = url::Url::parse(&request.url)
        .map_err(|_| PluginError::invalid("The provider request address is invalid."))?;
    let endpoint = discovery::configured_bases(definition, Some(value))
        .into_iter()
        .filter_map(|base| url::Url::parse(&base.base_url).ok())
        .find(|endpoint| {
            let base_path = endpoint.path().trim_end_matches('/');
            endpoint.origin() == target.origin()
                && (target.path() == base_path
                    || target.path().starts_with(&format!("{base_path}/")))
        });
    if !target.username().is_empty() || target.password().is_some() || endpoint.is_none() {
        return Err(PluginError::invalid(
            "The request address is outside this provider's configured endpoint.",
        ));
    }
    let endpoint = endpoint.expect("The configured endpoint was checked above");
    let method = match request.method.to_ascii_uppercase().as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        _ => return Err(PluginError::invalid("The provider HTTP method is invalid.")),
    };
    let mut body = request.decode_body()?;
    let mut headers = request
        .headers
        .into_iter()
        .filter(|(name, _)| {
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization"
                    | "x-api-key"
                    | "api-key"
                    | "host"
                    | "content-length"
                    | "connection"
                    | "transfer-encoding"
                    | "x-amz-date"
                    | "x-amz-security-token"
                    | "x-amz-content-sha256"
            )
        })
        .collect::<BTreeMap<_, _>>();
    headers.extend(definition.config.headers.clone());
    definition
        .adapter
        .prepare_fetch(value, &mut target, &mut headers, &mut body, &mut resolved)
        .await?;
    for (key, value) in &definition.config.query_params {
        target.query_pairs_mut().append_pair(key, value);
    }
    if definition.config.endpoint_kind.as_deref() == Some("region") {
        let aws = parse_aws(&credentials.access_token).ok_or_else(|| {
            PluginError::authentication(
                "Enter AWS credentials as accessKeyId:secretAccessKey[:sessionToken] or JSON.",
            )
        })?;
        let region = endpoint
            .host_str()
            .and_then(|host| host.strip_prefix("bedrock-runtime."))
            .and_then(|host| host.strip_suffix(".amazonaws.com"))
            .ok_or_else(|| PluginError::invalid("The AWS region is invalid."))?;
        headers = AwsSigV4::sign_post(
            target.as_str(),
            body.as_deref().unwrap_or_default(),
            region,
            "bedrock",
            &aws,
            headers,
        );
    } else {
        if let AuthScheme::QueryKey { name } = &resolved.scheme {
            let query = target
                .query_pairs()
                .filter(|(key, _)| key != name)
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect::<Vec<_>>();
            target.set_query(None);
            target.query_pairs_mut().extend_pairs(query);
            if let Some(token) = resolved.token.as_deref() {
                target.query_pairs_mut().append_pair(name, token);
            }
        }
        nora_ai::auth_headers::apply_resolved_auth_with_merge(
            &resolved,
            &mut headers,
            |headers, name, value| {
                headers.retain(|key, _| !key.eq_ignore_ascii_case(&name));
                headers.insert(name, value);
            },
        );
    }
    Ok(HttpRequest {
        method,
        url: target.into(),
        headers,
        body,
    })
}

fn parse_aws(raw: &str) -> Option<AwsSigV4Credentials> {
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return Some(AwsSigV4Credentials {
            access_key_id: value
                .get("accessKeyId")
                .or_else(|| value.get("AWS_ACCESS_KEY_ID"))
                .and_then(Value::as_str)?
                .into(),
            secret_access_key: value
                .get("secretAccessKey")
                .or_else(|| value.get("AWS_SECRET_ACCESS_KEY"))
                .and_then(Value::as_str)?
                .into(),
            session_token: value
                .get("sessionToken")
                .or_else(|| value.get("AWS_SESSION_TOKEN"))
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    let mut parts = raw.splitn(3, ':');
    let access = parts.next().filter(|value| !value.is_empty())?;
    let secret = parts.next().filter(|value| !value.is_empty())?;
    Some(AwsSigV4Credentials {
        access_key_id: access.into(),
        secret_access_key: secret.into(),
        session_token: parts
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models;
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
    async fn anthropic_compatible_packages_keep_their_declared_bearer_authentication() {
        for id in [
            "kimi-coding",
            "minimax",
            "minimax-cn",
            "opencode",
            "xiaomi",
            "zai",
        ] {
            let definition = definition(id);
            assert_eq!(definition.api, "anthropic-messages", "{id}");
            assert_eq!(
                definition.config.api_key_auth,
                Some(AuthScheme::Bearer),
                "{id}"
            );
            let outgoing = build_request(
                &definition,
                request(
                    &format!("{}/v1/messages", definition.base_url.trim_end_matches('/')),
                    json!({"type":"apiKey","access":"private-compatible-key"}),
                ),
            )
            .await
            .unwrap();
            assert_eq!(
                outgoing.headers["Authorization"], "Bearer private-compatible-key",
                "{id}"
            );
            assert!(
                !outgoing
                    .headers
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case("x-api-key")),
                "{id}"
            );
        }
    }

    #[tokio::test]
    async fn anthropic_and_custom_packages_keep_the_default_api_key_header() {
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
            assert!(!outgoing
                .headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("authorization")));
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
    async fn anthropic_oauth_adds_the_cli_system_prefix_and_keeps_beta_flags() {
        use base64::Engine;
        let mut incoming = request(
            "https://api.anthropic.com/v1/messages",
            json!({"type":"oauth","access":"oauth-access","credentialProvider":"anthropic"}),
        );
        incoming
            .headers
            .insert("anthropic-beta".into(), "context-1m-2025-08-07".into());
        incoming.body = Some(
            base64::engine::general_purpose::STANDARD
                .encode(br#"{"system":"User system","messages":[]}"#),
        );
        let outgoing = build_request(&definition("anthropic"), incoming)
            .await
            .unwrap();
        assert_eq!(
            outgoing.headers["anthropic-beta"],
            "context-1m-2025-08-07,oauth-2025-04-20"
        );
        assert_eq!(outgoing.headers["Authorization"], "Bearer oauth-access");
        let body: Value = serde_json::from_slice(outgoing.body.as_ref().unwrap()).unwrap();
        assert!(body["system"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("You are Claude Code"));
        assert_eq!(body["system"][1]["text"], "User system");
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
    async fn declared_protocol_bases_are_allowed_but_custom_addresses_do_not_keep_the_old_origins()
    {
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
        assert_eq!(
            models::grok_model_selection("grok@131072"),
            ("grok", Some(131072))
        );
        for id in [
            "grok@0",
            "grok@-1",
            "grok@oops",
            "grok@18446744073709551615k",
        ] {
            assert_eq!(models::grok_model_selection(id), (id, None));
        }
    }
}
