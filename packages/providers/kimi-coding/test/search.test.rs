use super::*;
use async_trait::async_trait;
use nora_ai::{HttpClientError, HttpStreamingResponse};
use std::sync::Mutex;
struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    status: u16,
    value: Value,
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let body = self.value.to_string().into_bytes();
        Ok(HttpStreamingResponse {
            status: self.status,
            headers: Default::default(),
            body: Box::pin(futures::stream::once(async move { Ok(body.into()) })),
        })
    }
}
#[tokio::test]
async fn coding_search_uses_the_selected_base_and_key_with_kimi_cli_body() {
    for base in [
        "https://api.kimi.com/coding",
        "https://api.kimi.ai/coding/v1",
        "http://localhost:9000/custom/v1",
    ] {
        let client = Client {
            requests: Default::default(),
            status: 200,
            value: json!({"search_results":[{"title":"<b>Docs</b>","url":"https://example.com/docs","content":"Verified <b>facts</b>"}]}),
        };
        let auth = ResolvedProviderAuth {
            token: Some("private-key".into()),
            base_url: Some(base.into()),
            ..Default::default()
        };
        let result = search(&auth, "release", &client).await.unwrap();
        assert_eq!(result["sources"][0]["title"], "Docs");
        assert!(result["text"].as_str().unwrap().contains("Verified facts"));
        let requests = client.requests.lock().unwrap();
        let sent = &requests[0];
        assert_eq!(sent.headers["authorization"], "Bearer private-key");
        assert_eq!(sent.headers["user-agent"], "KimiCLI/1.52.0");
        let endpoint = if base.ends_with("/coding") {
            format!("{base}/v1/search")
        } else {
            format!("{base}/search")
        };
        assert_eq!(sent.url, endpoint);
        assert_eq!(
            serde_json::from_slice::<Value>(sent.body.as_ref().unwrap()).unwrap(),
            json!({"text_query":"release","limit":5,"enable_page_crawling":false,"timeout_seconds":30})
        );
    }
}
#[tokio::test]
async fn refusal_malformed_and_empty_searches_allow_router_fallback_without_exposing_secrets() {
    for (status, value) in [
        (401, json!({"error":"private-key"})),
        (429, json!({"error":"private-key"})),
        (200, json!({"wrong":[]})),
        (200, json!({"search_results":[]})),
    ] {
        let client = Client {
            requests: Default::default(),
            status,
            value,
        };
        let auth = ResolvedProviderAuth {
            token: Some("private-key".into()),
            base_url: Some("https://api.kimi.com/coding".into()),
            ..Default::default()
        };
        let err = search(&auth, "release", &client).await.unwrap_err();
        assert!(!err.to_string().contains("private-key"));
    }
}
