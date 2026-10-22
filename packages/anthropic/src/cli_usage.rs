use super::cli;
use chrono::{Datelike, TimeZone, Timelike};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
fn zone_time(
    naive: chrono::NaiveDateTime,
    zone: Option<chrono_tz::Tz>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let local = if let Some(zone) = zone {
        zone.from_local_datetime(&naive)
            .latest()
            .map(|time| time.with_timezone(&chrono::Utc))
    } else {
        chrono::Local
            .from_local_datetime(&naive)
            .latest()
            .map(|time| time.with_timezone(&chrono::Utc))
    };
    local
}
fn reset(raw: &str, now: chrono::DateTime<chrono::Utc>, span: i64) -> Option<String> {
    let (mut text, mut zone) = (raw.trim(), None);
    if let Some((head, tail)) = text.rsplit_once(" (") {
        zone = tail
            .strip_suffix(')')
            .and_then(|zone| zone.parse::<chrono_tz::Tz>().ok());
        if zone.is_none() {
            return None;
        }
        text = head.trim();
    }
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(time.to_rfc3339());
    }
    let normalized = minute_precision(text);
    let text = normalized.as_deref().unwrap_or(text);
    let current = zone
        .map(|zone| now.with_timezone(&zone).naive_local())
        .unwrap_or_else(|| now.with_timezone(&chrono::Local).naive_local());
    let latest = now + chrono::Duration::seconds(span + 7200);
    let mut candidates = Vec::new();
    for format in [
        "%b %d, %Y at %I:%M%p",
        "%b %d, %Y at %I%p",
        "%b %d, %Y, %I:%M%p",
        "%b %d, %Y, %I%p",
    ] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, format) {
            return zone_time(naive, zone).map(|time| time.to_rfc3339());
        }
    }
    for year in current.year() - 1..=current.year() + 1 {
        let input = format!("{year} {text}");
        for format in [
            "%Y %b %d at %I:%M%p",
            "%Y %b %d at %I%p",
            "%Y %b %d, %I:%M%p",
            "%Y %b %d, %I%p",
        ] {
            if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(&input, format) {
                if let Some(time) = zone_time(naive, zone) {
                    candidates.push(time);
                }
            }
        }
    }
    if candidates.is_empty() {
        for format in ["%I:%M%p", "%I%p"] {
            if let Ok(time) = chrono::NaiveTime::parse_from_str(text, format) {
                for day in -1..=2 {
                    if let Some(date) = current
                        .date()
                        .checked_add_signed(chrono::Duration::days(day))
                    {
                        if let Some(time) =
                            zone_time(date.and_hms_opt(time.hour(), time.minute(), 0)?, zone)
                        {
                            candidates.push(time);
                        }
                    }
                }
            }
        }
    }
    candidates.sort();
    candidates
        .iter()
        .find(|time| **time >= now && **time <= latest)
        .or_else(|| candidates.iter().rev().find(|time| **time <= now))
        .map(|time| time.to_rfc3339())
}

// Chrono requires minutes to construct a time; Claude also prints hour-only
// values such as "3pm", including after a calendar date.
fn minute_precision(text: &str) -> Option<String> {
    let start = text
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map_or(0, |(at, character)| at + character.len_utf8());
    let time = &text[start..];
    let lower = time.to_ascii_lowercase();
    if !(lower.ends_with("am") || lower.ends_with("pm")) {
        return None;
    }
    let suffix = time.len() - 2;
    let hour = time[..suffix].parse::<u32>().ok()?;
    if !(1..=12).contains(&hour) {
        return None;
    }
    Some(format!(
        "{}{}:00{}",
        &text[..start],
        &time[..suffix],
        &time[suffix..]
    ))
}
pub(crate) fn parse(text: &str, now: chrono::DateTime<chrono::Utc>) -> PluginResult<Value> {
    let ansi = regex::Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap();
    let text = ansi.replace_all(text, "");
    let lower = text.to_ascii_lowercase();
    if [
        "not logged in",
        "not signed in",
        "authentication failed",
        "unauthorized",
        "oauth session expired",
    ]
    .iter()
    .any(|part| lower.contains(part))
    {
        return Err(cli::error());
    }
    let pattern = regex::Regex::new(
        r"^Current (session|week(?: \(([^)]+)\))?):\s*([0-9.]+)% used(?:\s*·\s*resets (.+))?$",
    )
    .unwrap();
    let mut items = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for line in text.lines() {
        let Some(capture) = pattern.captures(line.trim()) else {
            continue;
        };
        let Some(percent) = capture[3]
            .parse::<f64>()
            .ok()
            .filter(|percent| percent.is_finite() && (0.0..=100.0).contains(percent))
        else {
            continue;
        };
        let scope = capture
            .get(2)
            .map(|scope| scope.as_str().trim())
            .unwrap_or_default();
        let weekly = &capture[1] != "session";
        let id = if weekly {
            if scope.is_empty() || scope.eq_ignore_ascii_case("all models") {
                "seven_day".into()
            } else {
                format!("seven_day_{}", scope.to_ascii_lowercase())
            }
        } else {
            "five_hour".into()
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let span = if weekly { 604800 } else { 18000 };
        let mut item = nora_provider::usage::parsing::quantities(
            &id,
            if weekly {
                if scope.is_empty() {
                    "Weekly"
                } else {
                    scope
                }
            } else {
                "Five hours"
            },
            &json!({"used_percent":percent}),
            None,
            None,
        )
        .unwrap();
        item["windowSeconds"] = json!(span);
        let model = match scope.to_ascii_lowercase().as_str() {
            "opus" => Some("*opus*"),
            "sonnet" => Some("*sonnet*"),
            "fable" => Some("*fable*"),
            _ => None,
        };
        item["countsForRouting"] =
            json!(scope.is_empty() || scope.eq_ignore_ascii_case("all models") || model.is_some());
        if let Some(model) = model {
            item["modelIDs"] = json!([model]);
        }
        if let Some(at) = capture
            .get(4)
            .and_then(|reset_text| reset(reset_text.as_str(), now, span))
        {
            item["resetsAt"] = json!(at);
        }
        items.push(item);
    }
    if items.is_empty() {
        return Err(PluginError::transport(
            "Claude Code's usage command did not return readable allowances.",
        ));
    }
    Ok(nora_provider::usage::parsing::result(
        &json!({}),
        items,
        Vec::new(),
    ))
}
#[cfg(test)]
#[path = "../test/cli_usage.test.rs"]
mod tests;
