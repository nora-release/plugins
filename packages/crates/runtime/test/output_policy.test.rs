use super::*;
use base64::Engine;
use std::sync::Mutex;

struct Upstream(Mutex<Vec<HttpRequest>>);
#[async_trait::async_trait]
impl HttpClient for Upstream {
    async fn stream(
        &self,
        request: HttpRequest,
    ) -> Result<HttpStreamingResponse, nora_ai::HttpClientError> {
        self.0.lock().unwrap().push(request);
        Ok(HttpStreamingResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::empty()),
        })
    }
}

fn fixture(api: &str, extra: Value) -> Definition {
    Definition::from_fields(json!({
        "id":"compatible","name":"Custom","api":api,"baseURL":"https://custom.invalid/v1",
        "config":{"keyProvider":"custom","extraBody":extra}
    }))
    .unwrap()
}

fn request(path: &str, cap: Option<u64>, body: Value) -> FetchRequest {
    serde_json::from_value(json!({
        "account":"member","credentials":{"type":"apiKey","access":"fixture"},
        "url":format!("https://custom.invalid/v1/{path}"),"method":"POST",
        "constraints":{"outputTokenLimit":cap},
        "body":base64::engine::general_purpose::STANDARD.encode(body.to_string())
    }))
    .unwrap()
}

#[tokio::test]
async fn forced_connection_defaults_cannot_enable_explicitly_disabled_reasoning() {
    for (api, path, body, extra, pointer, expected) in [
        (
            "openai-completions",
            "chat/completions",
            json!({"reasoning_effort":"none"}),
            json!({"!reasoning_effort":"high"}),
            "/reasoning_effort",
            json!("none"),
        ),
        (
            "openai-responses",
            "responses",
            json!({"reasoning":{"effort":"none"}}),
            json!({"!reasoning":{"effort":"high","summary":"auto"}}),
            "/reasoning/effort",
            json!("none"),
        ),
        (
            "anthropic-messages",
            "messages",
            json!({"thinking":{"type":"disabled"}}),
            json!({"!thinking":{"type":"enabled","budget_tokens":16000}}),
            "/thinking",
            json!({"type":"disabled"}),
        ),
        (
            "bedrock-converse-stream",
            "model/m/converse-stream",
            json!({"additionalModelRequestFields":{"thinking":{"type":"enabled","budget_tokens":16000}}}),
            json!({}),
            "/additionalModelRequestFields/thinking",
            json!({"type":"disabled"}),
        ),
        (
            "google-generative-ai",
            "models/gemini-2.5-flash:generateContent",
            json!({"generationConfig":{"thinkingConfig":{"thinkingBudget":0}}}),
            json!({"!generationConfig":{"thinkingConfig":{"thinkingBudget":16000},"temperature":0.4}}),
            "/generationConfig/thinkingConfig",
            json!({"thinkingBudget":0}),
        ),
        (
            "google-generative-ai",
            "models/gemini-2.5-pro:generateContent",
            json!({"generationConfig":{"thinkingConfig":{"thinkingBudget":128}}}),
            json!({"!generationConfig":{"thinkingConfig":{"thinkingBudget":16000}}}),
            "/generationConfig/thinkingConfig",
            json!({"thinkingBudget":128}),
        ),
        (
            "google-generative-ai",
            "models/gemini-2.0-flash:generateContent",
            json!({"generationConfig":{"temperature":0.5}}),
            json!({"!generationConfig":{"thinkingConfig":{"thinkingBudget":16000}}}),
            "/generationConfig/thinkingConfig",
            Value::Null,
        ),
    ] {
        let definition = fixture(api, extra);
        let mut request = request(path, None, body);
        request.constraints.reasoning_disabled = true;
        let outgoing = build_request(&definition, request).await.unwrap();
        let body: Value = serde_json::from_slice(outgoing.body.as_ref().unwrap()).unwrap();
        assert_eq!(
            body.pointer(pointer).unwrap_or(&Value::Null),
            &expected,
            "{api}: {body}"
        );
    }
}

#[tokio::test]
async fn forced_connection_defaults_cannot_raise_the_selected_route_output_limit() {
    for (api, path, body, extra, pointer) in [
        (
            "openai-completions",
            "chat/completions",
            json!({"model":"m","max_tokens":4000,"max_completion_tokens":4000}),
            json!({"!max_tokens":100000,"!max_completion_tokens":100000}),
            "/max_tokens",
        ),
        (
            "openai-responses",
            "responses",
            json!({"model":"m","max_output_tokens":4000}),
            json!({"!max_output_tokens":100000}),
            "/max_output_tokens",
        ),
        (
            "anthropic-messages",
            "messages",
            json!({"model":"m","max_tokens":4000,"thinking":{"type":"enabled","budget_tokens":3999}}),
            json!({"!max_tokens":100000,"!thinking":{"type":"enabled","budget_tokens":16000}}),
            "/max_tokens",
        ),
        (
            "google-generative-ai",
            "models/gemini-3-flash:generateContent",
            json!({"model":"m","generationConfig":{"maxOutputTokens":4000}}),
            json!({"!generationConfig":{"maxOutputTokens":100000,"temperature":0.4}}),
            "/generationConfig/maxOutputTokens",
        ),
        (
            "bedrock-converse-stream",
            "model/m/converse-stream",
            json!({"model":"m","inferenceConfig":{"maxTokens":100000},"additionalModelRequestFields":{"thinking":{"type":"enabled","budget_tokens":16000},"vendor_setting":"kept"}}),
            json!({}),
            "/inferenceConfig/maxTokens",
        ),
    ] {
        let definition = fixture(api, extra);
        let outgoing = build_request(&definition, request(path, Some(4000), body))
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(outgoing.body.as_ref().unwrap()).unwrap();
        assert_eq!(body.pointer(pointer), Some(&json!(4000)), "{api}: {body}");
        if api == "openai-completions" {
            assert_eq!(body["max_completion_tokens"], 4000);
        }
        if api == "anthropic-messages" {
            assert_eq!(body["thinking"]["budget_tokens"], 3999);
        }
        if api == "bedrock-converse-stream" {
            assert_eq!(
                body["additionalModelRequestFields"]["thinking"]["budget_tokens"],
                3999
            );
            assert_eq!(
                body["additionalModelRequestFields"]["vendor_setting"],
                "kept"
            );
        }
        if api == "google-generative-ai" {
            assert_eq!(body["generationConfig"]["temperature"], 0.4);
        }
        let upstream = Upstream(Mutex::new(Vec::new()));
        let response = send(
            &upstream,
            &definition,
            &json!({"type":"apiKey","access":"fixture"}),
            api,
            outgoing,
        )
        .await
        .unwrap();
        assert_eq!(response.status, 200);
        let requests = upstream.0.lock().unwrap();
        let sent: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
        assert_eq!(sent, body);
    }
}

#[tokio::test]
async fn public_body_and_metadata_cannot_supply_a_private_route_limit() {
    let definition = fixture("openai-completions", json!({"!max_tokens":100000}));
    let mut request = request(
        "chat/completions",
        None,
        json!({"model":"m","max_tokens":4000,"outputTokenLimit":1}),
    );
    request
        .metadata
        .insert("outputTokenLimit".into(), "1".into());
    let outgoing = build_request(&definition, request).await.unwrap();
    let body: Value = serde_json::from_slice(outgoing.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["max_tokens"], 100000);
}

#[tokio::test]
async fn a_route_limit_does_not_invent_optional_output_fields_or_raise_smaller_requests() {
    let definition = fixture("openai-responses", json!({}));
    for (body, expected) in [
        (json!({"model":"m"}), None),
        (json!({"model":"m","max_output_tokens":2000}), Some(2000)),
    ] {
        let outgoing = build_request(&definition, request("responses", Some(4000), body))
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(outgoing.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["max_output_tokens"].as_u64(), expected);
    }
}
