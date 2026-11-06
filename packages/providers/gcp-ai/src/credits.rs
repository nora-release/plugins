use crate::{
    browser::{self, BrowserService},
    cache,
    cdp::Page,
    settings::Settings,
};
use futures::{future::BoxFuture, FutureExt};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};
fn failed(message: impl Into<String>) -> PluginError {
    PluginError::transport(message)
}
fn normalized(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn money(value: &str) -> PluginResult<i64> {
    let re = Regex::new(r"^(?:US\$|\$|USD\s*)([\d,]+)(?:\.(\d{2}))?(?:\s*USD)?$").unwrap();
    let captures = re.captures(value).ok_or_else(|| {
        failed(format!(
            "Unsupported credit amount/currency: {}. This provider requires USD billing.",
            value.chars().take(256).collect::<String>()
        ))
    })?;
    let dollars = captures[1].replace(',', "").parse::<i64>().ok();
    let cents = captures
        .get(2)
        .map(|m| m.as_str().parse::<i64>().unwrap())
        .unwrap_or(0);
    dollars
        .and_then(|v| v.checked_mul(100))
        .and_then(|v| v.checked_add(cents))
        .filter(|v| *v <= 9_007_199_254_740_991)
        .ok_or_else(|| failed("Invalid Google Cloud credit amounts."))
}
fn date(value: &str) -> PluginResult<Option<i64>> {
    if value.is_empty() {
        return Ok(None);
    }
    let time = chrono::DateTime::parse_from_rfc3339(value)
        .or_else(|_| chrono::DateTime::parse_from_rfc2822(value))
        .map(|value| value.timestamp_millis())
        .ok()
        .or_else(|| {
            ["%B %e, %Y", "%b %e, %Y", "%Y-%m-%d"]
                .into_iter()
                .find_map(|format| chrono::NaiveDate::parse_from_str(value, format).ok())
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|time| time.and_utc().timestamp_millis())
        });
    time.map(Some).ok_or_else(|| {
        failed(format!(
            "Unsupported Google Cloud credit date: {}",
            value.chars().take(256).collect::<String>()
        ))
    })
}
fn iso(timestamp: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
pub(crate) fn parse_tables(tables: &Value, now: i64) -> PluginResult<Value> {
    let mut credits = BTreeMap::<String, Value>::new();
    let mut order = Vec::new();
    let mut recognized = false;
    for table in tables.as_array().into_iter().flatten() {
        let headers = table["headers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(normalized)
            .map(|value| value.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let index = |name: &str| headers.iter().position(|header| header == name);
        if [
            "credit name",
            "status",
            "remaining value",
            "original value",
            "credit id",
        ]
        .iter()
        .any(|name| index(name).is_none())
        {
            continue;
        }
        recognized = true;
        for row in table["rows"].as_array().into_iter().flatten() {
            let cell = |name: &str| {
                index(name)
                    .map(|index| normalized(&row[index]))
                    .unwrap_or_default()
            };
            let status = cell("status");
            let kind = status.to_ascii_lowercase();
            if ![
                "available",
                "active",
                "exhausted",
                "used up",
                "used",
                "expired",
                "pending",
            ]
            .contains(&kind.as_str())
            {
                return Err(failed(format!(
                    "Unknown Google Cloud credit status: {}",
                    status.chars().take(128).collect::<String>()
                )));
            }
            let id = cell("credit id");
            if id.is_empty() || credits.contains_key(&id) {
                continue;
            }
            let reported = money(&cell("remaining value"))?;
            let original = money(&cell("original value"))?;
            if reported < 0 || reported > original {
                return Err(failed("Invalid Google Cloud credit amounts."));
            }
            let remaining = if ["exhausted", "used up", "used"].contains(&kind.as_str()) {
                0
            } else {
                reported
            };
            let start = date(&cell("start date"))?;
            let end = date(&cell("end date"))?;
            let eligible = ["available", "active", "exhausted", "used up", "used"]
                .contains(&kind.as_str())
                && start.is_none_or(|value| value <= now)
                && end.is_none_or(|value| value > now);
            let mut credit = json!({"id":id,"name":cell("credit name"),"status":status,"remaining":remaining as f64/100.,"original":original as f64/100.,"scope":cell("usage scope"),"eligible":eligible});
            if let Some(start) = start {
                credit["start"] = json!(iso(start));
            }
            if let Some(end) = end {
                credit["end"] = json!(iso(end));
            }
            order.push(id.clone());
            credits.insert(id, credit);
        }
    }
    if !recognized || credits.is_empty() {
        return Err(failed("Google Cloud Credits table was not found or is empty; the session may be expired, the page may still be loading, or its layout changed."));
    }
    let values = order
        .iter()
        .map(|id| credits.remove(id).unwrap())
        .collect::<Vec<_>>();
    let active = values
        .iter()
        .filter(|credit| credit["eligible"] == true)
        .collect::<Vec<_>>();
    let sum = |key: &str| -> PluginResult<i64> {
        active.iter().try_fold(0i64, |sum, credit| {
            sum.checked_add((credit[key].as_f64().unwrap_or(0.) * 100.).round() as i64)
                .filter(|sum| *sum <= 9_007_199_254_740_991)
                .ok_or_else(|| failed("Invalid Google Cloud credit totals."))
        })
    };
    let remaining = sum("remaining")?;
    let original = sum("original")?;
    let mut result = json!({"currency":"USD","remaining":remaining as f64/100.,"original":original as f64/100.,"used":(original-remaining) as f64/100.,"usedPercent":if original>0{(original-remaining) as f64*100./original as f64}else{100.},"credits":values,"fetchedAt":iso(now),"cached":false});
    let soonest = active
        .iter()
        .filter(|credit| credit["remaining"].as_f64().unwrap_or(0.) > 0.)
        .filter_map(|credit| credit["end"].as_str())
        .min();
    if let Some(end) = soonest {
        result["runsOutAt"] = json!(end);
        result["runsOut"] = json!(
            active
                .iter()
                .filter(|credit| credit["end"] == end)
                .map(|credit| (credit["remaining"].as_f64().unwrap_or(0.) * 100.).round() as i64)
                .sum::<i64>() as f64
                / 100.
        );
    }
    Ok(result)
}
const TABLES: &str = r#"[...document.querySelectorAll("table, [role='grid'], [role='table']")].map(table=>({headers:[...table.querySelectorAll("thead th, [role='columnheader']")].map(cell=>cell.innerText),rows:[...table.querySelectorAll("tbody tr, [role='row']")].map(row=>[...row.querySelectorAll("td, [role='gridcell'], [role='cell']")].map(cell=>cell.innerText)).filter(row=>row.length)}))"#;
async fn billing(page: &Page, settings: &Settings) -> PluginResult<String> {
    if let Some(account) = &settings.billing_account {
        return Ok(account.clone());
    }
    let project = settings.project.as_ref().ok_or_else(|| {
        failed("Select a Google Cloud project before reading its promotional credits.")
    })?;
    let mut url =
        url::Url::parse("https://console.cloud.google.com/billing/linkedaccount").unwrap();
    url.query_pairs_mut()
        .append_pair("project", project)
        .append_pair("hl", "en");
    if let Some(account) = &settings.account {
        url.query_pairs_mut().append_pair("authuser", account);
    }
    page.goto(url.as_str()).await?;
    let _=page.wait(r#"location.hostname==='accounts.google.com'||/billing\/[0-9A-F]{6}-[0-9A-F]{6}-[0-9A-F]{6}/i.test(location.href)||[...document.querySelectorAll('a[href]')].some(a=>/\/billing\/[0-9A-F]{6}-[0-9A-F]{6}-[0-9A-F]{6}/i.test(a.href))"#,Duration::from_secs(45)).await;
    browser::assert_console(page, Duration::from_secs(20)).await?;
    let _=page.wait(r#"/billing\/[0-9A-F]{6}-[0-9A-F]{6}-[0-9A-F]{6}/i.test(location.href)||[...document.querySelectorAll('a[href]')].some(a=>/\/billing\/[0-9A-F]{6}-[0-9A-F]{6}-[0-9A-F]{6}/i.test(a.href))"#,Duration::from_secs(20)).await;
    let urls = page
        .evaluate("[location.href,...[...document.querySelectorAll('a[href]')].map(a=>a.href)]")
        .await?;
    let pattern =
        Regex::new(r"(?i)/billing/([0-9A-F]{6}-[0-9A-F]{6}-[0-9A-F]{6})(?:[/?#]|$)").unwrap();
    let accounts = urls
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .flat_map(|url| {
            pattern
                .captures_iter(url)
                .map(|capture| capture[1].to_owned())
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    if accounts.len() != 1 {
        return Err(failed("Cannot uniquely identify the project's billing account; set VERTEX_BILLING_ACCOUNT explicitly."));
    }
    Ok(accounts.into_iter().next().unwrap())
}
async fn scrape(page: Page, settings: &Settings) -> PluginResult<Value> {
    let billing = billing(&page, settings).await?;
    let mut source = url::Url::parse(&format!(
        "https://console.cloud.google.com/billing/{billing}/credits/all"
    ))
    .unwrap();
    source.query_pairs_mut().append_pair("hl", "en");
    if let Some(account) = &settings.account {
        source.query_pairs_mut().append_pair("authuser", account);
    }
    page.goto(source.as_str()).await?;
    let _=page.wait(r#"location.hostname==='accounts.google.com'||[...document.querySelectorAll("th, [role='columnheader']")].some(cell=>cell.innerText.trim()==='Remaining value')"#,Duration::from_secs(45)).await;
    browser::assert_console(&page, Duration::from_secs(20)).await?;
    page.wait(r#"[...document.querySelectorAll("tbody tr, [role='row']")].some(row=>/\$\s*\d/.test(row.innerText))"#,Duration::from_secs(30)).await.map_err(|_|failed("Google Cloud Credits table was not found or is empty; the session may be expired, the page may still be loading, or its layout changed."))?;
    if let Some(expected) = &settings.account {
        let shown=browser::account(&page,Duration::from_secs(8)).await.map_err(|_|failed("Cloud Console did not show which account the credits belong to; refusing to report another account's balance. Sign in with the Chrome profile again."))?;
        if !shown.eq_ignore_ascii_case(expected) {
            return Err(failed(format!("Cloud Console is signed in as {shown}, not {expected}; sign in with the intended Chrome profile account again.")));
        }
    }
    let mut tables = Vec::new();
    let mut seen = BTreeSet::new();
    for _ in 0..100 {
        let next = page.evaluate(TABLES).await?;
        let signature = serde_json::to_string(&next).unwrap();
        if !seen.insert(signature.clone()) {
            return Err(failed(
                "Credits pagination did not advance; refusing an incomplete balance.",
            ));
        }
        tables.extend(
            next.as_array()
                .ok_or_else(|| failed("Cloud Console returned an invalid credits table."))?
                .iter()
                .cloned(),
        );
        let more=page.evaluate(r#"(()=>{const label=el=>(el.getAttribute('aria-label')??el.innerText??'').trim();const button=[...document.querySelectorAll("button, [role='button']")].find(el=>/^(?:Go to )?Next page$/i.test(label(el)));if(!button||button.disabled||button.getAttribute('aria-disabled')==='true')return false;button.click();return true;})()"#).await?;
        if more != true {
            let mut result = parse_tables(&json!(tables), chrono::Utc::now().timestamp_millis())?;
            result["billingAccount"] = json!(billing);
            result["source"] = json!(source.as_str());
            return Ok(result);
        }
        page.wait(
            &format!(
                "JSON.stringify({TABLES}) !== {}",
                serde_json::to_string(&signature).unwrap()
            ),
            Duration::from_secs(20),
        )
        .await
        .map_err(|_| {
            failed("Credits pagination did not advance; refusing an incomplete balance.")
        })?;
    }
    Err(failed(
        "Credits table exceeded 100 pages; refusing an incomplete balance.",
    ))
}
fn transient(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "err_timed_out",
        "err_connection",
        "err_network",
        "err_internet_disconnected",
        "err_name_not_resolved",
        "err_name_resolution_failed",
        "err_proxy",
        "err_tunnel",
        "err_ssl",
        "err_empty_response",
        "net::err_",
        "timed out after",
        "timeouterror",
        "econnreset",
        "etimedout",
        "enotfound",
        "socket hang up",
    ]
    .iter()
    .any(|part| message.contains(part))
}
fn cached(path: &Path, settings: &Settings, now: i64) -> Option<PluginResult<Value>> {
    if settings.force {
        return None;
    }
    let mut value = cache::read(path)?;
    let failed_at = cache::timestamp(&value, "failedAt");
    let at = failed_at.or_else(|| cache::timestamp(&value, "fetchedAt"))?;
    let age = now - at;
    if age < 0 {
        return None;
    }
    if failed_at.is_some() {
        let error = value["error"].as_str()?;
        let ttl = if transient(error) {
            settings.transient_ttl
        } else {
            settings.failure_ttl
        };
        return ((age as f64) < ttl * 1000.).then(|| Err(failed(error)));
    }
    let future = value
        .get("runsOutAt")
        .is_none_or(|_| cache::timestamp(&value, "runsOutAt").is_some_and(|end| end > now));
    if (age as f64) < settings.credits_ttl * 1000.
        && future
        && value["currency"] == "USD"
        && ["remaining", "original", "usedPercent"]
            .iter()
            .all(|field| value[*field].as_f64().is_some_and(f64::is_finite))
    {
        value["cached"] = json!(true);
        return Some(Ok(value));
    }
    None
}
pub(crate) fn cache_file(settings: &Settings) -> std::path::PathBuf {
    let identity = json!([
        settings.profile,
        settings.project.as_deref().unwrap_or(""),
        settings.billing_account.as_deref().unwrap_or(""),
        settings.account.as_deref().unwrap_or(""),
        settings
            .cloudsdk_config
            .as_ref()
            .map(|path| path.to_string_lossy())
            .unwrap_or_default()
    ]);
    settings
        .cache_root
        .join(format!("credits-{}.json", &cache::digest(&identity)[..24]))
}
async fn read_once(browser: &BrowserService, settings: &Settings) -> PluginResult<Value> {
    settings.validate()?;
    let file = cache_file(settings);
    if let Some(hit) = cached(&file, settings, chrono::Utc::now().timestamp_millis()) {
        return hit;
    }
    let _lock = cache::Lock::take(&file, Duration::from_secs(180)).await?;
    if let Some(hit) = cached(&file, settings, chrono::Utc::now().timestamp_millis()) {
        return hit;
    }
    let result = browser
        .with_page(&settings.profile, |page| scrape(page, settings))
        .await;
    match result {
        Ok(value) => {
            cache::write(&file, &value)?;
            Ok(value)
        }
        Err(error) => {
            cache::write(
                &file,
                &json!({"error":error.message,"failedAt":iso(chrono::Utc::now().timestamp_millis())}),
            )?;
            Err(error)
        }
    }
}

type Shared = futures::future::Shared<BoxFuture<'static, PluginResult<Value>>>;
fn pending() -> &'static tokio::sync::Mutex<BTreeMap<std::path::PathBuf, Shared>> {
    static TASKS: OnceLock<tokio::sync::Mutex<BTreeMap<std::path::PathBuf, Shared>>> =
        OnceLock::new();
    TASKS.get_or_init(Default::default)
}
pub(crate) async fn read(browser: &BrowserService, settings: &Settings) -> PluginResult<Value> {
    settings.validate()?;
    let file = cache_file(settings);
    if let Some(hit) = cached(&file, settings, chrono::Utc::now().timestamp_millis()) {
        return hit;
    }
    let mut tasks = pending().lock().await;
    if let Some(task) = tasks.get(&file) {
        let task = task.clone();
        drop(tasks);
        return task.await;
    }
    let browser = browser.clone();
    let settings = settings.clone();
    let path = file.clone();
    let task = tokio::spawn(async move {
        let result = read_once(&browser, &settings).await;
        pending().lock().await.remove(&path);
        result
    });
    let task = async move {
        task.await
            .map_err(|_| failed("The Google Cloud credits read stopped."))?
    }
    .boxed()
    .shared();
    tasks.insert(file, task.clone());
    drop(tasks);
    task.await
}

#[cfg(test)]
#[path = "../test/credits.test.rs"]
mod tests;
