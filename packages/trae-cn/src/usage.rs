use crate::*;
use sha2::{Digest, Sha256};
pub(crate) fn date(value: &Value) -> Option<u64> {
    if let Some(value) = value.as_str() {
        if let Ok(value) = chrono::DateTime::parse_from_rfc3339(value) {
            return (value.timestamp_millis() > 0).then_some(value.timestamp_millis() as u64);
        }
    }
    let value = number(value).filter(|value| *value > 0.0)?;
    let at = if value < 1e11 { value * 1000.0 } else { value };
    if at > i64::MAX as f64 {
        return None;
    }
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at as i64)?;
    Some(at as u64)
}
fn stamp(at: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at.min(i64::MAX as u64) as i64)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
pub(crate) async fn fetch(
    provider: &Trae,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Value> {
    let path = if provider.site == Site::China {
        "/trae/api/v2/pay/ide_user_ent_usage"
    } else {
        "/trae/api/v1/pay/user_current_entitlement_list"
    };
    let body = if provider.site == Site::China {
        json!({"require_usage":true,"req_source":0})
    } else {
        json!({})
    };
    let (status, value) = post(
        client,
        format!("{}{path}", provider.site.pay(credentials)),
        credentials,
        body,
    )
    .await?;
    check(provider.site, status, &value)?;
    parse(provider.site, &value, credentials)
}
pub(crate) fn parse(site: Site, value: &Value, credentials: &Value) -> PluginResult<Value> {
    let packs = value
        .get("user_entitlement_pack_list")
        .and_then(Value::as_array)
        .filter(|packs| packs.len() <= 4096)
        .ok_or_else(|| transport("Trae returned no valid entitlement packs."))?;
    let (mut limit, mut used, mut until, mut unlimited) = (0.0, 0.0, None, false);
    let mut plan = if value.get("is_credits_billing").and_then(Value::as_bool) == Some(true) {
        "Credits".to_owned()
    } else {
        "Free".to_owned()
    };
    if site == Site::China {
        for pack in packs {
            let base = pack.get("entitlement_base_info").unwrap_or(&Value::Null);
            let Some(amount) = base.pointer("/quota/credits_limit").and_then(number) else {
                continue;
            };
            if amount == -1.0 {
                unlimited = true;
            } else if amount >= 0.0 {
                limit += amount;
            }
            let amount = pack
                .pointer("/usage/credits_amount")
                .and_then(number)
                .unwrap_or(0.0);
            if amount > 0.0 {
                used += amount;
            }
            if let Some(end) = base
                .get("end_time")
                .or_else(|| base.get("expire_time"))
                .or_else(|| pack.get("end_time"))
                .and_then(date)
            {
                until = Some(until.map_or(end, |until: u64| until.min(end)));
            }
        }
    } else {
        let pack = packs
            .iter()
            .find(|pack| {
                pack.get("usage").is_some()
                    || pack.pointer("/entitlement_base_info/quota").is_some()
            })
            .ok_or_else(|| transport("Trae Global returned no billing entitlement pack."))?;
        let quota = pack
            .pointer("/entitlement_base_info/quota")
            .filter(|quota| quota.is_object())
            .ok_or_else(|| transport("Trae Global returned no billing quota."))?;
        limit = quota
            .get("basic_usage_limit")
            .and_then(number)
            .unwrap_or(0.0)
            + quota
                .get("bonus_usage_limit")
                .and_then(number)
                .unwrap_or(0.0);
        used = pack
            .pointer("/usage/basic_usage_amount")
            .and_then(number)
            .unwrap_or(0.0)
            + pack
                .pointer("/usage/bonus_usage_amount")
                .and_then(number)
                .unwrap_or(0.0);
        until = pack
            .pointer("/entitlement_base_info/end_time")
            .and_then(date);
        if !text(pack, "display_desc").is_empty() {
            plan = text(pack, "display_desc").into();
        }
    }
    let mut windows = Vec::new();
    if limit > 0.0 {
        let mut window = json!({"name":if site==Site::China{"Credits"}else{"Dollar Usage"},"used":round((used/limit*100.0).clamp(0.0,100.0)),"amount":round(used),"limit":round(limit),"unit":if site==Site::China{"credits"}else{"usd"}});
        if let Some(until) = until {
            window["resetsAt"] = json!(stamp(until));
        }
        windows.push(window);
    }
    let mut result = json!({"plan":plan,"user":if text(credentials,"accountId").is_empty(){text(credentials,"uid")}else{text(credentials,"accountId")},"windows":windows,"signIn":"kept"});
    if unlimited {
        result["balance"] = json!("unlimited");
    }
    Ok(result)
}
pub(crate) fn ug_device(credentials: &Value) -> String {
    let device = text(credentials, "deviceId");
    if (15..=16).contains(&device.len()) && device.bytes().all(|byte| byte.is_ascii_digit()) {
        return device.into();
    }
    let hash = Sha256::digest(
        format!("trae-cn-device\0{device}\0{}", text(credentials, "uid")).as_bytes(),
    );
    let number = hash[..15]
        .iter()
        .fold(0u128, |value, byte| value * 256 + *byte as u128);
    (1_000_000_000_000_000u128 + number % 9_000_000_000_000_000u128).to_string()
}
async fn growth_headers(credentials: &Value) -> PluginResult<BTreeMap<String, String>> {
    let mut headers=BTreeMap::from([("content-type".into(),"application/json".into()),("authorization".into(),format!("Cloud-IDE-JWT {}",text(credentials,"access"))),("user-agent".into(),"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) TRAE-SOLO-CN/0.1.69 Chrome/120.0.6099.291 Electron/28.2.10 Safari/537.36".into()),("x-device-id".into(),ug_device(credentials)),("x-device-brand".into(),site::BRAND.into()),("x-device-type".into(),"Windows".into()),("x-os-version".into(),"10.0.22631".into()),("x-app-version".into(),"0.1.69".into())]);
    if let Some(device) = desktop::device(credentials).await? {
        let (kind, os) = desktop::machine()?;
        headers.insert("x-device-id".into(), device);
        headers.insert("x-device-type".into(), kind);
        headers.insert("x-os-version".into(), os);
        headers.remove("x-device-brand");
    }
    Ok(headers)
}
pub(crate) async fn checkin(
    provider: &Trae,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Value> {
    checkin_with_status(provider, client, credentials, false).await
}
pub(crate) async fn checkin_status(
    provider: &Trae,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Value> {
    checkin_with_status(provider, client, credentials, true).await
}
async fn checkin_with_status(
    provider: &Trae,
    client: &dyn HttpClient,
    credentials: &Value,
    status_only: bool,
) -> PluginResult<Value> {
    if provider.site != Site::China {
        return Ok(json!({"outcome":"inactive"}));
    }
    let headers = growth_headers(credentials).await?;
    let (status, value) = json_request(
        client,
        HttpRequest {
            method: HttpMethod::Post,
            url: format!(
                "{}/trae/api/v2/ug/checkin_credits/status",
                provider.site.auth()
            ),
            headers: headers.clone(),
            body: Some(json!({"req_source":1}).to_string().into_bytes()),
        },
    )
    .await?;
    check(provider.site, status, &value)?;
    if value["enable"].as_bool().is_none() && value["checked_in"].as_bool().is_none() {
        return Err(transport("Trae returned no valid check-in status."));
    }
    if value.get("checked_in").and_then(Value::as_bool) == Some(true) {
        return Ok(outcome("done", value.get("credits").and_then(number)));
    }
    if value.get("enable").and_then(Value::as_bool) != Some(true) {
        return Ok(outcome("inactive", None));
    }
    if status_only {
        return Ok(json!({"outcome":"ready","retrySafe":true}));
    }
    let (status, claimed) = json_request(
        client,
        HttpRequest {
            method: HttpMethod::Post,
            url: format!(
                "{}/trae/api/v2/ug/checkin_credits/claim",
                provider.site.auth()
            ),
            headers,
            body: Some(json!({"req_source":1}).to_string().into_bytes()),
        },
    )
    .await?;
    let (code, _) = error_of(&claimed);
    if ["9095", "9074"].contains(&code.as_str()) {
        return Ok(outcome(
            if code == "9074" {
                "own-app"
            } else {
                "ineligible"
            },
            None,
        ));
    }
    check(provider.site, status, &claimed)?;
    if !claimed.is_object()
        || claimed.get("credits").and_then(number).is_none()
            && claimed.get("extra_credits").and_then(number).is_none()
            && claimed["checked_in"] != true
    {
        return Err(transport("Trae did not confirm the daily credit claim."));
    }
    let credits = claimed.get("credits").and_then(number).unwrap_or(0.0)
        + claimed.get("extra_credits").and_then(number).unwrap_or(0.0);
    let fallback = value.get("credits").and_then(number).unwrap_or(0.0)
        + value.get("extra_credits").and_then(number).unwrap_or(0.0);
    Ok(outcome(
        "claimed",
        Some(if credits > 0.0 { credits } else { fallback }),
    ))
}
fn outcome(outcome: &str, credit: Option<f64>) -> Value {
    let mut result = json!({"outcome":outcome});
    if let Some(credit) = credit.filter(|credit| *credit > 0.0) {
        result["credit"] = json!(credit);
    }
    result
}
#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
