use nora_provider::usage::parsing::number;
use nora_provider_quota::{balance, finish};
use serde_json::Value;

pub fn parse(payload: &Value) -> Result<Value, String> {
    let amount = if payload.get("success").is_some() {
        if payload["success"] != true {
            return Err("AiHubMix did not accept the account balance token.".into());
        }
        number(&payload["data"]["quota"]).map(|amount| amount / 500_000.0)
    } else {
        number(&payload["total_usage"])
    }
    .ok_or("AiHubMix did not return a readable balance.")?;
    if amount < 0.0 {
        return Err("This AiHubMix key has no spending limit. Add the account's system access token to read its account balance.".into());
    }
    finish(
        Vec::new(),
        vec![balance("balance", "Balance", amount, "USD")],
    )
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
