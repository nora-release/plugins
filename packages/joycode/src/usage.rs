use crate::*;
pub(crate) fn parse(data: &Value) -> PluginResult<Value> {
    let items = data["usageItems"]
        .as_array()
        .filter(|items| !items.is_empty())
        .ok_or_else(|| transport("JoyCode has not returned quota data."))?;
    let mut plans = Vec::new();
    let mut windows = Vec::new();
    for item in items {
        if !item["title"].is_string()
            || ["used", "total", "remain", "usedPercent"]
                .iter()
                .any(|key| !item[*key].as_f64().is_some_and(f64::is_finite))
        {
            return Err(transport("JoyCode returned incomplete quota data."));
        }
        let badge = text(item, "badge");
        if !badge.is_empty() && !plans.iter().any(|value| value == badge) {
            plans.push(badge.to_owned());
        }
        let unit = if text(item, "unit").is_empty() {
            "积分"
        } else {
            text(item, "unit")
        };
        let mut display = format!(
            "{} / {} · 剩余 {}",
            format(item["used"].as_f64().unwrap()),
            format(item["total"].as_f64().unwrap()),
            format(item["remain"].as_f64().unwrap())
        );
        if !text(item, "expireText").is_empty() {
            display.push_str(" · ");
            display.push_str(text(item, "expireText"));
        }
        windows.push(json!({"label":item["title"],"usedPercent":item["usedPercent"],"used":item["used"],"total":item["total"],"remaining":item["remain"],"unit":unit,"display":display,"countsForRouting":false,"aside":true}));
    }
    let mut result = json!({"available":true,"items":windows});
    if !plans.is_empty() {
        result["plan"] = json!(plans.join(" / "));
    }
    if items.len() == 1 {
        let unit = if text(&items[0], "unit").is_empty() {
            "积分"
        } else {
            text(&items[0], "unit")
        };
        result["balance"] = json!(format!(
            "{} {unit}",
            format(items[0]["remain"].as_f64().unwrap())
        ));
    }
    Ok(result)
}
fn format(value: f64) -> String {
    let value = format!("{value:.6}");
    let value = value.trim_end_matches('0').trim_end_matches('.');
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let sign = whole.starts_with('-');
    let digits = whole.trim_start_matches('-');
    let mut out = String::new();
    if sign {
        out.push('-');
    }
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if !fraction.is_empty() {
        out.push('.');
        out.push_str(fraction);
    }
    out
}
