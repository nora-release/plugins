use super::*;
use chrono::{FixedOffset, NaiveDateTime, TimeZone, Utc};
use std::collections::BTreeSet;

const PAID: &[&str] = &[
    "TCACA_code_002_AkiJS3ZHF5",
    "TCACA_code_005_maRGyrHhw1",
    "TCACA_code_003_FAnt7lcmRT",
    "TCACA_code_023_4xbGhMrE6q",
    "TCACA_code_026_BaESVICNoi",
    "TCACA_code_027_0FCGVA6vSa",
    "TCACA_code_009_0XmEQc2xOf",
    "TCACA_code_038_OhvqZtiPKr",
    "TCACA_code_036_lupO5WgNdG",
];
const DAILY: &str = "TCACA_code_001_PqouKr6QWV";
const FREE: &[&str] = &[
    DAILY,
    "TCACA_code_008_cfWoLwvjU4",
    "TCACA_code_035_ArVxJcGDsm",
    "TCACA_code_006_DbXS0lrypC",
    "TCACA_code_039_KRcQj7wUat",
    "TCACA_code_040_mi9rCYg46x",
    "TCACA_code_007_nzdH5h4Nl0",
    "TCACA_code_028_NtpWi0jzXs",
    "TCACA_code_037_WxOD3MpI2o",
    "TCACA_code_029_6wCGEWquYy",
    "TCACA_code_030_BjSt89qTvr",
];

pub(super) async fn fetch(
    provider: &WorkBuddy,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Value> {
    let summary = meter(
        provider,
        client,
        credentials,
        "/billing/meter/get-user-resource-summary",
        json!({}),
    )
    .await?;
    let mut value = parse(&summary, text(credentials, "plan"))?;
    if value["windows"].as_array().is_some_and(Vec::is_empty) {
        if let Ok(enterprise) = meter(
            provider,
            client,
            credentials,
            "/billing/meter/get-enterprise-user-usage",
            json!({}),
        )
        .await
        {
            if let Ok(Some(window)) = enterprise_window(&enterprise) {
                value["windows"] = json!([window]);
                if text(credentials, "plan").is_empty() {
                    value["plan"] = json!("Enterprise");
                }
            }
        }
    } else if let Ok(packages) = packages(provider, client, credentials).await {
        if let Ok(Some((expires, amount))) = runs_out(&summary, &packages, now()) {
            value["windows"][0]["runsOutAt"] = json!(expires);
            value["windows"][0]["runsOut"] = json!(amount);
        }
    }
    if let Ok(models) = models::fetch(provider, client, credentials).await {
        let free = models
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|model| model.pointer("/info/free").and_then(Value::as_bool) == Some(true))
            .filter_map(|model| model.get("id"))
            .cloned()
            .collect::<Vec<_>>();
        if !free.is_empty() {
            for window in value["windows"].as_array_mut().unwrap() {
                window["notModels"] = json!(free);
            }
        }
    }
    value["signIn"] = json!("kept");
    Ok(value)
}

async fn meter(
    provider: &WorkBuddy,
    client: &dyn HttpClient,
    credentials: &Value,
    path: &str,
    body: Value,
) -> PluginResult<Value> {
    provider
        .call(
            client,
            HttpMethod::Post,
            path,
            request::headers(provider.site, credentials),
            Some(body),
        )
        .await
}

pub(super) fn parse(summary: &Value, plan: &str) -> PluginResult<Value> {
    let mut total = 0.0;
    let mut used = 0.0;
    for package in summary
        .get("Packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        total += capacity(package.get("CycleTotalCapacity"))?;
        used += capacity(package.get("CycleUsedCapacity"))?;
    }
    let windows = if total > 0.0 {
        vec![window(used, total)]
    } else {
        Vec::new()
    };
    Ok(
        json!({"plan":if !plan.is_empty(){plan}else if summary.get("IsPaidUser").and_then(Value::as_bool)==Some(true){"Pro"}else{"Free"},"windows":windows}),
    )
}

fn capacity(value: Option<&Value>) -> PluginResult<f64> {
    let Some(value) = value.filter(|value| !value.is_null() && value.as_str() != Some("")) else {
        return Ok(0.0);
    };
    if value.as_str().is_some_and(|value| value.trim() != value) {
        return Err(invalid("WorkBuddy returned an invalid credit amount."));
    }
    number(value).ok_or_else(|| invalid("WorkBuddy returned an invalid credit amount."))
}
fn compact(number: f64) -> String {
    format!("{number:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}
fn window(used: f64, total: f64) -> Value {
    json!({"name":"Credits","used":100.0*used/total,"display":format!("{} / {}",compact(used),compact(total)),"amount":used,"limit":total,"unit":"credits"})
}

pub(super) fn enterprise_window(value: &Value) -> PluginResult<Option<Value>> {
    let limit = capacity(value.get("limitNum"))?;
    if limit <= 0.0 {
        return Ok(None);
    }
    let mut window = window(capacity(value.get("credit"))?, limit);
    let start = beijing_date(text(value, "cycleStartTime"));
    let reset = beijing_date(text(value, "cycleResetTime"));
    if let Some(reset) = reset {
        window["resetsAt"] = json!(stamp(reset));
    }
    if let Some((reset, start)) = reset.zip(start).filter(|(reset, start)| reset > start) {
        window["span"] = json!((reset - start) / 1000);
    }
    Ok(Some(window))
}

async fn packages(
    provider: &WorkBuddy,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Vec<Value>> {
    let today = Utc::now()
        .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
        .format("%Y-%m-%d")
        .to_string();
    let lists = [
        (
            "/billing/meter/get-user-resource-paid-packages",
            json!({"PackageCodes":PAID,"NeedRenewInfo":true}),
        ),
        (
            "/billing/meter/get-user-resource-free-packages",
            json!({"PackageCodes":FREE,"SlicePeriodStartTime":format!("{today} 00:00:00"),"SlicePeriodEndTime":format!("{today} 23:59:59")}),
        ),
    ];
    let mut packages = Vec::new();
    for (path, extra) in lists {
        let mut seen = 0;
        for page in 1..=10 {
            let mut body = extra.clone();
            body["PageNumber"] = json!(page);
            body["PageSize"] = json!(200);
            body["Status"] = json!([0, 3]);
            let value = meter(provider, client, credentials, path, body).await?;
            let rows = value
                .get("Accounts")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("WorkBuddy returned an invalid credit package list."))?;
            if rows.len() > 200 {
                return Err(invalid("WorkBuddy returned too many credit packages."));
            }
            packages.extend(rows.iter().cloned());
            seen += rows.len();
            if rows.len() < 200
                || value
                    .get("TotalCount")
                    .and_then(Value::as_u64)
                    .is_some_and(|total| seen as u64 >= total)
            {
                break;
            }
        }
    }
    Ok(packages)
}

pub(super) fn runs_out(
    summary: &Value,
    packages: &[Value],
    at: u64,
) -> PluginResult<Option<(String, f64)>> {
    let counted = summary
        .get("Packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|package| text(package, "PackageCode"))
        .collect::<BTreeSet<_>>();
    let mut expires = 0;
    let mut amount = 0.0;
    for package in packages {
        let code = text(package, "PackageCode");
        if !counted.contains(code)
            || code == DAILY
            || package
                .get("CapacityType")
                .is_some_and(|kind| kind == "4" || kind == 4)
        {
            continue;
        }
        let left = capacity(
            package
                .get("CycleCapacityRemainPrecise")
                .filter(|value| !value.is_null())
                .or_else(|| package.get("CycleCapacityRemain")),
        )?;
        let end = package
            .get("DeductionEndTime")
            .and_then(number)
            .filter(|value| *value > 0.0)
            .map(|value| value as u64)
            .or_else(|| {
                ["ExpiredTime", "CycleEndTime"]
                    .into_iter()
                    .find_map(|field| beijing_date(text(package, field)))
            });
        let Some(end) = end.filter(|end| *end > at && left > 0.0) else {
            continue;
        };
        if expires == 0 || end < expires {
            expires = end;
            amount = left;
        } else if end == expires {
            amount += left;
        }
    }
    Ok((expires > 0).then(|| (stamp(expires), amount)))
}

fn beijing_date(value: &str) -> Option<u64> {
    models::date(value).or_else(|| {
        let date = NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
        let stamp = FixedOffset::east_opt(8 * 3600)?
            .from_local_datetime(&date)
            .single()?
            .timestamp_millis();
        (stamp > 0).then_some(stamp as u64)
    })
}
fn stamp(at: u64) -> String {
    chrono::DateTime::<Utc>::from_timestamp_millis(at.min(i64::MAX as u64) as i64)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
