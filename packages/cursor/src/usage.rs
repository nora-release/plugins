use super::{auth, network};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeSet;
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
}
fn amount(value: &Value) -> f64 {
    number(value).unwrap_or(0.0)
}
fn reset(value: &Value) -> Option<String> {
    let ms = value
        .as_str()
        .and_then(|s| s.parse::<i64>().ok())
        .or_else(|| value.as_i64())?;
    chrono::DateTime::from_timestamp_millis(ms)
        .filter(|_| ms > 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}
pub(crate) fn pool_base(id: &str) -> String {
    let mut id = id
        .split('@')
        .next()
        .unwrap_or(id)
        .trim()
        .to_ascii_lowercase();
    id = id.strip_prefix("cursor-").unwrap_or(&id).to_owned();
    loop {
        let next = [
            "-extra-high",
            "-thinking",
            "-medium",
            "-xhigh",
            "-none",
            "-high",
            "-fast",
            "-low",
            "-max",
        ]
        .into_iter()
        .find_map(|s| id.strip_suffix(s).map(str::to_owned));
        match next {
            Some(s) => id = s,
            None => return if id == "auto" { "default".into() } else { id },
        }
    }
}
fn first(id: &str) -> bool {
    id == "default"
        || id == "composer"
        || id.starts_with("composer-")
        || ["grok-4.5", "grok-4.6", "grok-4.7"]
            .into_iter()
            .any(|base| id == base || id.starts_with(&format!("{base}-")))
}
#[derive(Debug, PartialEq)]
enum OnDemand {
    Unknown,
    Disabled,
    Unlimited(f64),
    Fixed(f64, f64, Option<f64>),
}
fn ondemand(data: &Value, hard: Option<&Value>) -> OnDemand {
    let s = &data["spendLimitUsage"];
    let used = amount(&s["individualUsed"]) / 100.0;
    let own = number(&s["individualLimit"]).map(|n| {
        if n > 0.0 {
            OnDemand::Fixed(used, n / 100.0, None)
        } else {
            OnDemand::Disabled
        }
    });
    if s["limitType"] == "team" {
        if let Some(own) = own {
            return own;
        }
        if hard.is_some_and(|h| h["noUsageBasedAllowed"] == true || amount(&h["hardLimit"]) <= 0.0)
        {
            return OnDemand::Disabled;
        }
        let pool = amount(&s["pooledLimit"]);
        if pool > 0.0 {
            return OnDemand::Fixed(amount(&s["pooledUsed"]) / 100.0, pool / 100.0, Some(used));
        }
        return if hard.is_some() {
            OnDemand::Unlimited(used)
        } else {
            OnDemand::Unknown
        };
    }
    let Some(hard) = hard else {
        return own.unwrap_or(OnDemand::Unknown);
    };
    if hard["noUsageBasedAllowed"] == true {
        return OnDemand::Disabled;
    }
    let limit = amount(&hard["hardLimit"]);
    if limit >= 2_147_483_647.0 {
        return OnDemand::Unlimited(used);
    }
    if limit > 0.0 {
        OnDemand::Fixed(used, limit, None)
    } else {
        OnDemand::Disabled
    }
}
pub(crate) fn parse(data: &Value, hard: Option<&Value>, ids: &[String]) -> PluginResult<Value> {
    if !data.is_object() {
        return Err(PluginError::transport("Cursor returned invalid usage."));
    }
    let mut items = Vec::new();
    let at = reset(&data["billingCycleEnd"]);
    let bucket: BTreeSet<_> = data["autoBucketModels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(pool_base)
        .collect();
    let in_pool = |id: &str| {
        let base = pool_base(id);
        bucket.contains(&base) || first(&base)
    };
    let mut pool: BTreeSet<String> = BTreeSet::from(["auto".into()]);
    let mut other = BTreeSet::new();
    for id in ids.iter().map(String::as_str).chain(
        data["autoBucketModels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str),
    ) {
        for id in [id, id.split('@').next().unwrap_or(id)] {
            if in_pool(id) {
                pool.insert(id.into());
            } else {
                other.insert(id.to_owned());
            }
        }
    }
    let od = ondemand(data, hard);
    let pooled = matches!(od, OnDemand::Unlimited(_) | OnDemand::Fixed(..));
    if let Some(plan) = data.get("planUsage").filter(|p| p.is_object()) {
        for (id, label, key, models) in [
            ("cursor-models", "Cursor Models", "autoPercentUsed", pool),
            ("other-models", "Other Models", "apiPercentUsed", other),
        ] {
            if let Some(used) = number(&plan[key]) {
                let mut item = json!({"id":id,"label":label,"type":"quota","unit":"percent","usedPercent":used.clamp(0.0,100.0),"value":format!("{:.0}% used",used),"countsForRouting":!pooled&&!models.is_empty(),"modelIDs":models});
                if let Some(at) = &at {
                    item["resetsAt"] = json!(at);
                }
                items.push(item);
            }
        }
        if let Some(used) = number(&plan["totalPercentUsed"]) {
            let mut item = json!({"id":"total","label":"Total","type":"quota","usedPercent":used.clamp(0.0,100.0),"value":format!("{:.0}% used",used),"countsForRouting":false});
            if let Some(at) = &at {
                item["resetsAt"] = json!(at);
            }
            items.push(item);
        }
    }
    let demand = match od {
        OnDemand::Fixed(used, limit, mine) => {
            let mut item = json!({"id":"on-demand","label":"On-demand","type":"quota","unit":"USD","used":used,"limit":limit,"remaining":(limit-used).max(0.0),"usedPercent":(100.0*used/limit).clamp(0.0,100.0),"value":format!("${used:.2} / ${limit:.2}"),"countsForRouting":true});
            if let Some(mine) = mine {
                item["individualUsed"] = json!(mine);
            }
            Some(item)
        }
        OnDemand::Unlimited(used) => Some(
            json!({"id":"on-demand","label":"On-demand","type":"balance","unit":"USD","used":used,"value":format!("${used:.2} used; unlimited"),"countsForRouting":false}),
        ),
        _ => None,
    };
    if let Some(mut item) = demand {
        if let Some(at) = &at {
            item["resetsAt"] = json!(at);
        }
        items.push(item);
    }
    Ok(nora_provider::usage::parsing::result(
        data,
        items,
        Vec::new(),
    ))
}
pub(crate) async fn fetch(
    client: &dyn nora_ai::HttpClient,
    token: &str,
    version: &str,
    ids: &[String],
) -> PluginResult<Value> {
    let empty = json!({});
    let (period, hard) = tokio::join!(
        network::unary(
            client,
            "aiserver.v1.DashboardService/GetCurrentPeriodUsage",
            token,
            version,
            &empty
        ),
        network::unary(
            client,
            "aiserver.v1.DashboardService/GetHardLimit",
            token,
            version,
            &empty
        )
    );
    let mut value = parse(&period?, hard.as_ref().ok(), ids)?;
    // Grok Bot uses a separate quota; it never gates Cursor chat routing.
    if let Some(bot) = bot(client, token).await {
        if let Some(items) = value["items"].as_array_mut() {
            items.push(bot);
        }
    }
    Ok(value)
}
async fn bot(client: &dyn nora_ai::HttpClient, token: &str) -> Option<Value> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token.split('.').nth(1)?.trim_end_matches('='))
        .ok()?;
    let jwt: Value = serde_json::from_slice(&bytes).ok()?;
    let subject = jwt["sub"].as_str()?;
    let user = subject.split('|').filter(|s| !s.is_empty()).next_back()?;
    if user.is_empty()
        || !user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return None;
    }
    let mut request = nora_ai::HttpRequest::new(
        nora_ai::HttpMethod::Post,
        "https://cursor.com/api/dashboard/get-sand-usage-status",
    )
    .with_header("content-type", "application/json")
    .with_header("origin", "https://cursor.com")
    .with_header(
        "cookie",
        format!(
            "WorkosCursorSessionToken={}",
            url::form_urlencoded::byte_serialize(format!("{user}::{token}").as_bytes())
                .collect::<String>()
        ),
    );
    request.body = Some(b"{}".to_vec());
    let data = network::read(network::send(client, request, 5).await.ok()?)
        .await
        .ok()?;
    let paid = data["includedLimitZero"]
        .as_bool()
        .map(|b| !b)
        .unwrap_or(data["hasNonZeroIncludedLimit"] == true);
    let trial = !paid
        && data["sandTrialExpiresAt"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .is_some_and(|d| d.timestamp_millis() > auth::now() as i64);
    if !paid && !trial {
        return None;
    }
    let used = number(&data["usagePercent"])?;
    let mut item = json!({"id":"grok-bot","label":if trial{"Grok Bot (trial)"}else{"Grok Bot"},"type":"quota","usedPercent":used.clamp(0.0,100.0),"value":format!("{used:.0}% used"),"countsForRouting":false});
    if paid {
        if let Some(at) = data["nextResetTimestampUtc"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        {
            item["resetsAt"] = json!(at.to_rfc3339());
            item["windowSeconds"] = json!(604800);
        }
    }
    Some(item)
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
