use nora_ai::Model;
use serde_json::Value;

/// Supplier prices use either dollars per million tokens (`cost`) or per token
/// (`pricing`, as returned by OpenRouter). Missing prices remain unknown.
pub(crate) fn apply(entry: &Value, model: &mut Model) {
    if let Some(free) = ["/info/free", "/free", "/is_free", "/isFree"]
        .iter()
        .find_map(|path| entry.pointer(path).and_then(Value::as_bool))
    {
        model.info.free = Some(free);
    }
    let (prices, multiplier) = if let Some(cost) = entry.get("cost").and_then(Value::as_object) {
        (cost, 1.0)
    } else if let Some(prices) = entry.get("pricing").and_then(Value::as_object) {
        (prices, 1_000_000.0)
    } else {
        return;
    };
    let get = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| prices.get(*key).and_then(number))
            .map(|value| value * multiplier)
            .filter(|value| value.is_finite())
    };
    let input = get(&["input", "prompt"]);
    let output = get(&["output", "completion"]);
    if let Some(input) = input {
        model.cost.input = input;
    }
    if let Some(output) = output {
        model.cost.output = output;
    }
    if let Some(read) = get(&["cache_read", "input_cache_read"]) {
        model.cost.cache_read = read;
    }
    if let Some(write) = get(&["cache_write", "input_cache_write"]) {
        model.cost.cache_write = write;
    }
    let other_paid = prices.iter().any(|(name, value)| {
        !matches!(
            name.as_str(),
            "input"
                | "prompt"
                | "output"
                | "completion"
                | "cache_read"
                | "input_cache_read"
                | "cache_write"
                | "input_cache_write"
        ) && number(value).is_some_and(|value| value > 0.0)
    });
    model.info.pricing_known = Some(input.is_some() && output.is_some() && !other_paid);
    if other_paid && model.info.free.is_none() {
        model.info.free = Some(false);
    }
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
}

#[cfg(test)]
#[path = "../test/pricing.test.rs"]
mod tests;
