use nora_provider::usage::parsing::number;
use nora_provider_quota::{balance, finish};
use serde_json::Value;

pub fn parse(payload: &Value) -> Result<Value, String> {
    let rows = payload["balance_infos"]
        .as_array()
        .ok_or("DeepSeek did not return balances.")?;
    let mut balances = Vec::new();
    for row in rows {
        let Some(amount) = number(&row["total_balance"]) else {
            continue;
        };
        let Some(currency) = row["currency"].as_str().filter(|currency| {
            !currency.is_empty()
                && currency.len() <= 8
                && currency.bytes().all(|byte| byte.is_ascii_alphabetic())
        }) else {
            continue;
        };
        let currency = currency.to_ascii_uppercase();
        balances.push(balance(
            &format!("balance:{currency}"),
            "Balance",
            amount,
            &currency,
        ));
        if let (Some(granted), Some(topped)) = (
            number(&row["granted_balance"]),
            number(&row["topped_up_balance"]),
        ) {
            if granted != 0.0 {
                balances.push(balance(
                    &format!("granted:{currency}"),
                    "Granted",
                    granted,
                    &currency,
                ));
                balances.push(balance(
                    &format!("topped_up:{currency}"),
                    "Topped up",
                    topped,
                    &currency,
                ));
            }
        }
    }
    balances.sort_by(|left, right| {
        let positive = |value: &Value| value["amount"].as_f64().is_some_and(|amount| amount > 0.0);
        positive(right)
            .cmp(&positive(left))
            .then_with(|| left["unit"].as_str().cmp(&right["unit"].as_str()))
            .then_with(|| left["id"].as_str().cmp(&right["id"].as_str()))
    });
    finish(Vec::new(), balances)
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
