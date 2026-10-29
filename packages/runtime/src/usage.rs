use nora_ai::{HttpClient, HttpRequest, ResolvedProviderAuth};
use serde_json::Value;
use std::time::Duration;

#[path = "usage_parse.rs"]
pub mod parsing;
pub use parsing::unavailable;

pub async fn request_json(
    client: &dyn HttpClient,
    request: HttpRequest,
    timeout: Duration,
) -> Result<Value, String> {
    tokio::time::timeout(timeout, async {
        let response = client
            .stream(request)
            .await
            .map_err(|_| "The provider usage endpoint could not be reached")?;
        if !(200..300).contains(&response.status) {
            return Err(format!(
                "The provider usage request returned HTTP {}",
                response.status
            ));
        }
        let text = nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024)
            .await
            .map_err(|_| "The provider usage response stopped before it finished")?;
        serde_json::from_str(&text)
            .map_err(|_| "The provider returned an invalid usage response".into())
    })
    .await
    .map_err(|_| "The provider usage request timed out".to_string())?
}

pub fn header<'a>(auth: &'a ResolvedProviderAuth, name: &str) -> Option<&'a str> {
    auth.headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
