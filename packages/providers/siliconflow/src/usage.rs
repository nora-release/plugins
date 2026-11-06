use serde_json::Value;
pub fn parse(payload: &Value) -> Result<Value, String> {
    if payload["code"] == 20092 {
        return Err("SiliconFlow retired its balance query and has no replacement API; check the provider console for balances and vouchers.".into());
    }
    if payload["code"]
        .as_i64()
        .is_some_and(|code| code != 20000 && code != 0)
    {
        return Err("SiliconFlow refused the balance query.".into());
    }
    nora_provider_quota::balance_at(payload, "/data/totalBalance", "CNY")
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
