use serde_json::Value;
pub fn parse(payload: &Value) -> Result<Value, String> {
    if payload["status"] == false || payload["code"].as_i64().is_some_and(|code| code != 0) {
        return Err("Moonshot refused the balance query.".into());
    }
    nora_provider_quota::balance_at(payload, "/data/available_balance", "CNY")
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
