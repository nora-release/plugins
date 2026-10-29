use super::*;
use nora_ai::{HttpClientError, HttpRequest, HttpResponse, HttpStreamingResponse};
use serde_json::json;
use std::sync::{Arc, Mutex};

struct Client(Mutex<Vec<HttpRequest>>);
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let response = self.request(request).await?;
        Ok(HttpStreamingResponse {
            status: response.status,
            headers: response.headers,
            body: Box::pin(futures::stream::iter([Ok(response.body.into())])),
        })
    }
    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        assert!(
            Url::parse(&request.url)
                .unwrap()
                .path()
                .ends_with("/models"),
            "{}",
            request.url
        );
        self.0.lock().unwrap().push(request);
        Ok(HttpResponse {status:200,headers:BTreeMap::new(),body:serde_json::to_vec(&json!({"data":[{"id":"chat","model_picker_enabled":true,"capabilities":{"type":"chat"},"supported_endpoints":["/chat/completions"]}]})).unwrap()})
    }
}

#[tokio::test]
async fn discovery_advertises_supported_subscription_transports_separately_from_model_defaults() {
    let mut definition = nora_provider::definition::Definition::parse(
        serde_json::from_str(include_str!("../nora-plugin.json")).unwrap(),
    )
    .unwrap();
    definition.adapter = Arc::new(Copilot::default());
    for api_key in [false, true] {
        let credentials = OAuthCredentials::new(
            if api_key {
                OAuthProviderId::ApiKey
            } else {
                OAuthProviderId::GitHubCopilot
            },
            "fixture",
            None,
            Some(4_000_000_000_000_i64),
            BTreeMap::from([(
                "endpoint".into(),
                JsonValue::string("https://managed.invalid"),
            )]),
        );
        let credentials = nora_provider::auth::encode(&credentials, "github-copilot", false);
        let client = Client(Mutex::new(Vec::new()));
        let path =
            std::env::temp_dir().join(format!("nora-copilot-api-{}-{api_key}", std::process::id()));
        let result =
            nora_provider::discovery::discover(&definition, Some(&credentials), &path, &client)
                .await
                .unwrap();
        if api_key {
            assert!(result.get("apiTransports").is_none());
        } else {
            assert_eq!(
                result["apiTransports"],
                json!({"openai-completions":"github-copilot-chat","openai-responses":"github-copilot-openai-responses","anthropic-messages":"github-copilot-anthropic-messages"})
            );
            assert_eq!(result["defaultAPI"], "github-copilot-chat");
            assert_eq!(result["defaultBaseURL"], "https://managed.invalid");
        }
        assert_eq!(
            result["supportedAPIs"]["chat"],
            json!(["openai-completions"]),
            "{result}"
        );
        assert_eq!(
            client.0.lock().unwrap().len(),
            1,
            "API choices must not fetch the catalog three times"
        );
        let _ = std::fs::remove_dir_all(path);
    }
}
