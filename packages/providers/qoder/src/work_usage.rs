use crate::*;

pub(crate) fn fallback_catalog() -> Value {
    json!({"qwork":[
        {"key":"flash","display_name":"标准","is_vl":false,"is_reasoning":false,"max_input_tokens":131072},
        {"key":"pro","display_name":"高级","is_vl":false,"is_reasoning":false,"max_input_tokens":131072},
        {"key":"qwen3.8-max-preview","display_name":"Qwen3.8-Max","is_vl":false,"is_reasoning":false,"max_input_tokens":131072}
    ]})
}
pub(crate) async fn fetch(
    provider: &Qoder,
    client: &dyn HttpClient,
    credentials: Value,
) -> PluginResult<Value> {
    let mut credentials = auth::fresh(provider, client, &credentials).await?;
    let context = usage::page(
        provider,
        client,
        &mut credentials,
        "/api/v1/adapter/user/account-context?include=user,plan,quota",
        HttpMethod::Get,
        None,
        &BTreeMap::new(),
    )
    .await?;
    let wallets = if provider.site == Site::QwenWork
        && unwrap(&context)
            .pointer("/user/is_biz")
            .and_then(Value::as_bool)
            != Some(true)
    {
        usage::page(
            provider,
            client,
            &mut credentials,
            "/api/v1/adapter/user/wallets",
            HttpMethod::Get,
            None,
            &BTreeMap::new(),
        )
        .await
        .ok()
    } else {
        None
    };
    Ok(parse(&context, wallets.as_ref()))
}
fn unwrap(value: &Value) -> &Value {
    value.get("data").filter(|v| v.is_object()).unwrap_or(value)
}
fn finite(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite())
}
fn number_text(value: f64) -> String {
    format!("{}", (value * 100.0).round() / 100.0)
}
pub(crate) fn parse(context: &Value, wallets: Option<&Value>) -> Value {
    let data = unwrap(context);
    let quota = &data["quota"];
    let mut items = Vec::new();
    let mut balances = Vec::new();
    let plan = data.pointer("/plan/name").and_then(Value::as_str);
    let until = data
        .pointer("/plan/next_refresh_date")
        .or_else(|| data.pointer("/plan/next_due_date"))
        .and_then(usage::date)
        .and_then(|at| {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at.min(i64::MAX as u64) as i64)
        })
        .map(|date| date.to_rfc3339());
    let total = finite(&quota["total"]);
    let remaining = finite(&quota["remaining"]);
    let used = finite(&quota["used"]).or_else(|| Some(total? - remaining?));
    if let Some((total, used)) = total
        .zip(used)
        .filter(|(total, used)| *total > 0.0 && *used >= 0.0)
    {
        let mut item = json!({"id":"credits","label":"Credits","used":used,"limit":total,"usedPercent":(used/total*100.0).min(100.0),"unit":"credits","countsForRouting":true,"value":format!("{} / {} credits",number_text(used),number_text(total))});
        if let Some(until) = &until {
            item["resetsAt"] = json!(until);
        }
        items.push(item);
    }
    if let Some(remaining) = remaining {
        let mut display = format!("{} credits", number_text(remaining));
        if let Some(wallets) = wallets.map(unwrap) {
            let pools = [
                ("daily_credits", "daily"),
                ("monthly_credits", "monthly"),
                ("longterm_credits", "long-term"),
            ]
            .into_iter()
            .filter_map(|(key, label)| {
                let pool = &wallets[key];
                let amount = finite(
                    pool.get("total_balance")
                        .or_else(|| pool.get("totalBalance"))
                        .unwrap_or(&Value::Null),
                )?;
                (amount > 0.0).then(|| format!("{label} {}", number_text(amount)))
            })
            .collect::<Vec<_>>();
            if !pools.is_empty() {
                display.push_str(&format!(" ({})", pools.join(" · ")));
            }
        }
        let mut balance = json!({"id":"credits","label":"Credits","amount":remaining,"value":display,"unit":"credits"});
        if let Some(until) = &until {
            balance["expiresAt"] = json!(until);
        }
        balances.push(balance);
        let exceeded = remaining <= 0.0
            || ["exceeded", "is_quota_exceeded", "isQuotaExceeded"]
                .into_iter()
                .any(|key| quota[key] == true);
        if items.is_empty() && exceeded {
            items.push(json!({"id":"credits","label":"Credits","usedPercent":100,"countsForRouting":true,"value":"No credits left"}));
        }
    }
    let available = !items.is_empty() || !balances.is_empty();
    let mut result = json!({"available":available,"items":items,"balances":balances,"account":{},"updatedAt":chrono::Utc::now().to_rfc3339()});
    if let Some(plan) = plan {
        result["plan"] = json!(plan);
        result["account"]["plan"] = json!(plan);
    }
    if !available {
        result["message"] = json!("QwenWork did not provide a credit balance for this account.");
    }
    result
}
#[cfg(test)]
#[path = "../test/work_usage.test.rs"]
mod tests;
