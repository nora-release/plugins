use nora_provider::usage::parsing::number;
use nora_provider_quota::{balance, finish};
use serde_json::Value;

pub fn parse(payload: &Value) -> Result<Value, String> {
    let credits =
        number(&payload["data"]["total_credits"]).ok_or("OpenRouter did not return credits.")?;
    let used = number(&payload["data"]["total_usage"]).unwrap_or(0.0);
    finish(
        Vec::new(),
        vec![balance("balance", "Balance", credits - used, "USD")],
    )
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
