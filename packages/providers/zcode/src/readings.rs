use nora_provider_subscription_core as core;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|n| n.is_finite())
}
pub(crate) fn when(value: &Value) -> Option<u64> {
    if let Some(text) = value.as_str() {
        let text = text.trim().replace(' ', "T");
        if text.len() == 10 || text.len() == 19 {
            let text = if text.len() == 10 {
                format!("{text}T00:00:00+08:00")
            } else {
                format!("{text}+08:00")
            };
            if let Ok(date) = chrono::DateTime::parse_from_rfc3339(&text) {
                return u64::try_from(date.timestamp_millis()).ok();
            }
        }
    }
    core::timestamp(value)
}
fn span(unit: u64, count: u64) -> Option<f64> {
    let count = if unit == 3 && count == 0 {
        5
    } else {
        count.max(1)
    } as f64;
    Some(
        match unit {
            1 => 60.,
            3 => 3600.,
            4 => 86400.,
            5 => 30. * 86400.,
            6 => 7. * 86400.,
            _ => return None,
        } * count,
    )
}
pub(crate) fn limits(value: &Value) -> Vec<Value> {
    value["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|limit| {
            let period = span(
                number(&limit["unit"]).filter(|n| *n >= 0.).unwrap_or(0.) as u64,
                number(&limit["number"]).filter(|n| *n >= 0.).unwrap_or(0.) as u64,
            );
            let mut aside = limit["type"]
                .as_str()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("TIME_LIMIT"))
                || limit["usage"] == 0;
            let label = if aside
                && limit["type"]
                    .as_str()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("TIME_LIMIT"))
            {
                "MCP · Month".to_owned()
            } else {
                match period {
                    Some(span) if span < 86400. => format!("{} hours", (span / 3600.) as u64),
                    Some(span) if span == 7. * 86400. => "Weekly".into(),
                    Some(span) if span >= 28. * 86400. => "Monthly".into(),
                    Some(span) => format!("{} days", (span / 86400.) as u64),
                    None => "Credits".into(),
                }
            };
            let (total, used) = if let Some(total) = limit["usage"].as_f64().filter(|n| *n > 0.) {
                let used = limit["remaining"]
                    .as_f64()
                    .map(|left| total - left)
                    .or_else(|| limit["currentValue"].as_f64());
                if let Some(used) = used {
                    (total, used.max(0.))
                } else if let Some(percent) = limit["percentage"].as_f64() {
                    (100., percent)
                } else {
                    aside = true;
                    (0., 0.)
                }
            } else if let Some(percent) = limit["percentage"].as_f64() {
                (100., if aside { 0. } else { percent })
            } else {
                aside = true;
                (0., 0.)
            };
            core::usage_item(
                &label,
                used,
                total,
                number(&limit["nextResetTime"])
                    .filter(|t| *t > 0.)
                    .map(|t| t as u64),
                period,
                !aside,
            )
        })
        .collect()
}
pub(crate) fn term(subs: &Value) -> Value {
    for sub in subs.as_array().into_iter().flatten() {
        if !sub["status"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("VALID"))
        {
            continue;
        }
        let auto = sub["autoRenew"] == true || sub["autoRenew"] == 1;
        if let Some(until) = when(&sub["nextRenewTime"]) {
            return json!({"until":core::iso(until),"renew":if auto{"auto"}else{"off"}});
        }
        if !auto {
            let pattern =
                regex::Regex::new(r"\d{4}-\d{2}-\d{2}(?:[ T]\d{2}:\d{2}:\d{2})?").unwrap();
            if let Some(until) = sub["valid"]
                .as_str()
                .and_then(|s| pattern.find_iter(s).last())
                .and_then(|m| when(&json!(m.as_str())))
            {
                return json!({"until":core::iso(until),"renew":"off"});
            }
        }
        return json!({});
    }
    json!({})
}
pub(crate) fn plan(subs: &Value) -> Option<String> {
    subs.as_array()?
        .iter()
        .find(|sub| {
            sub["status"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("VALID"))
        })
        .and_then(|sub| core::text(&sub["productName"]))
        .map(str::to_owned)
}
pub(crate) fn gift_id(model: &str) -> String {
    let rows: Vec<Value> = serde_json::from_str(include_str!("models.json")).unwrap();
    format!(
        "{}-Trial",
        rows.iter()
            .find(|row| row["id"]
                .as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(model)))
            .and_then(|row| row["id"].as_str())
            .unwrap_or(model)
    )
}
fn same(bucket: &Value, plan: &Value) -> bool {
    let identifier = |value: &Value| {
        value
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                if let Value::Number(number) = value {
                    Some(number.to_string())
                } else {
                    None
                }
            })
    };
    if let Some((a, b)) = identifier(&bucket["user_plan_id"]).zip(identifier(&plan["user_plan_id"]))
    {
        return a == b;
    }
    identifier(&bucket["plan_id"]).unwrap_or_default()
        == identifier(&plan["plan_id"]).unwrap_or_default()
}

fn models(bucket: &Value) -> Vec<String> {
    bucket["capabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| s.starts_with("model:") || !s.contains(':'))
        .map(|s| s.strip_prefix("model:").unwrap_or(s).trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
pub(crate) fn spent(bucket: &Value) -> bool {
    if let Some(left) = number(&bucket["remaining_units"]) {
        return left <= 0.;
    }
    number(&bucket["total_units"])
        .zip(number(&bucket["used_units"]))
        .is_some_and(|(total, used)| used >= total)
}
#[derive(Clone, Debug)]
pub(crate) struct Gift {
    pub plan: String,
    pub until: Option<u64>,
    pub ttl: u64,
    pub all: Vec<String>,
    pub live: Vec<String>,
    pub windows: Vec<Value>,
}
pub(crate) fn gift(value: &Value, start: bool) -> Option<Gift> {
    let now = number(&value["server_time"])
        .filter(|n| *n > 0.)
        .unwrap_or(core::now() as f64 / 1000.);
    let live: Vec<&Value> = value["plans"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|plan| {
            plan["status"]
                .as_str()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("active"))
                && !number(&plan["ends_at"]).is_some_and(|end| end > 0. && end <= now)
        })
        .collect();
    if live.is_empty() {
        return None;
    }
    let first = live
        .iter()
        .min_by(|a, b| {
            number(&a["ends_at"])
                .filter(|n| *n > 0.)
                .unwrap_or(f64::INFINITY)
                .total_cmp(
                    &number(&b["ends_at"])
                        .filter(|n| *n > 0.)
                        .unwrap_or(f64::INFINITY),
                )
        })
        .unwrap();
    let until = number(&first["ends_at"])
        .filter(|n| *n > 0.)
        .map(|n| (n * 1000.) as u64);
    let mut ttl = 600_000u64;
    for plan in &live {
        if let Some(end) = number(&plan["ends_at"]).filter(|n| *n > now) {
            ttl = ttl.min(((end - now) * 1000.) as u64);
        }
    }
    let mut rows = Vec::new();
    let mut all = BTreeMap::new();
    let mut available = BTreeSet::new();
    for bucket in value["balances"].as_array().into_iter().flatten() {
        let Some(owner) = live.iter().find(|plan| same(bucket, plan)) else {
            continue;
        };
        let expiry = number(&bucket["expires_at"]).filter(|n| *n > 0.);
        if expiry.is_some_and(|end| end <= now) {
            continue;
        }
        if let Some(end) = expiry {
            ttl = ttl.min(((end - now) * 1000.) as u64);
        }
        let models = models(bucket);
        let spent = spent(bucket);
        for model in &models {
            all.entry(model.to_lowercase())
                .or_insert_with(|| model.clone());
            if !spent {
                available.insert(model.to_lowercase());
            }
        }
        let total = number(&bucket["total_units"]);
        let used = number(&bucket["used_units"]).or_else(|| {
            total
                .zip(number(&bucket["remaining_units"]))
                .map(|(total, left)| total - left)
        });
        if total.is_none() && used.is_none() && number(&bucket["remaining_units"]).is_none() {
            continue;
        }
        let label = core::text(&bucket["show_name"])
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if models.is_empty() {
                    "Credits".into()
                } else {
                    models.join(", ")
                }
            });
        let span = owner["entitlements"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["entitlement_id"] == bucket["entitlement_id"])
            .and_then(|item| item["period"].as_str())
            .and_then(|period| {
                let period = period.to_lowercase();
                if period.contains("day") || period.contains("daily") {
                    Some(86400.)
                } else if period.contains("week") {
                    Some(7. * 86400.)
                } else if period.contains("month") {
                    Some(30. * 86400.)
                } else {
                    None
                }
            })
            .or_else(|| {
                number(&bucket["period_start"])
                    .zip(number(&bucket["period_end"]))
                    .filter(|(a, b)| b > a)
                    .map(|(a, b)| b - a)
            });
        let mut item = core::usage_item(
            &label,
            used.unwrap_or(0.).max(0.),
            total.unwrap_or(0.),
            expiry.map(|n| (n * 1000.) as u64),
            span,
            false,
        );
        if spent {
            item["usedPercent"] = json!(item["usedPercent"].as_f64().unwrap_or(0.).max(100.));
        }
        rows.push((item, models, spent));
    }
    let mut best: BTreeMap<String, usize> = BTreeMap::new();
    for (index, (item, models, spent)) in rows.iter().enumerate() {
        for model in models {
            let key = model.to_lowercase();
            let replace = best.get(&key).is_none_or(|previous| {
                let (prev, _, previous_spent) = &rows[*previous];
                (*previous_spent && !*spent)
                    || previous_spent == spent
                        && item["usedPercent"].as_f64().unwrap_or(0.)
                            < prev["usedPercent"].as_f64().unwrap_or(0.)
            });
            if replace {
                best.insert(key, index);
            }
        }
    }
    let windows = rows
        .into_iter()
        .enumerate()
        .map(|(index, (mut item, models, _))| {
            let selected: Vec<_> = models
                .iter()
                .filter(|model| best.get(&model.to_lowercase()) == Some(&index))
                .cloned()
                .collect();
            let ids = if start {
                selected
                    .iter()
                    .cloned()
                    .chain(selected.iter().map(|m| gift_id(m)))
                    .collect::<Vec<_>>()
            } else {
                selected.iter().map(|m| gift_id(m)).collect()
            };
            item["countsForRouting"] = json!(!ids.is_empty());
            if !ids.is_empty() {
                item["models"] = json!(ids);
            }
            item
        })
        .collect();
    Some(Gift {
        plan: core::text(&first["name"])
            .unwrap_or("Start Plan")
            .to_owned(),
        until,
        ttl,
        all: all.into_values().collect(),
        live: available.into_iter().collect(),
        windows,
    })
}
pub(crate) fn resets(value: &Value, team: bool) -> Option<Value> {
    let mut each = Vec::new();
    let mut five = 0;
    let mut week = 0;
    for (key, window, count) in [
        ("fiveHourResets", "fiveHour", &mut five),
        ("weekResets", "weekly", &mut week),
    ] {
        for reset in value[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|reset| reset["available"] == true)
        {
            *count += 1;
            each.push(json!({"window":window,"until":when(&reset["expireTime"]).map(core::iso)}));
        }
    }
    if five + week == 0 {
        return None;
    }
    each.sort_by_key(|reset| {
        reset["until"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| "~".into())
    });
    Some(
        json!({"count":five+week,"byWindow":true,"fiveHour":five,"weekly":week,"each":each,"until":each.first().and_then(|r|r.get("until")),"team":team}),
    )
}
#[cfg(test)]
#[path = "../test/readings.test.rs"]
mod tests;
