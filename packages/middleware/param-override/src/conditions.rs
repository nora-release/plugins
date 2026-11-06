use crate::paths;
use nora_gateway_middleware::value::{keys, text, truthy};
use serde_json::{json, Value};
pub fn list(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(a) => a.iter().filter(|v| v.is_object()).cloned().collect(),
        Value::Object(_) => keys(value)
            .into_iter()
            .map(|k| json!({"path":k,"mode":"full","value":value[&k]}))
            .collect(),
        _ => vec![],
    }
}
pub fn when(body: &Value, request: &Value, raw: &Value, logic: &str) -> bool {
    let conditions = list(raw);
    if conditions.is_empty() {
        return true;
    }
    let hit = |c: &Value| {
        let Some(value) =
            paths::lookup(body, &c["path"]).or_else(|| paths::lookup(request, &c["path"]))
        else {
            return truthy(&c["pass_missing_key"]);
        };
        let result = compare(value, c.get("value"), c["mode"].as_str().unwrap_or("full"));
        if truthy(&c["invert"]) {
            !result
        } else {
            result
        }
    };
    if logic.eq_ignore_ascii_case("AND") {
        conditions.iter().all(hit)
    } else {
        conditions.iter().any(hit)
    }
}
fn compare(value: &Value, target: Option<&Value>, mode: &str) -> bool {
    let Some(target) = target else {
        return matches!(mode, "prefix" | "suffix" | "contains")
            && match mode {
                "prefix" => text(value).starts_with("undefined"),
                "suffix" => text(value).ends_with("undefined"),
                _ => text(value).contains("undefined"),
            };
    };
    match mode.to_ascii_lowercase().as_str() {
        "full" => {
            if value.is_object() || value.is_array() {
                value.to_string() == target.to_string()
            } else if value.is_number() && target.is_number() {
                value.as_f64() == target.as_f64()
            } else {
                value == target
            }
        }
        "prefix" => text(value).starts_with(&text(target)),
        "suffix" => text(value).ends_with(&text(target)),
        "contains" => text(value).contains(&text(target)),
        mode => {
            let (Some(a), Some(b)) = (value.as_f64(), target.as_f64()) else {
                return false;
            };
            match mode {
                "gt" => a > b,
                "gte" => a >= b,
                "lt" => a < b,
                "lte" => a <= b,
                _ => false,
            }
        }
    }
}
