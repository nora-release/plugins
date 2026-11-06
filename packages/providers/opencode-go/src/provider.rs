use async_trait::async_trait;
use nora_ai::{HttpClient, ResolvedProviderAuth};
use nora_provider::ProviderAdapter;
use nora_provider_quota::{fetch_with_headers, unsupported_origin};
use serde_json::Value;

#[path = "usage.rs"]
mod usage;

#[derive(Debug)]
pub struct OpenCodeGo;

#[async_trait]
impl ProviderAdapter for OpenCodeGo {
    fn id(&self) -> Option<&'static str> {
        Some("opencode-go")
    }

    async fn usage(
        &self,
        auth: &ResolvedProviderAuth,
        _account: Option<&str>,
        client: &dyn HttpClient,
    ) -> Result<Value, String> {
        let session = session_id()?;
        let headers = vec![
            ("x-opencode-session".into(), session.clone()),
            ("x-opencode-session-id".into(), session),
        ];
        let payload = fetch_with_headers(
            auth,
            "opencode.ai",
            "https://opencode.ai/zen/go/v1/usage",
            true,
            &headers,
            client,
        )
        .await
        .map_err(|error| error.to_string())?;
        let Some(payload) = payload else {
            return Ok(unsupported_origin());
        };
        usage::parse(&payload)
    }
}

fn session_id() -> Result<String, String> {
    const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut random = [0u8; 14];
    getrandom::getrandom(&mut random)
        .map_err(|_| "The OpenCode usage session could not be created.")?;
    let clock =
        !((nora_ai::messages::now_millis() as u64).wrapping_mul(0x1000) + 1) & 0xffffffffffff;
    let mut session = format!("ses_{clock:012x}");
    session.extend(
        random
            .into_iter()
            .map(|byte| BASE62[byte as usize % BASE62.len()] as char),
    );
    Ok(session)
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
