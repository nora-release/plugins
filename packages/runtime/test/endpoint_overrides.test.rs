use super::*;
#[test]
fn balance_key_placeholders_are_encoded_once_in_urls_and_raw_in_headers() {
    for marker in [
        "{key}",
        "{apiKey}",
        "{api_key}",
        "{API_KEY}",
        "{token}",
        "{{key}}",
        "${API_KEY}",
    ] {
        assert_eq!(
            replace_key(
                &format!("https://test.invalid/balance?key={marker}"),
                "a&b +=?",
                true
            ),
            "https://test.invalid/balance?key=a%26b+%2B%3D%3F"
        );
        assert_eq!(replace_key(marker, "a&b +=?", false), "a&b +=?");
    }
}
#[test]
fn account_tokens_keep_existing_schemes_and_only_jwts_gain_bearer() {
    assert_eq!(account_token("key", "key"), None);
    assert_eq!(account_token("bEaReR key", "key"), None);
    assert_eq!(authorization("raw-token"), "raw-token");
    assert_eq!(authorization("eyJx.a.b"), "Bearer eyJx.a.b");
    assert_eq!(authorization("Bearer eyJx.a.b"), "Bearer eyJx.a.b");
}

use async_trait::async_trait;
use nora_ai::{HttpClientError, HttpStreamingResponse};
use std::sync::Mutex;
struct Mock {
    requests: Mutex<Vec<HttpRequest>>,
    reply: Value,
}
#[async_trait]
impl HttpClient for Mock {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        Ok(HttpStreamingResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::iter(vec![Ok(self
                .reply
                .to_string()
                .into_bytes()
                .into())])),
        })
    }
}
fn mock(reply: Value) -> Mock {
    Mock {
        requests: Mutex::new(Vec::new()),
        reply,
    }
}
fn definition() -> Definition {
    Definition::parse(json!({"id":"fixture","name":"Fixture","api":"openai-completions","baseURL":"https://relay.invalid/v1","config":{"keyProvider":"fixture-key","customTemplate":true}})).unwrap()
}
fn credentials() -> Value {
    json!({"type":"apiKey","access":"api-private&+"})
}
fn resolved() -> ResolvedProviderAuth {
    ResolvedProviderAuth {
        token: Some("api-private&+".into()),
        scheme: nora_ai::AuthScheme::Bearer,
        ..Default::default()
    }
}
fn header<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}
#[tokio::test]
async fn balance_url_headers_post_and_multiple_amounts_match_magpie_without_disclosing_secrets() {
    let mut definition = definition();
    definition.config.balance_url =
        "https://balance.invalid/query?key={key}&private=query-private".into();
    definition.config.balance_path =
        "left: $data.left; used: data.ratio %; next: data.missing; note: data.note".into();
    definition.config.balance_method = "post".into();
    definition
        .config
        .headers
        .insert("X-User-Key".into(), "{api_key}".into());
    definition
        .config
        .headers
        .insert("Authorization".into(), "Custom explicit-private".into());
    let client = mock(
        json!({"data":{"left":3.5,"ratio":0.4,"note":"query-private api-private&+ Custom explicit-private"}}),
    );
    let result = balance(&definition, &credentials(), &resolved(), &client)
        .await
        .unwrap();
    let requests = client.requests.lock().unwrap();
    let request = &requests[0];
    assert_eq!(request.method, HttpMethod::Post);
    assert!(request.body.is_none());
    assert_eq!(
        request.url,
        "https://balance.invalid/query?key=api-private%26%2B&private=query-private"
    );
    assert_eq!(
        header(request, "authorization"),
        Some("Custom explicit-private")
    );
    assert_eq!(header(request, "x-user-key"), Some("api-private&+"));
    assert_eq!(result["balances"][0]["amount"], 3.5);
    assert_eq!(result["balances"][0]["unit"], "USD");
    assert_eq!(result["items"][0]["usedPercent"], 40.0);
    assert_eq!(result["items"][0]["countsForRouting"], false);
    assert_eq!(result["balanceParts"][2]["text"], "—");
    let text = result.to_string();
    for secret in ["query-private", "api-private", "explicit-private"] {
        assert!(!text.contains(secret), "{text}");
    }
}
#[tokio::test]
async fn balance_account_token_overrides_only_balance_authorization_and_key_query_without_token_is_rewritten(
) {
    let mut definition = definition();
    definition.config.balance_url =
        "https://relay.invalid/panel/api/user/self?private=old-query".into();
    definition.config.balance_path = "$data.quota / 500000".into();
    let client = mock(json!({"data":{"total_available":1000000}}));
    let result = balance(&definition, &credentials(), &resolved(), &client)
        .await
        .unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].url,
        "https://relay.invalid/panel/api/usage/token"
    );
    assert_eq!(
        header(&requests[0], "authorization"),
        Some("Bearer api-private&+")
    );
    assert_eq!(result["balances"][0]["amount"], 2.0);
    drop(requests);
    definition.config.balance_token = "eyJaccount.payload.signature".into();
    definition
        .config
        .headers
        .insert("new-api-user".into(), "42".into());
    definition
        .config
        .headers
        .insert("Authorization".into(), "Custom other-private".into());
    let client = mock(json!({"data":{"quota":2000000}}));
    balance(&definition, &credentials(), &resolved(), &client)
        .await
        .unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, definition.config.balance_url);
    assert_eq!(
        header(&requests[0], "authorization"),
        Some("Bearer eyJaccount.payload.signature")
    );
    assert_eq!(header(&requests[0], "new-api-user"), Some("42"));
    // The resolver handed to balance remains the model/chat API credential.
    assert_eq!(resolved().token.as_deref(), Some("api-private&+"));
}
#[tokio::test]
async fn account_tokens_cannot_be_sent_to_key_only_endpoints_and_user_cookie_stops_rewriting() {
    for (url, field) in [
        ("https://relay.invalid/api/usage/token", "/api/user/self"),
        ("https://relay.invalid/v1/usage", "/api/v1/user/profile"),
    ] {
        let mut definition = definition();
        definition.config.balance_url = url.into();
        definition.config.balance_token = "account-private".into();
        let client = mock(json!({}));
        let error = balance(&definition, &credentials(), &resolved(), &client)
            .await
            .unwrap_err();
        assert!(error.message.contains(field));
        assert!(!error.message.contains("account-private"));
        assert!(client.requests.lock().unwrap().is_empty());
    }
    let mut definition = definition();
    definition.config.balance_url = "https://relay.invalid/api/user/self".into();
    definition
        .config
        .headers
        .insert("Cookie".into(), "session=cookie-private".into());
    let client = mock(json!({"data":{"quota":500000}}));
    balance(&definition, &credentials(), &resolved(), &client)
        .await
        .unwrap();
    assert_eq!(
        client.requests.lock().unwrap()[0].url,
        definition.config.balance_url
    );
}
#[tokio::test]
async fn known_default_balance_sources_and_reader_tables_match_magpie() {
    for (base, want, reply, amount, currency) in [
        (
            "https://api.deepseek.com/v1",
            "https://api.deepseek.com/user/balance",
            json!({"balance_infos":[{"currency":"CNY","total_balance":"110"}]}),
            110.0,
            "CNY",
        ),
        (
            "https://api.moonshot.cn/v1",
            "https://api.moonshot.cn/v1/users/me/balance",
            json!({"data":{"available_balance":49.58894}}),
            49.58894,
            "CNY",
        ),
        (
            "https://api.moonshot.ai/v1",
            "https://api.moonshot.ai/v1/users/me/balance",
            json!({"data":{"available_balance":1.5}}),
            1.5,
            "USD",
        ),
        (
            "https://openrouter.ai/api/v1",
            "https://openrouter.ai/api/v1/credits",
            json!({"data":{"total_credits":20,"total_usage":3.5}}),
            16.5,
            "USD",
        ),
        (
            "https://api.siliconflow.cn/v1",
            "https://api.siliconflow.cn/v1/user/info",
            json!({"data":{"totalBalance":"88.88"}}),
            88.88,
            "CNY",
        ),
        (
            "https://api.siliconflow.com/v1",
            "https://api.siliconflow.com/v1/user/info",
            json!({"data":{"totalBalance":"88.88"}}),
            88.88,
            "USD",
        ),
        (
            "https://api.stepfun.com/step_plan/v1",
            "https://api.stepfun.com/v1/accounts",
            json!({"balance":26}),
            26.0,
            "CNY",
        ),
        (
            "https://api.stepfun.ai/v1",
            "https://api.stepfun.ai/v1/accounts",
            json!({"balance":0}),
            0.0,
            "USD",
        ),
        (
            "https://api.commandcode.ai/provider/v1",
            "https://api.commandcode.ai/alpha/billing/credits",
            json!({"credits":{"monthlyCredits":12.3,"purchasedCredits":2,"freeCredits":0}}),
            14.3,
            "USD",
        ),
        (
            "https://ollama.com/v1",
            "https://ollama.com/api/balance",
            json!({"purchased":{"balance_usd":"8"}}),
            8.0,
            "USD",
        ),
        (
            "https://aihubmix.com/v1",
            "https://aihubmix.com/dashboard/billing/remain",
            json!({"total_usage":12}),
            12.0,
            "USD",
        ),
    ] {
        let mut definition = definition();
        definition.base_url = base.into();
        assert!(applies(&definition, &credentials()));
        let client = mock(reply);
        let result = balance(&definition, &credentials(), &resolved(), &client)
            .await
            .unwrap();
        assert_eq!(client.requests.lock().unwrap()[0].url, want);
        assert_eq!(result["balances"][0]["amount"], amount);
        assert_eq!(result["balances"][0]["currency"], currency);
    }
    for base in [
        "https://unknown.invalid/v1",
        "https://api.deepseek.com.evil.invalid/v1",
        "http://api.deepseek.com/v1",
        "https://api.deepseek.com:444/v1",
    ] {
        let mut definition = definition();
        definition.base_url = base.into();
        assert!(!applies(&definition, &credentials()));
    }
}
#[test]
fn explicit_balance_overrides_preserve_subscription_usage_and_configuration_validation() {
    let mut definition = definition();
    definition.config.balance_url = "https://balance.invalid/private?q=secret".into();
    assert!(!applies(
        &definition,
        &json!({"type":"oauth","access":"subscription"})
    ));
    definition.config.balance_method = "DELETE".into();
    assert!(validate(&definition).is_err());
    definition.config.balance_method = "GET".into();
    definition.config.models_url = "file:///secret".into();
    assert!(validate(&definition).is_err());
    definition.config.models_url.clear();
    definition.config.insecure_tls = true;
    assert!(validate(&definition).is_ok());
    definition.config.extra.remove("customTemplate");
    assert!(validate(&definition).is_err());
}

#[tokio::test]
async fn api_key_pasted_as_balance_token_uses_normal_api_key_authorization() {
    for token in ["api-private&+", "Bearer api-private&+"] {
        let mut definition = definition();
        definition.config.balance_token = token.into();
        definition.config.balance_url = "https://relay.invalid/api/usage/token".into();
        let client = mock(json!({"data":{"total_available":500000}}));
        let result = balance(&definition, &credentials(), &resolved(), &client)
            .await
            .unwrap();
        assert_eq!(result["balances"][0]["amount"], 1.0);
        assert_eq!(
            header(&client.requests.lock().unwrap()[0], "authorization"),
            Some("Bearer api-private&+")
        );
        definition.config.balance_url.clear();
        definition.base_url = "https://aihubmix.com/v1".into();
        definition.config.balance_method = "POST".into();
        let client = mock(json!({"total_usage":3.0}));
        balance(&definition, &credentials(), &resolved(), &client)
            .await
            .unwrap();
        let sent = client.requests.lock().unwrap();
        assert_eq!(sent[0].url, "https://aihubmix.com/dashboard/billing/remain");
        assert_eq!(sent[0].method, HttpMethod::Get);
        assert_eq!(
            header(&sent[0], "authorization"),
            Some("Bearer api-private&+")
        );
    }
}
#[tokio::test]
async fn query_and_bare_balance_token_echoes_are_redacted_from_public_parts() {
    let mut definition = definition();
    definition.config.balance_url =
        "https://balance.invalid/balance?key={key}&q=query-private".into();
    definition.config.balance_path = "data.note".into();
    definition.config.balance_token = "Bearer token-private".into();
    let client = mock(json!({"data":{"note":"token-private query-private api-private%26%2B"}}));
    let reply = balance(&definition, &credentials(), &resolved(), &client)
        .await
        .unwrap();
    for secret in ["token-private", "query-private", "api-private"] {
        assert!(!reply.to_string().contains(secret));
    }
}
