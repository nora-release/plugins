use super::signing;
use chrono::Utc;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, ResolvedProviderAuth};
use nora_provider::usage::parsing::{number, result, unavailable};
use nora_provider_quota::window;
use serde_json::{json, Value};
use std::time::Duration;

const KEY_HINT: &str = "Check the AccessKey ID and Secret in this account's settings, and that the access key may query Ark (IAM).";
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Plan {
    pub action: &'static str,
    pub label: &'static str,
    pub region: String,
}
pub(crate) fn plan(endpoint: Option<&str>) -> Option<Plan> {
    let url = url::Url::parse(endpoint?).ok()?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    let labels = host.split('.').collect::<Vec<_>>();
    if labels.len() != 4
        || labels[0] != "ark"
        || labels[1].is_empty()
        || labels[2..] != ["volces", "com"]
    {
        return None;
    }
    let path = format!("{}/", url.path().trim_end_matches('/'));
    let (action, label) = if path.starts_with("/api/coding/") {
        ("GetCodingPlanUsage", "Coding Plan")
    } else if path.starts_with("/api/plan/") {
        ("GetAFPUsage", "Agent Plan")
    } else {
        return None;
    };
    Some(Plan {
        action,
        label,
        region: labels[1].into(),
    })
}
fn metadata<'a>(auth: &'a ResolvedProviderAuth, key: &str) -> Option<&'a str> {
    auth.metadata
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
}
pub(crate) async fn fetch(
    auth: &ResolvedProviderAuth,
    client: &dyn HttpClient,
) -> Result<Value, String> {
    let Some(plan) = plan(auth.base_url.as_deref()) else {
        return Ok(unavailable(
            "This endpoint does not offer an Ark Coding Plan or Agent Plan allowance API.",
        ));
    };
    let (Some(access), Some(secret)) = (
        metadata(auth, "accessKeyID"),
        metadata(auth, "secretAccessKey"),
    ) else {
        return Ok(unavailable(&format!(
            "{} allowance needs this cloud account's access key. {KEY_HINT}",
            plan.label
        )));
    };
    let query = signing::query(plan.action);
    let body = b"{}";
    let (authorization, at, sha) =
        signing::sign(access, secret, &plan.region, &query, body, Utc::now());
    let request = HttpRequest::new(
        HttpMethod::Post,
        format!("https://{}/?{query}", signing::HOST),
    )
    .with_header("content-type", signing::CONTENT_TYPE)
    .with_header("accept", "application/json")
    .with_header("x-date", at)
    .with_header("x-content-sha256", sha)
    .with_header("authorization", authorization)
    .with_body(body.to_vec());
    let value = tokio::time::timeout(Duration::from_secs(20), async {
        let response = client
            .stream(request)
            .await
            .map_err(|_| "The Volcengine allowance service could not be reached.".to_owned())?;
        let status = response.status;
        let bytes = nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024)
            .await
            .map_err(|_| {
                "Volcengine returned an incomplete or oversized allowance response.".to_owned()
            })?;
        let value: Value = serde_json::from_str(&bytes).map_err(|_| {
            if status == 401 || status == 403 {
                format!(
                    "Volcengine {} refused the cloud key (HTTP {status}). {KEY_HINT}",
                    plan.action
                )
            } else {
                format!(
                    "Volcengine {} returned an unreadable response (HTTP {status}).",
                    plan.action
                )
            }
        })?;
        let error = value.pointer("/ResponseMetadata/Error");
        let code = error
            .and_then(|error| error.get("Code"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let message = error
            .and_then(|error| error.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !code.is_empty() || !message.is_empty() || !(200..300).contains(&status) {
            let hint = if status == 401 || status == 403 || refuses_key(code) {
                KEY_HINT
            } else {
                ""
            };
            return Err(format!(
                "Volcengine {}: {code} {message} (HTTP {status}). {hint}",
                plan.action
            ));
        }
        Ok(value)
    })
    .await
    .map_err(|_| "The Volcengine allowance request timed out.".to_owned())??;
    parse(&value, &plan)
}
fn refuses_key(code: &str) -> bool {
    let code = code.to_ascii_lowercase();
    [
        "accesskey",
        "signature",
        "accessdenied",
        "unauthorized",
        "forbidden",
        "credential",
        "authfailure",
    ]
    .iter()
    .any(|word| code.contains(word))
}
fn named(level: &str) -> Option<(&'static str, &'static str, f64)> {
    match level.trim().to_ascii_lowercase().as_str() {
        "session" => Some(("five-hour", "5 hours", 5.0 * 3600.0)),
        "weekly" => Some(("weekly", "Weekly", 7.0 * 86400.0)),
        "monthly" => Some(("monthly", "Month", 30.0 * 86400.0)),
        _ => None,
    }
}
pub(crate) fn parse(payload: &Value, plan: &Plan) -> Result<Value, String> {
    let raw = payload
        .get("Result")
        .ok_or("Volcengine did not return a plan allowance result.")?;
    let mut items = Vec::new();
    let mut label = plan.label.to_owned();
    if plan.action == "GetCodingPlanUsage" {
        for entry in raw
            .get("QuotaUsage")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some((id, label, span)) =
                entry.get("Level").and_then(Value::as_str).and_then(named)
            else {
                continue;
            };
            let Some(percent) = entry.get("Percent").and_then(number) else {
                continue;
            };
            if let Some(item) = window(
                id,
                label,
                &json!({"used_percent":percent}),
                entry.get("ResetTimestamp"),
                Some(span),
                true,
                None,
            ) {
                items.push(item);
            }
        }
    } else {
        if let Some(tier) = raw
            .get("Tier")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|tier| !tier.is_empty())
        {
            label.push(' ');
            label.push_str(tier);
        }
        for (key, level) in [
            ("AFPFiveHour", "session"),
            ("AFPWeekly", "weekly"),
            ("AFPMonthly", "monthly"),
        ] {
            let Some(bucket) = raw.get(key).filter(|bucket| bucket.is_object()) else {
                continue;
            };
            let (id, label, span) = named(level).unwrap();
            let values = if let Some(percent) = bucket.get("Percent").and_then(number) {
                json!({"used_percent":percent})
            } else {
                let used = bucket.get("Used").and_then(number);
                let total = bucket
                    .get("Quota")
                    .and_then(number)
                    .or_else(|| bucket.get("Total").and_then(number));
                let (Some(used), Some(total)) = (used, total.filter(|total| *total > 0.0)) else {
                    continue;
                };
                json!({"used":used,"limit":total})
            };
            let reset = bucket
                .get("ResetTime")
                .filter(|value| nora_provider::usage::parsing::timestamp(value).is_some())
                .or_else(|| bucket.get("ResetTimestamp"));
            if let Some(item) = window(id, label, &values, reset, Some(span), true, None) {
                items.push(item);
            }
        }
    }
    if items.is_empty() {
        return Err(format!(
            "This Volcengine account has no {}, or Ark did not return readable allowance totals.",
            plan.label
        ));
    }
    Ok(result(&json!({"account":{"plan":label}}), items, vec![]))
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
