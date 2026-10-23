use crate::{
    cache::{self, Begin, Cache, Reading},
    config::Account,
    transport,
};
use nora_ai::{HttpClient, HttpMethod};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::path::Path;
const CARDS: &str = "/v1/magpie/quotas/cards";
const LIMIT: &str = "/v1/magpie/limit";
fn valid(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}
fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}
fn reset(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok())
}
async fn reading(
    cache: &Cache,
    client: &dyn HttpClient,
    account: &Account,
    cards: bool,
    force: bool,
) -> PluginResult<Reading> {
    let revision = match cache.begin(cards, cache::now(), force)? {
        Begin::Cached(value) => return Ok(value),
        Begin::Read(revision) => revision,
    };
    let result = transport::read(
        client,
        account,
        HttpMethod::Get,
        if cards { CARDS } else { LIMIT },
        false,
    )
    .await
    .and_then(|value| {
        let okay = if cards {
            value
                .get("data")
                .is_none_or(|cards| cards.as_array().is_some_and(|cards| cards.len() <= 1000))
        } else {
            value.get("limited").is_some_and(Value::is_boolean)
        };
        if okay {
            Ok(value)
        } else {
            Err(transport::RemoteError {
                status: 200,
                held: false,
                message: "The remote gateway returned unreadable allowance information.".into(),
            })
        }
    });
    cache.finish(cards, revision, cache::now(), result)
}
pub(crate) async fn read(
    directory: &Path,
    client: &dyn HttpClient,
    account: &Account,
    local_account: &str,
    force: bool,
) -> PluginResult<Value> {
    let cache = Cache::new(directory, &account.fingerprint())?;
    let (cards, key) = futures::join!(
        reading(&cache, client, account, true, force),
        reading(&cache, client, account, false, force)
    );
    Ok(render(account, local_account, &cards?, &key?))
}
pub(crate) async fn refresh(
    directory: &Path,
    client: &dyn HttpClient,
    account: &Account,
    local_account: &str,
    params: &Value,
) -> PluginResult<Value> {
    let provider = params
        .get("provider")
        .and_then(Value::as_str)
        .filter(|provider| !provider.is_empty());
    let mut path = "/v1/magpie/quotas/refresh".to_owned();
    if let Some(provider) = provider {
        if !valid(provider, 512) {
            return Err(PluginError::invalid("Choose a remote allowance card."));
        }
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("provider", provider);
        if let Some(user) = params
            .get("user")
            .and_then(Value::as_str)
            .filter(|user| !user.is_empty())
        {
            if !valid(user, 512) {
                return Err(PluginError::invalid("Choose a remote allowance account."));
            }
            query.append_pair("user", user);
        }
        path.push('?');
        path.push_str(&query.finish());
    }
    let cache = Cache::new(directory, &account.fingerprint())?;
    if let Err(error) = transport::read(client, account, HttpMethod::Post, &path, false).await {
        if let Begin::Read(revision) = cache.begin(true, cache::now(), true)? {
            cache.finish(true, revision, cache::now(), Err(error))?;
        }
    }
    read(directory, client, account, local_account, true).await
}
pub(crate) async fn history(
    client: &dyn HttpClient,
    account: &Account,
    local_account: &str,
    days: &str,
) -> PluginResult<Value> {
    let days = days
        .parse::<u16>()
        .ok()
        .filter(|days| (1..=366).contains(days))
        .ok_or_else(|| PluginError::invalid("Choose between 1 and 366 days of history."))?;
    let mut answer = transport::read(
        client,
        account,
        HttpMethod::Get,
        &format!("/v1/magpie/quotas/history?days={days}"),
        false,
    )
    .await
    .map_err(transport::RemoteError::plugin)?;
    let lines = answer
        .get_mut("data")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            PluginError::transport("The remote gateway returned unreadable allowance history.")
        })?;
    if lines.len() > 10_000 {
        return Err(PluginError::transport(
            "The remote gateway returned too much allowance history.",
        ));
    }
    for line in lines {
        if let Some(provider) = line
            .get("provider")
            .and_then(Value::as_str)
            .filter(|provider| valid(provider, 512))
        {
            line["provider"] = json!(format!("remote-magpie/{local_account}/{provider}"));
        }
    }
    Ok(answer)
}
fn remote_cards(
    account: &Account,
    local_account: &str,
    reading: &Reading,
    items: &mut Vec<Value>,
) -> Vec<Value> {
    if reading.held {
        return Vec::new();
    }
    let mut out = Vec::new();
    for card in reading
        .value
        .as_ref()
        .and_then(|value| value.get("data"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(1000)
    {
        if card
            .get("from")
            .and_then(Value::as_str)
            .is_some_and(|from| !from.is_empty())
        {
            continue;
        }
        let Some(provider) = card
            .get("provider")
            .and_then(Value::as_str)
            .filter(|provider| valid(provider, 512))
        else {
            continue;
        };
        let name = card
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| valid(name, 512))
            .unwrap_or(provider);
        let user = card
            .get("user")
            .and_then(Value::as_str)
            .filter(|user| valid(user, 512))
            .unwrap_or("");
        let full = format!("{name} · {}", account.name);
        let mut clean = json!({"provider":format!("remote-magpie/{local_account}/{provider}"),"originProvider":provider,"name":full,"from":account.name,"kind":card.get("kind").and_then(Value::as_str).filter(|kind|matches!(*kind,"plan"|"balance"|"subscription")).unwrap_or("subscription"),"user":user,"windows":[]});
        for key in [
            "icon", "plan", "balance", "until", "renew", "error", "asOf", "readAt", "alias", "seat",
        ] {
            if let Some(value) = card
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| valid(value, 4096))
            {
                clean[key] = json!(value);
            }
        }
        let mut windows = Vec::new();
        for (index, window) in card
            .get("windows")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(256)
            .enumerate()
        {
            let label = window
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| valid(name, 512))
                .unwrap_or("Allowance");
            let mut safe = json!({"name":label});
            for key in ["used", "amount", "limit", "runsOut"] {
                if let Some(value) = window.get(key).and_then(number) {
                    safe[key] = json!(value);
                }
            }
            for key in ["resetsAt", "runsOutAt"] {
                if let Some(value) = window.get(key).and_then(reset) {
                    safe[key] = json!(value);
                }
            }
            for key in ["display", "unit", "family", "pool"] {
                if let Some(value) = window
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|value| valid(value, 512))
                {
                    safe[key] = json!(value);
                }
            }
            if window.get("unlimited") == Some(&Value::Bool(true)) {
                safe["unlimited"] = json!(true);
            }
            let mut item = json!({"id":format!("remote-card/{provider}/{user}/{index}"),"label":format!("{full} · {label}"),"countsForRouting":false,"includedAllowance":false,"source":"remote","remoteProvider":provider,"remoteUser":user});
            if let Some(used) = window.get("used").and_then(number) {
                item["usedPercent"] = json!(used.clamp(0.0, 100.0));
            }
            if let Some(reset) = window.get("resetsAt").and_then(reset) {
                item["resetsAt"] = json!(reset);
            }
            if let Some(value) = window
                .get("display")
                .and_then(Value::as_str)
                .filter(|value| valid(value, 512))
            {
                item["value"] = json!(value);
            }
            if window.get("unlimited") == Some(&Value::Bool(true)) {
                item["value"] = json!("Unlimited");
                item.as_object_mut().unwrap().remove("usedPercent");
            }
            items.push(item);
            windows.push(safe);
        }
        if let Some(balance) = card
            .get("balance")
            .and_then(Value::as_str)
            .filter(|balance| valid(balance, 512))
        {
            items.push(json!({"id":format!("remote-balance/{provider}/{user}"),"label":full,"value":balance,"countsForRouting":false,"includedAllowance":false,"source":"remote","remoteProvider":provider,"remoteUser":user}));
        }
        clean["windows"] = json!(windows);
        if !reading.error.is_empty() && reading.read_at > 0 {
            clean["asOf"] = json!(reading.read_at);
        }
        out.push(clean);
    }
    if out.is_empty() {
        let message = if reading.error.is_empty() {
            "Nothing has been read on this gateway yet. Refresh its allowances there."
        } else {
            &reading.error
        };
        out.push(json!({"provider":format!("remote-magpie/{local_account}"),"name":account.name,"from":account.name,"kind":"subscription","windows":[],"error":message}));
        items.push(json!({"id":"remote-card/unavailable","label":format!("{} · Remote accounts",account.name),"value":message,"countsForRouting":false,"includedAllowance":false,"source":"remote"}));
    }
    out
}
fn period(value: &Value) -> String {
    match value.get("period").and_then(Value::as_str) {
        Some("day") => "today",
        Some("week") => "this week",
        Some("month") => "this month",
        _ => "this cycle",
    }
    .into()
}
fn render(account: &Account, local_account: &str, cards: &Reading, key: &Reading) -> Value {
    let mut items = Vec::new();
    let remote = remote_cards(account, local_account, cards, &mut items);
    let mut routing = json!({});
    let mut key_card = Value::Null;
    if let Some(answer) = &key.value {
        let limited = answer
            .get("limited")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        key_card = json!({"name":answer.get("key").and_then(Value::as_str).filter(|name|valid(name,512)).unwrap_or("Gateway key"),"limited":limited,"source":"remote","readAt":key.read_at});
        if !limited {
            items.insert(0,json!({"id":"remote-key/unlimited","label":"Gateway key","value":"Unlimited","countsForRouting":false}));
            routing["allowed"] = json!(true);
        } else {
            let all = answer
                .get("limits")
                .and_then(Value::as_array)
                .filter(|limits| !limits.is_empty())
                .cloned()
                .unwrap_or_else(|| {
                    answer
                        .get("limit")
                        .filter(|limit| limit.is_object())
                        .cloned()
                        .into_iter()
                        .collect()
                });
            let mut spent = false;
            let mut resets = Vec::new();
            let mut unknown_reset = false;
            for (index, limit) in all.iter().take(256).enumerate() {
                if limit.get("spent") == Some(&Value::Bool(true)) {
                    spent = true;
                    if let Some(value) = limit.get("reset").and_then(reset) {
                        resets.push(value.to_owned());
                    } else {
                        unknown_reset = true;
                    }
                }
                for (cap, used, unit, label) in [
                    ("tokenLimit", "tokens", "tokens", "Tokens"),
                    ("costLimit", "cost", "USD", "Spend"),
                ] {
                    let Some(cap) = limit.get(cap).and_then(number).filter(|cap| *cap > 0.0) else {
                        continue;
                    };
                    let Some(used) = limit.get(used).and_then(number) else {
                        continue;
                    };
                    let mut item = json!({"id":format!("remote-key/{index}/{unit}"),"label":format!("Gateway key · {label} {}",period(limit)),"used":used,"limit":cap,"unit":unit,"usedPercent":((used/cap*1000.0).round()/10.0).clamp(0.0,100.0),"countsForRouting":true,"includedAllowance":true});
                    if let Some(value) = limit.get("reset").and_then(reset) {
                        item["resetsAt"] = json!(value);
                    }
                    items.insert(index.min(items.len()), item);
                }
            }
            routing["allowed"] = json!(!spent);
            if spent && !unknown_reset {
                if let Some(last) = resets.into_iter().max_by_key(|value| {
                    chrono::DateTime::parse_from_rfc3339(value)
                        .ok()
                        .map(|date| date.timestamp_millis())
                }) {
                    routing["resetsAt"] = json!(last);
                }
            }
            // Preserve the key's exact remote status beside rendered windows.
            for name in ["limit", "limits"] {
                if let Some(value) = answer.get(name) {
                    key_card[name] = value.clone();
                }
            }
        }
    }
    let errors: [Option<String>; 2] = [
        (!cards.error.is_empty()).then(|| format!("Remote accounts: {}", cards.error)),
        (!key.error.is_empty()).then(|| format!("Gateway key: {}", key.error)),
    ];
    let error = errors.into_iter().flatten().collect::<Vec<_>>().join(" ");
    let mut result = json!({"available":cards.value.is_some()||key.value.is_some(),"items":items,"remoteCards":remote,"gatewayKey":key_card,"routing":routing,"source":"remote","readAt":cards.read_at.max(key.read_at)});
    if !error.is_empty() {
        result["error"] = json!(error);
        result["stale"] = json!(true);
    }
    if cards.held {
        result["remoteAccountsRestricted"] = json!(true);
    }
    result
}
#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
