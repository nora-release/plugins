use chrono::{DateTime, Utc};
use nora_ai::{HttpClient, ResolvedProviderAuth};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};

const MINUTE: i64 = 60_000;
fn at(value: &Value) -> Option<i64> {
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|v| v.timestamp_millis())
}
fn iso(value: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(value)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(super) fn earliest(parsed: &Value, now: i64) -> Option<(String, Option<i64>)> {
    if parsed["totalAvailable"] == 0 {
        return None;
    }
    let mut grants = parsed["grants"]
        .as_array()?
        .iter()
        .filter_map(|grant| {
            if !matches!(
                grant["status"].as_str(),
                Some("available" | "active" | "unredeemed" | "not_redeemed" | "granted")
            ) || !grant["redeemedAt"].is_null()
            {
                return None;
            }
            let id = grant["id"].as_str().filter(|id| {
                !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control)
            })?;
            let expiry = at(&grant["expiresAt"]);
            if expiry.is_some_and(|v| v <= now) || at(&grant["grantedAt"]).is_some_and(|v| v > now)
            {
                return None;
            }
            Some((id.to_string(), expiry))
        })
        .collect::<Vec<_>>();
    grants.sort_by_key(|(_, at)| at.unwrap_or(i64::MAX));
    grants.into_iter().next()
}

pub(super) fn reading(usage: &Value, now: i64) -> PluginResult<Value> {
    if usage["available"] != true || usage["resetCredits"]["available"] != true {
        return Err(PluginError::transport(
            "Reset eligibility could not be verified; no reset was used.",
        ));
    }
    let windows = usage["items"].as_array().ok_or_else(|| {
        PluginError::transport("Usage windows were not provided; no reset was used.")
    })?;
    let windows = windows
        .iter()
        .filter(|window| window["includedAllowance"] == true && window["aside"] != true)
        .collect::<Vec<_>>();
    let used = |window: &&Value| window["usedPercent"].as_f64().filter(|n| n.is_finite());
    let windows_used = windows.iter().any(|w| used(w).is_some_and(|n| n > 0.0));
    let mut result = json!({"available":true,"windowsUsed":windows_used});
    if let Some(count) = usage["resetCredits"]["totalAvailable"].as_u64() {
        result["count"] = json!(count);
    }
    let weekly = windows
        .iter()
        .filter(|w| {
            w["windowSeconds"].as_f64().is_some_and(|s| s >= 86400.0)
                && used(w).is_some_and(|n| n >= 100.0)
                && w["countsForRouting"] != false
        })
        .filter_map(|w| at(&w["resetsAt"]))
        .filter(|at| *at > now)
        .max();
    if let Some(until) = weekly {
        result["weekly"] = json!({"exhausted":true,"cycleEndsAt":iso(until)});
    }
    let Some((id, expiry)) = earliest(&usage["resetCredits"], now) else {
        result["nextReadAt"] = json!(iso(now + 6 * 60 * MINUTE));
        return Ok(result);
    };
    result["offerID"] = json!(id);
    let Some(expiry) = expiry else {
        result["nextReadAt"] = json!(iso(now + 6 * 60 * MINUTE));
        return Ok(result);
    };
    result["nextExpiry"] = json!(iso(expiry));
    let full = windows
        .iter()
        .filter(|w| used(w).is_some_and(|n| n >= 100.0) && w["countsForRouting"] != false)
        .collect::<Vec<_>>();
    let back = if full.iter().any(|w| at(&w["resetsAt"]).is_none()) {
        None
    } else {
        full.iter().filter_map(|w| at(&w["resetsAt"])).max()
    };
    let stopped = usage["routing"]["allowed"] == false || !full.is_empty();
    let lead = expiry.saturating_sub(30 * MINUTE);
    let due = if stopped && back.is_none_or(|back| back >= lead) {
        now
    } else {
        lead.max(now)
    };
    result["expiring"] = json!({"dueAt":iso(due),"creditExpiresAt":iso(expiry)});
    let next = if expiry - now <= 60 * MINUTE {
        now + 5 * MINUTE
    } else {
        (now + 30 * MINUTE).min(expiry - 60 * MINUTE)
    };
    result["nextReadAt"] = json!(iso(next));
    Ok(result)
}

pub async fn prepare(auth: &ResolvedProviderAuth, client: &dyn HttpClient) -> PluginResult<Value> {
    let usage = super::super::fetch(auth, None, client).await.map_err(|_| {
        PluginError::transport("Reset eligibility could not be read; no reset was used.")
    })?;
    reading(&usage, Utc::now().timestamp_millis())
}

#[cfg(test)]
#[path = "../../../test/reset_prepare.test.rs"]
mod tests;
