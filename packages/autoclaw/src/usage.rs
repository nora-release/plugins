use super::*;

async fn tasks(
    client: &dyn HttpClient,
    site: Site,
    access: &str,
) -> PluginResult<(Site, Vec<Value>)> {
    for host in [site, site.other()] {
        let reply = transport::call(
            client,
            host.host(),
            "/autoclaw-proxy/proxy/autoclaw-task-list",
            HttpMethod::Get,
            access,
            None,
        )
        .await;
        match reply {
            Err(error) if error.code == refusal::CODE => return Err(error),
            Ok(reply) if reply.code == 0 => {
                if let Some(tasks) = reply.data.as_array() {
                    return Ok((host, tasks.clone()));
                }
            }
            _ => {}
        }
    }
    Err(PluginError::transport(
        "AutoClaw could not read the daily check-in status.",
    ))
}
fn task_status(tasks: &[Value]) -> PluginResult<Value> {
    let Some(task) = tasks.iter().find(|t| t["task_id"] == "daily_signin") else {
        return Ok(json!({"outcome":"inactive"}));
    };
    let outcome = match text(task, "status") {
        "completed" => "done",
        "pending" | "uncompleted" | "not_started" | "todo" => "ready",
        _ => {
            return Err(PluginError::transport(
                "AutoClaw returned an unknown check-in status.",
            ))
        }
    };
    let mut result = json!({"outcome":outcome,"retrySafe":true});
    if let Some(credit) = number(&task["reward_points"]).filter(|n| *n >= 0.0) {
        result["credit"] = json!(credit);
    }
    Ok(result)
}
pub(super) async fn checkin(
    client: &dyn HttpClient,
    site: Site,
    credentials: &Value,
    status_only: bool,
) -> PluginResult<Value> {
    let access = text(credentials, "access");
    let (host, tasks) = tasks(client, site, access).await?;
    let status = task_status(&tasks)?;
    if status_only || status["outcome"] != "ready" {
        return Ok(status);
    }
    // A lost POST acknowledgement is ambiguous. The host persists unknown and
    // queries task status before any new claim; never send a second POST here.
    let result = transport::call(
        client,
        host.host(),
        "/autoclaw-proxy/proxy/autoclaw-task-complete",
        HttpMethod::Post,
        access,
        Some(json!({"task_id":"daily_signin","source_id":"autoclaw"})),
    )
    .await?;
    if result.code != 0 {
        return Err(PluginError::transport(
            "AutoClaw did not confirm the check-in result.",
        ));
    }
    let outcome = if result.data["success"] == true {
        "claimed"
    } else if result.data["already_completed"] == true {
        "done"
    } else if result.data["success"] == false {
        return Ok(json!({"outcome":"failed","retrySafe":true}));
    } else {
        return Err(PluginError::transport(
            "AutoClaw returned no check-in confirmation.",
        ));
    };
    let mut value = json!({"outcome":outcome});
    if let Some(credit) = number(&result.data["reward_points"]).filter(|n| *n >= 0.0) {
        value["credit"] = json!(credit);
    }
    if let Some(streak) = result.data["continuous_days"].as_u64() {
        value["streak"] = json!(streak);
    }
    Ok(value)
}
pub(super) async fn fetch(
    client: &dyn HttpClient,
    site: Site,
    credentials: &Value,
) -> PluginResult<Value> {
    let access = text(credentials, "access");
    let mut wallets = None;
    for host in [site, site.other()] {
        let reply = transport::call(
            client,
            host.host(),
            "/agent-assetmgr/api/v2/wallets?biz_app_id=autoclaw",
            HttpMethod::Get,
            access,
            None,
        )
        .await;
        match reply {
            Err(error) if error.code == refusal::CODE => return Err(error),
            Ok(reply) if reply.code == 0 && reply.data.is_object() => {
                wallets = Some(reply.data);
                break;
            }
            _ => {}
        }
    }
    let data = wallets
        .ok_or_else(|| PluginError::transport("AutoClaw could not read this account's credits."))?;
    let total = number(&data["total_balance"])
        .filter(|n| *n >= 0.0)
        .ok_or_else(|| PluginError::transport("AutoClaw returned an invalid credit balance."))?;
    let mut items = vec![
        json!({"id":"credits","label":"Credits","amount":total,"unit":"Credits","aside":true}),
    ];
    let mut balances =
        vec![json!({"id":"credits","label":"Credits","amount":total,"unit":"Credits"})];
    if let Some(rows) = data["wallets"].as_array() {
        for row in rows
            .iter()
            .filter(|row| row["display"] != false)
            .filter(|row| {
                number(&row["balance"]).is_some_and(|n| n > 0.0 && n.round() != total.round())
            })
            .take(3)
        {
            let label = row
                .get("display_name")
                .or_else(|| row.get("public_wallet_type"))
                .and_then(Value::as_str)
                .filter(|v| {
                    !v.trim().is_empty() && v.len() <= 128 && !v.chars().any(char::is_control)
                })
                .unwrap_or("Credits");
            let amount = number(&row["balance"]).unwrap();
            let kind = row
                .get("public_wallet_type")
                .and_then(Value::as_str)
                .filter(|v| {
                    !v.trim().is_empty() && v.len() <= 128 && !v.chars().any(char::is_control)
                })
                .unwrap_or(label);
            let id = format!("wallet-{kind}");
            items
                .push(json!({"id":id,"label":label,"amount":amount,"unit":"Credits","aside":true}));
            balances.push(json!({"id":id,"label":label,"amount":amount,"unit":"Credits"}));
        }
    }
    if let Ok((_, tasks)) = tasks(client, site, access).await {
        if let Ok(status) = task_status(&tasks) {
            if status["outcome"] != "inactive" {
                items.push(json!({"id":"daily-checkin","label":"Daily check-in","value":if status["outcome"]=="done"{"Done today"}else{"Not checked in"},"aside":true}));
            }
        }
    }
    Ok(
        json!({"available":true,"balance":format!("{total:.0} Credits"),"balances":balances,"items":items}),
    )
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
