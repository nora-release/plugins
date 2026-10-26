use nora_ai::{HttpClient, ResolvedProviderAuth};
use nora_provider::usage::parsing::{number, timestamp};
use nora_provider_quota::{fetch, unsupported_origin};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time::Instant;

// The router's shortest usage deadline is 12 seconds, including host overhead.
const PERSONAL_USAGE_BUDGET: Duration = Duration::from_secs(10);

pub async fn usage(
    auth: &ResolvedProviderAuth,
    host: &str,
    endpoint: &str,
    client: &dyn HttpClient,
) -> Result<Value, String> {
    let started = Instant::now();
    let Some(payload) = fetch(auth, host, endpoint, false, client)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(unsupported_origin());
    };
    let mut result = super::usage::parse(&payload)?;
    let root = format!("https://{host}");
    let reset_url = format!("{root}/api/biz/customer-package-reset/list?targetType=PERSONAL");
    let term_url = format!("{root}/api/biz/subscription/list");
    let timeout = PERSONAL_USAGE_BUDGET.saturating_sub(started.elapsed());
    if timeout.is_zero() {
        return Ok(result);
    }
    let (resets, subscriptions) = tokio::join!(
        tokio::time::timeout(timeout, fetch(auth, host, &reset_url, false, client)),
        tokio::time::timeout(timeout, fetch(auth, host, &term_url, false, client)),
    );
    // These optional details must not discard an otherwise readable allowance.
    if let Ok(Ok(Some(payload))) = resets {
        if let Some(credits) = data(&payload).and_then(reset_credits) {
            result["resetCredits"] = credits;
        }
    }
    if let Ok(Ok(Some(payload))) = subscriptions {
        if let Some(term) = data(&payload).and_then(plan_term) {
            result.as_object_mut().unwrap().extend(term);
        }
    }
    Ok(result)
}

fn data(payload: &Value) -> Option<&Value> {
    if payload["success"] == false
        || payload
            .get("code")
            .and_then(number)
            .is_some_and(|code| code != 200.0 && code != 0.0)
    {
        return None;
    }
    payload.get("data").filter(|data| !data.is_null())
}

fn reset_credits(data: &Value) -> Option<Value> {
    let mut grants = Vec::new();
    for (key, window) in [("fiveHourResets", "fiveHour"), ("weekResets", "weekly")] {
        let resets = data.get(key)?.as_array()?;
        for (index, reset) in resets.iter().enumerate() {
            if !reset.get("available")?.as_bool()? {
                continue;
            }
            let mut grant =
                json!({"id":format!("{window}:{index}"),"window":window,"status":"available"});
            if let Some(expiry) = when(&reset["expireTime"]) {
                grant["expiresAt"] = json!(expiry);
            }
            grants.push(grant);
        }
    }
    grants.sort_by_key(|grant| {
        grant["expiresAt"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| "~".into())
    });
    let count = grants.len();
    grants.truncate(100);
    Some(json!({"available":true,"totalAvailable":count,"grants":grants}))
}

fn plan_term(subscriptions: &Value) -> Option<serde_json::Map<String, Value>> {
    let subscription = subscriptions.as_array()?.iter().find(|subscription| {
        subscription["status"]
            .as_str()
            .is_some_and(|status| status.eq_ignore_ascii_case("VALID"))
    })?;
    let auto = subscription["autoRenew"] == true || subscription["autoRenew"] == 1;
    let until = when(&subscription["nextRenewTime"]).or_else(|| {
        if auto {
            return None;
        }
        static DATES: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let pattern = DATES.get_or_init(|| {
            regex::Regex::new(r"\d{4}-\d{2}-\d{2}(?:[ T]\d{2}:\d{2}:\d{2})?").unwrap()
        });
        let last = pattern.find_iter(subscription["valid"].as_str()?).last()?;
        when(&json!(last.as_str()))
    })?;
    Some(
        json!({"until":until,"renew":if auto {"auto"}else{"off"}})
            .as_object()
            .unwrap()
            .clone(),
    )
}

// The vendor's timezone-free plan dates are in Beijing, including date-only values.
fn when(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        let text = text.trim().replace(' ', "T");
        if text.len() == 10 || text.len() == 19 {
            let text = if text.len() == 10 {
                format!("{text}T00:00:00+08:00")
            } else {
                format!("{text}+08:00")
            };
            if let Ok(date) = chrono::DateTime::parse_from_rfc3339(&text) {
                return Some(date.to_utc().to_rfc3339());
            }
        }
    }
    timestamp(value).and_then(|text| {
        chrono::DateTime::parse_from_rfc3339(&text)
            .ok()
            .map(|date| date.to_utc().to_rfc3339())
    })
}

#[cfg(test)]
#[path = "../test/personal.test.rs"]
mod tests;
