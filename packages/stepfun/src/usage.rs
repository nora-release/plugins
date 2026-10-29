use serde_json::Value;
pub fn parse(payload: &Value) -> Result<Value, String> {
    nora_provider_quota::balance_at(payload, "/balance", "USD")
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
