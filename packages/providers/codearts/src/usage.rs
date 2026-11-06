use crate::provider::*;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use nora_ai::{HttpClient, HttpMethod};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const TOTAL: &str = "usageTotalPackageCredit";
const BASIC: &str = "usageBasicPackageCredit";
const BONUS: &str = "usageBonusPackageCredit";

#[derive(Clone)]
pub(super) struct Batch {
    pub metric: String,
    pub amount: f64,
    pub left: f64,
    pub expires: u64,
}

pub(super) async fn fetch(
    provider: &CodeArts,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Value> {
    let statistics = provider
        .get(
            client,
            credentials,
            &format!("{SNAP}/snap-manager/v1/statistics/plugin"),
            snap_headers(),
        )
        .await?;
    let account = provider.account(credentials).await?;
    let mut state = account.models.lock().await;
    let benefit = if state.benefit.as_ref().is_none_or(|ids| !ids.is_empty()) {
        provider
            .get(
                client,
                credentials,
                &format!("{OPEN}/api/v1/user/tokens/balance"),
                BTreeMap::new(),
            )
            .await
            .ok()
    } else {
        None
    };
    if state.batches_at == 0 || now().saturating_sub(state.batches_at) > 600_000 {
        if let Ok(batches) = batches(provider, client, credentials).await {
            state.batches = Some(batches);
        }
        state.batches_at = now();
    }
    let batches = state
        .batches
        .clone()
        .unwrap_or_else(|| read_ledger(provider, credentials));
    let ids = state
        .benefit
        .as_ref()
        .map(|ids| ids.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_else(|| {
            crate::models::BENEFIT_DEFAULTS
                .into_iter()
                .map(str::to_owned)
                .collect()
        });
    let mut value = parse(&statistics, benefit.as_ref(), &batches, &ids, now())?;
    let (_, name) = identity(credentials);
    if !name.is_empty() {
        value["user"] = json!(name);
    }
    Ok(value)
}

pub(super) fn parse(
    raw: &Value,
    benefit: Option<&Value>,
    batches: &[Batch],
    ids: &[String],
    at: u64,
) -> PluginResult<Value> {
    let data = if raw.get("code").is_some() {
        if number(raw.get("code").unwrap()).unwrap_or(-1.0) != 0.0 {
            return Err(PluginError::transport("CodeArts refused the usage lookup."));
        }
        raw.get("data")
            .ok_or_else(|| invalid("CodeArts returned no credit statistics."))?
    } else {
        raw
    };
    if !data.is_object() {
        return Err(invalid("CodeArts credit statistics could not be read."));
    }
    let package = data.get("package").unwrap_or(&Value::Null);
    let plan = ["package_name_cn", "package_name_en", "spec_code"]
        .into_iter()
        .map(|field| text(package, field))
        .find(|value| !value.is_empty())
        .unwrap_or("");
    let reset = batches
        .iter()
        .filter(|batch| batch.metric == BASIC && batch.expires > at)
        .map(|batch| batch.expires)
        .min()
        .or_else(|| data.get("end_date").and_then(date));
    let mut windows = Vec::new();
    for metric in data
        .get("metrics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let key = text(metric, "name");
        let name = match key {
            TOTAL => "总积分",
            BASIC => "基础积分包",
            "usageOnDemandPackageCredit" => "按需积分包",
            BONUS => "赠送积分包",
            _ => continue,
        };
        let limit = metric
            .get("package_credit_amount")
            .and_then(number)
            .unwrap_or(0.0);
        let remain = metric
            .get("package_credit_remain")
            .and_then(number)
            .unwrap_or(0.0);
        if limit <= 0.0 && remain <= 0.0 {
            continue;
        }
        let used = (limit - remain).max(0.0);
        let mut window = json!({"name":name,"used":if limit>0.0{(used/limit*100.0).clamp(0.0,100.0)}else{0.0},"amount":used,"limit":limit,"unit":"credits","display":format!("{used:.0} / {limit:.0}"),"aside":key!=TOTAL});
        if [TOTAL, BASIC].contains(&key) {
            if let Some(reset) = reset {
                window["resetsAt"] = json!(stamp(reset));
            }
        }
        if key != BASIC {
            let own = batches
                .iter()
                .filter(|batch| {
                    if key == TOTAL {
                        batch.metric != BASIC
                    } else {
                        batch.metric == key
                    }
                })
                .cloned()
                .collect::<Vec<_>>();
            if let Some((expires, left)) = soonest(&own, at) {
                let remaining = (limit - used).max(0.0);
                if remaining > 0.0 {
                    window["runsOutAt"] = json!(stamp(expires));
                    window["runsOut"] = json!(left.min(remaining));
                }
            }
        }
        let expiring = metric
            .get("package_credit_expiring_amount")
            .and_then(number)
            .unwrap_or(0.0);
        if expiring > 0.0 {
            window["display"] = json!(format!("{used:.0} / {limit:.0} · {expiring:.0} 即将过期"));
        }
        windows.push(window);
    }
    windows.sort_by_key(|window| text(window, "name") != "总积分");
    if let Some(benefit) = benefit {
        let benefit = benefit
            .get("result")
            .or_else(|| benefit.get("data"))
            .unwrap_or(benefit);
        let limit = benefit
            .get("daily_token_limit")
            .and_then(number)
            .unwrap_or(0.0);
        if limit > 0.0 {
            let used = benefit
                .get("daily_tokens_used")
                .and_then(number)
                .unwrap_or(0.0)
                .clamp(0.0, limit);
            let reset = (at + 8 * 3_600_000) / 86_400_000 * 86_400_000 + 86_400_000 - 8 * 3_600_000;
            let mut window = json!({"name":"免费额度","used":used/limit*100.0,"amount":used,"limit":limit,"unit":"tokens","aside":true,"span":86400,"resetsAt":stamp(reset)});
            if !ids.is_empty() {
                window["models"] = json!(ids);
            }
            windows.push(window);
        }
    }
    if windows.is_empty() {
        return Err(PluginError::transport(
            "This CodeArts account has no credit allowance.",
        ));
    }
    Ok(json!({"plan":plan,"windows":windows,"signIn":"kept"}))
}

async fn batches(
    provider: &CodeArts,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Vec<Batch>> {
    let mut batches = Vec::new();
    let mut reached = false;
    for query in [
        json!({"pageNum":1,"pageSize":100,"sortField":"created_time","sortOrder":"desc","packageTypes":["basic","ondemand"]}),
        json!({"pageNum":1,"pageSize":100,"packageTypes":["bonus"]}),
    ] {
        if let Ok(value) = provider
            .snap(
                client,
                credentials,
                HttpMethod::Post,
                &format!("{SNAP}/snap-manager/v1/package/credit/page"),
                Some(query),
            )
            .await
        {
            if let Some(list) = value
                .get("list")
                .and_then(Value::as_array)
                .filter(|list| list.len() <= 100)
            {
                reached = true;
                for package in list {
                    let metric = match text(package, "packageType") {
                        "basic" => BASIC,
                        "ondemand" => "usageOnDemandPackageCredit",
                        "bonus" => BONUS,
                        _ => continue,
                    };
                    let amount = package.get("creditAmount").and_then(number).unwrap_or(0.0);
                    let left = (amount - package.get("creditUsed").and_then(number).unwrap_or(0.0))
                        .max(0.0);
                    let expires = package.get("expiredTime").and_then(date).unwrap_or(0);
                    batches.push(Batch {
                        metric: metric.into(),
                        amount,
                        left,
                        expires,
                    });
                }
            }
        }
    }
    if let Ok(value) = provider
        .snap(
            client,
            credentials,
            HttpMethod::Get,
            &format!("{SNAP}/v1/ops/credit/overview"),
            None,
        )
        .await
    {
        if let Some(campaigns) = value
            .get("items")
            .and_then(Value::as_array)
            .filter(|campaigns| campaigns.len() <= 100)
        {
            reached = true;
            for campaign in campaigns {
                let remaining = campaign.get("totalClaimed").and_then(number).unwrap_or(0.0)
                    - campaign
                        .get("totalConsumed")
                        .and_then(number)
                        .unwrap_or(0.0)
                    - campaign.get("totalExpired").and_then(number).unwrap_or(0.0);
                if remaining <= 0.0 {
                    continue;
                }
                let Some(id) = campaign.get("campaignId").filter(|id| !id.is_null()) else {
                    continue;
                };
                let id = id
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| id.to_string());
                let url = format!(
                    "{SNAP}/v1/ops/credit/campaign/{}?pageNum=1&pageSize=100",
                    crate::sign::encode(&id)
                );
                if let Ok(value) = provider
                    .snap(client, credentials, HttpMethod::Get, &url, None)
                    .await
                {
                    for benefit in value
                        .get("benefits")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .take(100)
                    {
                        let amount = benefit.get("claimedAmount").and_then(number).unwrap_or(0.0);
                        let left = (amount
                            - benefit
                                .get("consumedAmount")
                                .and_then(number)
                                .unwrap_or(0.0)
                            - benefit.get("expiredAmount").and_then(number).unwrap_or(0.0))
                        .max(0.0);
                        batches.push(Batch {
                            metric: BONUS.into(),
                            amount,
                            left,
                            expires: benefit.get("expireAt").and_then(date).unwrap_or(0),
                        });
                    }
                }
            }
        }
    }
    if !reached {
        return Err(PluginError::transport(
            "CodeArts credit batches are unavailable.",
        ));
    }
    Ok(batches)
}

pub(super) fn soonest(batches: &[Batch], at: u64) -> Option<(u64, f64)> {
    let expires = batches
        .iter()
        .filter(|batch| batch.left > 0.0 && batch.expires > at)
        .map(|batch| batch.expires)
        .min()?;
    let same_day = day(expires);
    let left = batches
        .iter()
        .filter(|batch| batch.left > 0.0 && batch.expires > at && day(batch.expires) == same_day)
        .map(|batch| batch.left.min(batch.amount).max(0.0))
        .sum();
    Some((expires, left))
}

async fn campaign(
    provider: &CodeArts,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Option<Value>> {
    let value = provider
        .snap(
            client,
            credentials,
            HttpMethod::Get,
            &format!("{SNAP}/v1/ops/delivery?channel=IDE"),
            None,
        )
        .await?;
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .filter(|items| items.len() <= 1000)
        .ok_or_else(|| invalid("CodeArts did not return a valid benefit campaign list."))?;
    Ok(items
        .iter()
        .find(|item| text(item, "type") == "USER_LOGIN")
        .cloned())
}
fn claimed(value: &Value) -> bool {
    value.get("claimable").and_then(Value::as_bool) != Some(true)
        && ["CLAIMED", "CONFIRMED", "CONSUMED"].contains(&text(value, "status"))
}
pub(super) async fn checkin(
    provider: &CodeArts,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Value> {
    let Some(item) = campaign(provider, client, credentials).await? else {
        return Ok(
            json!({"outcome":"inactive","message":"No daily CodeArts benefit is available."}),
        );
    };
    if claimed(&item) {
        return Ok(json!({"outcome":"done","message":"Today's benefit was already claimed."}));
    }
    if item.get("claimable").and_then(Value::as_bool) != Some(true) {
        return Ok(
            json!({"outcome":"ineligible","message":"This account cannot claim the current daily benefit."}),
        );
    }
    let campaign_id = item
        .get("campaignId")
        .filter(|id| !id.is_null() && id.as_str() != Some(""))
        .ok_or_else(|| invalid("The CodeArts benefit has no campaign identity."))?;
    let benefit = provider
        .snap(
            client,
            credentials,
            HttpMethod::Post,
            &format!("{SNAP}/v1/ops/claim"),
            Some(json!({"campaignId":campaign_id,"channel":"IDE"})),
        )
        .await;
    let benefit = match benefit {
        Ok(benefit) => benefit,
        Err(error) => {
            if let Ok(Some(again)) = campaign(provider, client, credentials).await {
                if claimed(&again) {
                    return Ok(
                        json!({"outcome":"done","message":"Today's benefit was already claimed."}),
                    );
                }
            }
            return Err(error);
        }
    };
    if benefit.get("id") != Some(&Value::Null) {
        provider
            .snap(
                client,
                credentials,
                HttpMethod::Post,
                &format!("{SNAP}/v1/ops/confirm"),
                Some(json!({"campaignId":campaign_id})),
            )
            .await?;
    }
    let credit = benefit
        .get("benefitAmount")
        .and_then(number)
        .filter(|credit| *credit > 0.0)
        .or_else(|| item.get("benefitAmount").and_then(number))
        .unwrap_or(0.0);
    let at = now();
    let expires = [
        "expire_time",
        "expireTime",
        "expired_time",
        "expiredTime",
        "expire_at",
        "expireAt",
        "valid_end_time",
        "end_time",
        "endTime",
    ]
    .into_iter()
    .find_map(|field| benefit.get(field).and_then(date))
    .unwrap_or(at + 30 * 86_400_000);
    let grant = json!({"at":at,"credit":credit,"expiresAt":expires});
    let _ = record_ledger(provider, credentials, grant);
    provider
        .account(credentials)
        .await?
        .models
        .lock()
        .await
        .batches_at = 0;
    Ok(
        json!({"outcome":"claimed","credit":credit,"message":if credit>0.0{format!("+{credit} credits")}else{"Benefit claimed.".into()}}),
    )
}

fn ledger(provider: &CodeArts, credentials: &Value) -> PluginResult<std::path::PathBuf> {
    let (id, _) = identity(credentials);
    let key = if id.is_empty() {
        session(credentials).0
    } else {
        id
    };
    Ok(provider.data()?.join(format!(
        "grants-{}.json",
        crate::sign::digest(key.as_bytes())
    )))
}
fn read_grants(provider: &CodeArts, credentials: &Value) -> Vec<Value> {
    let Ok(path) = ledger(provider, credentials) else {
        return Vec::new();
    };
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    if !file
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= 1024 * 1024)
    {
        return Vec::new();
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    if file.take(1024 * 1024 + 1).read_to_end(&mut bytes).is_err() || bytes.len() > 1024 * 1024 {
        return Vec::new();
    }
    serde_json::from_slice::<Vec<Value>>(&bytes)
        .unwrap_or_default()
        .into_iter()
        .filter(|grant| {
            grant
                .get("at")
                .and_then(Value::as_u64)
                .is_some_and(|at| now().saturating_sub(at) < 40 * 86_400_000)
        })
        .take(1000)
        .collect()
}
fn read_ledger(provider: &CodeArts, credentials: &Value) -> Vec<Batch> {
    read_grants(provider, credentials)
        .into_iter()
        .filter_map(|grant| {
            Some(Batch {
                metric: BONUS.into(),
                amount: grant.get("credit").and_then(number)?,
                left: grant.get("credit").and_then(number)?,
                expires: grant.get("expiresAt").and_then(date)?,
            })
        })
        .collect()
}
fn record_ledger(provider: &CodeArts, credentials: &Value, grant: Value) -> PluginResult<()> {
    use std::io::Write;
    let path = ledger(provider, credentials)?;
    let mut grants = read_grants(provider, credentials);
    let day_of_grant = grant.get("at").and_then(Value::as_u64).map(day);
    if grants
        .iter()
        .any(|previous| previous.get("at").and_then(Value::as_u64).map(day) == day_of_grant)
    {
        return Ok(());
    }
    grants.push(grant);
    std::fs::create_dir_all(provider.data()?)
        .map_err(|_| invalid("The CodeArts credit ledger directory is unavailable."))?;
    let temporary = path.with_extension(format!("{}.tmp", crate::sign::id(8)?));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(serde_json::to_string(&grants).unwrap().as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, &path)
    })()
    .map_err(|_| invalid("The CodeArts credit ledger could not be saved."));
    let _ = std::fs::remove_file(temporary);
    result
}

pub(super) fn date(value: &Value) -> Option<u64> {
    if let Some(number) = number(value).filter(|number| *number > 0.0) {
        let stamp = if number < 1e11 {
            number * 1000.0
        } else {
            number
        };
        if stamp > i64::MAX as f64 {
            return None;
        }
        DateTime::<Utc>::from_timestamp_millis(stamp as i64)?;
        return Some(stamp as u64);
    }
    let value = value.as_str()?.trim();
    if let Ok(value) = DateTime::parse_from_rfc3339(value) {
        return (value.timestamp_millis() > 0).then_some(value.timestamp_millis() as u64);
    }
    let date = [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ]
    .into_iter()
    .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
    .or_else(|| {
        NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .ok()
            .and_then(|date| date.and_hms_opt(0, 0, 0))
    })?;
    let stamp = FixedOffset::east_opt(8 * 3600)?
        .from_local_datetime(&date)
        .single()?
        .timestamp_millis();
    (stamp > 0).then_some(stamp as u64)
}
pub(super) fn day(at: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(at.min(i64::MAX as u64) as i64)
        .unwrap_or_default()
        .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
        .format("%Y-%m-%d")
        .to_string()
}
pub(super) fn stamp(at: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(at.min(i64::MAX as u64) as i64)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
