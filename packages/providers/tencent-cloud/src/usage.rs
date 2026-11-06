use super::signing;
use chrono::{DateTime, Datelike, Duration as DateDuration, FixedOffset, NaiveDate, TimeZone, Utc};
use futures::{stream, StreamExt};
use nora_ai::{HttpClient, HttpMethod, HttpRequest, ResolvedProviderAuth};
use nora_provider::usage::parsing::{number, result, unavailable};
use nora_provider_quota::window;
use serde_json::{json, Value};
use std::time::Duration;

const PLAN: &str = "Token Plan Enterprise";
const KEY_HINT: &str = "Check the SecretId and SecretKey in this account's settings (Tencent Cloud console → Access management → API keys), and that the key may read TokenHub (CAM).";

pub(crate) fn region(endpoint: Option<&str>) -> Option<&'static str> {
    let url = url::Url::parse(endpoint?).ok()?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || !format!("{}/", url.path().trim_end_matches('/')).starts_with("/plan/")
    {
        return None;
    }
    match url.host_str()? {
        "tokenhub.tencentmaas.com" => Some("ap-guangzhou"),
        "tokenhub-intl.tencentmaas.com" => Some("ap-singapore"),
        _ => None,
    }
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
    let Some(region) = region(auth.base_url.as_deref()) else {
        return Ok(unavailable("This endpoint does not offer a Token Plan Enterprise allowance API. Personal plans and pay-as-you-go usage are available in the Tencent Cloud console."));
    };
    let (Some(access), Some(secret)) = (
        metadata(auth, "accessKeyID"),
        metadata(auth, "secretAccessKey"),
    ) else {
        return Ok(unavailable(&format!(
            "Token Plan Enterprise allowance needs this cloud account's access key. {KEY_HINT}"
        )));
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let list = call(
        client,
        access,
        secret,
        region,
        "DescribeTokenPlanList",
        json!({"Offset":0,"Limit":100}),
        deadline,
    )
    .await?;
    let plans = list
        .pointer("/Response/TokenPlanSet")
        .and_then(Value::as_array)
        .ok_or("Tencent Cloud did not return a readable Token Plan Enterprise list.")?;
    if plans.len() > 100 {
        return Err("Tencent Cloud returned an oversized plan list.".into());
    }
    let plans = plans
        .iter()
        .filter(|plan| {
            plan.get("TeamId")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.trim().is_empty() && id.len() <= 512)
        })
        .cloned()
        .collect::<Vec<_>>();
    if plans.is_empty() {
        return Err(format!("This Tencent Cloud account has no Token Plan Enterprise in {region} that its API key can see. A sub-user sees only the plans it made."));
    }
    let multiple = plans.len() > 1;
    let now = Utc::now();
    let mut read = stream::iter(plans.into_iter().enumerate().map(
        |(index, mut plan)| async move {
            let team = plan
                .get("TeamId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if let Ok(detail) = call(
                client,
                access,
                secret,
                region,
                "DescribeTokenPlan",
                json!({"TeamId":team}),
                deadline,
            )
            .await
            {
                if let Some(detail) = detail.get("Response").filter(|value| value.is_object()) {
                    for key in ["PackageInfo", "TokenSummary"] {
                        if let Some(value) = detail.get(key).filter(|value| value.is_object()) {
                            plan[key] = value.clone();
                        }
                    }
                    if let Some(status) = detail
                        .get("Status")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                    {
                        plan["Status"] = json!(status);
                        plan["StopReason"] = json!(detail
                            .get("StopReason")
                            .and_then(Value::as_str)
                            .unwrap_or(""));
                    }
                    if let Some(value) = detail
                        .get("ProductType")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                    {
                        plan["ProductType"] = json!(value);
                    }
                }
            }
            let name = plan
                .get("Name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(&team);
            let status = status(&plan);
            let label = if multiple {
                format!("{name} · Month{status}")
            } else {
                format!("Month{status}")
            };
            let item = parse(&plan, &team, &label, now);
            (index, item, name.to_owned(), status)
        },
    ))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    read.sort_by_key(|(index, ..)| *index);
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut readable = false;
    let mut plan_label = PLAN.to_owned();
    for (index, item, name, status) in read {
        if !multiple {
            plan_label.push_str(&status);
        }
        if let Some(item) = item {
            readable = true;
            items.push(item);
        } else {
            let message = format!("Tencent Cloud told no quota for {name}{status}.");
            warnings.push(message.clone());
            items.push(json!({"id":format!("unreadable-team-{index}"),"label":format!("{name} · Month{status}"),"value":message,"countsForRouting":false}));
        }
    }
    if !readable {
        return Err(warnings.join(" "));
    }
    Ok(result(
        &json!({"account":{"plan":plan_label}}),
        items,
        vec![],
    ))
}
fn status(plan: &Value) -> String {
    let status = plan
        .get("Status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if status.is_empty() || status.eq_ignore_ascii_case("enable") {
        return String::new();
    }
    let reason = plan
        .get("StopReason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let reason = if reason.is_empty()
        || reason.eq_ignore_ascii_case("normal")
        || reason.eq_ignore_ascii_case("none")
    {
        String::new()
    } else {
        format!(", {reason}")
    };
    format!(" · {status}{reason}")
}
async fn call(
    client: &dyn HttpClient,
    access: &str,
    secret: &str,
    region: &str,
    action: &str,
    params: Value,
    deadline: tokio::time::Instant,
) -> Result<Value, String> {
    let body = params.to_string().into_bytes();
    let now = Utc::now();
    let auth = signing::sign(
        access,
        secret,
        "tokenhub",
        signing::HOST,
        action,
        &body,
        now,
    );
    let request = HttpRequest::new(HttpMethod::Post, format!("https://{}/", signing::HOST))
        .with_header("content-type", signing::CONTENT_TYPE)
        .with_header("x-tc-action", action)
        .with_header("x-tc-version", signing::VERSION)
        .with_header("x-tc-region", region)
        .with_header("x-tc-timestamp", now.timestamp().to_string())
        .with_header("authorization", auth)
        .with_body(body);
    tokio::time::timeout_at(deadline, async {
        let response = client
            .stream(request)
            .await
            .map_err(|_| "The Tencent Cloud allowance service could not be reached.".to_owned())?;
        let status = response.status;
        let text = nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024)
            .await
            .map_err(|_| {
                "Tencent Cloud returned an incomplete or oversized allowance response.".to_owned()
            })?;
        let value: Value = serde_json::from_str(&text).map_err(|_| {
            if status == 401 || status == 403 {
                format!("Tencent Cloud {action} refused the cloud key (HTTP {status}). {KEY_HINT}")
            } else {
                format!("Tencent Cloud {action} returned an unreadable response (HTTP {status}).")
            }
        })?;
        let error = value.pointer("/Response/Error");
        let code = error
            .and_then(|error| error.get("Code"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let message = error
            .and_then(|error| error.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !code.is_empty() || !message.is_empty() || !(200..300).contains(&status) {
            let lower = code.to_ascii_lowercase();
            let hint = if status == 401
                || status == 403
                || lower.starts_with("authfailure")
                || lower.contains("unauthorized")
            {
                KEY_HINT
            } else {
                ""
            };
            return Err(format!(
                "Tencent Cloud {action}: {code} {message} (HTTP {status}). {hint}"
            ));
        }
        Ok(value)
    })
    .await
    .map_err(|_| "The Tencent Cloud allowance request timed out.".to_owned())?
}
fn date(value: Option<&Value>) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(value?.as_str()?.trim()).ok()
}
// Go's AddDate normalizes a month's overflow (January 31 + one month is in
// March); retaining this matters for Tencent's package-cycle fallback.
fn month(start: DateTime<FixedOffset>, count: u32) -> Option<DateTime<FixedOffset>> {
    let total = i64::from(start.year()) * 12 + i64::from(start.month0()) + i64::from(count);
    let year = i32::try_from(total.div_euclid(12)).ok()?;
    let month = u32::try_from(total.rem_euclid(12) + 1).ok()?;
    let date = NaiveDate::from_ymd_opt(year, month, 1)?
        .checked_add_signed(DateDuration::days(i64::from(start.day()) - 1))?;
    start
        .offset()
        .from_local_datetime(&date.and_time(start.time()))
        .single()
}
pub(crate) fn parse(plan: &Value, id: &str, label: &str, now: DateTime<Utc>) -> Option<Value> {
    let package = plan.get("PackageInfo")?;
    let limit = package
        .get("CycleQuota")
        .and_then(number)
        .filter(|n| *n > 0.0)
        .or_else(|| {
            package
                .get("TotalQuota")
                .and_then(number)
                .filter(|n| *n > 0.0)
        })?;
    let used = package.get("TotalUsed").and_then(number)?;
    let token_plan = plan
        .get("ProductType")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("enterprise-auto"));
    let mut span = 30.0 * 86400.0;
    let mut reset = date(plan.pointer("/TokenSummary/CycleEndTime"));
    if let (Some(start), Some(end)) = (date(plan.pointer("/TokenSummary/CycleStartTime")), reset) {
        if end > start {
            span = (end - start).num_seconds() as f64;
        }
    }
    if reset.is_none() {
        let cycle_unit = package
            .get("CycleUnit")
            .and_then(Value::as_str)
            .unwrap_or("");
        if cycle_unit.is_empty() || cycle_unit.eq_ignore_ascii_case("month") {
            if let Some(start) = date(package.get("StartTime")) {
                let mut count = package
                    .get("CurrentCycle")
                    .and_then(number)
                    .unwrap_or(1.0)
                    .clamp(1.0, 1200.0) as u32;
                if let Some(mut end) = month(start, count) {
                    while end.with_timezone(&Utc) <= now && count < 1200 {
                        count += 1;
                        end = month(start, count)?;
                    }
                    if let Some(expiry) =
                        date(package.get("ExpireTime")).filter(|expiry| *expiry < end)
                    {
                        end = expiry;
                    }
                    reset = Some(end);
                }
            }
        }
    }
    let reset = reset.map(|date| json!(date.to_rfc3339()));
    window(
        &format!("team-{id}"),
        label,
        &json!({"used":used,"limit":limit}),
        reset.as_ref(),
        Some(span),
        true,
        Some(if token_plan { "tokens" } else { "credits" }),
    )
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
