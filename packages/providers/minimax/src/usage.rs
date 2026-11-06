use nora_provider::usage::parsing::number;
use nora_provider_quota::{finish, window};
use serde_json::{json, Value};

pub fn parse(payload: &Value) -> Result<Value, String> {
    if payload
        .pointer("/base_resp/status_code")
        .and_then(Value::as_i64)
        != Some(0)
    {
        return Err("MiniMax did not accept the Coding Plan query.".into());
    }
    let rows = payload["model_remains"]
        .as_array()
        .ok_or("MiniMax did not return Coding Plan allowances.")?;
    let mut items = Vec::new();
    for row in rows {
        let Some(name) = row["model_name"].as_str().filter(|name| {
            !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
        }) else {
            continue;
        };
        for (id, percent, status, start, end, total, count, fallback) in [
            (
                "interval",
                "current_interval_remaining_percent",
                "current_interval_status",
                "start_time",
                "end_time",
                "current_interval_total_count",
                "current_interval_usage_count",
                None,
            ),
            (
                "weekly",
                "current_weekly_remaining_percent",
                "current_weekly_status",
                "weekly_start_time",
                "weekly_end_time",
                "current_weekly_total_count",
                "current_weekly_usage_count",
                Some(604_800.0),
            ),
        ] {
            let status = row[status].as_i64().unwrap_or_default();
            if status == 3 {
                continue;
            }
            let left = number(&row[percent]);
            if left.is_none() && status != 2 {
                continue;
            }
            let mut values =
                json!({"used_percent":if status == 2 { 100.0 } else { 100.0 - left.unwrap() }});
            if let (Some(count), Some(total), Some(left)) = (
                number(&row[count]),
                number(&row[total]).filter(|total| *total > 0.0),
                left,
            ) {
                if count >= 0.0 && count <= total {
                    let as_used = ((total - count) / total * 100.0 - left).abs();
                    let as_left = (count / total * 100.0 - left).abs();
                    if as_used.min(as_left) <= 1.0 {
                        values["limit"] = json!(total);
                        values["used"] = json!(if status == 2 {
                            total
                        } else if as_used < as_left {
                            count
                        } else {
                            total - count
                        });
                    }
                }
            }
            let seconds = |value: &Value| {
                number(value).map(|number| {
                    if number >= 100_000_000_000.0 {
                        number / 1000.0
                    } else {
                        number
                    }
                })
            };
            let span = seconds(&row[start])
                .zip(seconds(&row[end]))
                .filter(|(start, end)| *start > 0.0 && end > start)
                .map(|(start, end)| end - start)
                .or(fallback);
            let general = name.eq_ignore_ascii_case("general");
            let label = if general {
                if id == "weekly" {
                    "Weekly".into()
                } else {
                    "Allowance".into()
                }
            } else {
                format!("{name} · {id}")
            };
            if let Some(item) = window(
                &format!("{name}:{id}"),
                &label,
                &values,
                row.get(end),
                span,
                general,
                name.eq_ignore_ascii_case("video").then_some("videos"),
            ) {
                items.push(item);
            }
        }
    }
    finish(items, Vec::new())
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
