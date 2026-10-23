//! Magpie's declared USD prices, including whole-request context tiers.
use serde_json::{json, Value};
#[derive(Clone, Copy)]
struct Rates {
    input: f64,
    output: f64,
    read: f64,
    write: f64,
    hour: f64,
}
fn rate(value: &Value, key: &str, default: f64) -> Option<f64> {
    match value.get(key) {
        None | Some(Value::Null) => Some(default),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite() && (0.0..=1_000_000.0).contains(value)),
    }
}
fn rates(value: &Value, base: Rates) -> Option<Rates> {
    value.as_object()?;
    Some(Rates {
        input: rate(value, "input", base.input)?,
        output: rate(value, "output", base.output)?,
        read: rate(value, "cache_read", base.read)?,
        write: rate(value, "cache_write", base.write)?,
        hour: rate(value, "cache_write_1h", 0.0)?,
    })
}
fn one_hour(id: &str, rates: &mut Rates) {
    if id.to_ascii_lowercase().contains("claude") && rates.write > 0.0 && rates.hour <= 0.0 {
        rates.hour = 2.0 * rates.input;
    }
}
pub(crate) fn apply(model: &mut Value, entry: &Value, id: &str) {
    let Some(price) = entry.get("magpie_price") else {
        return;
    };
    let Some(mut base) = rates(
        price,
        Rates {
            input: 0.0,
            output: 0.0,
            read: 0.0,
            write: 0.0,
            hour: 0.0,
        },
    ) else {
        return;
    };
    let mut tiers = Vec::new();
    if let Some(raw) = price.get("tiers").filter(|value| !value.is_null()) {
        let Some(raw) = raw.as_array().filter(|raw| raw.len() <= 256) else {
            return;
        };
        for tier in raw {
            let Some(fields) = tier.as_object() else {
                return;
            };
            let above = if let Some(kind) = fields.get("tier").filter(|value| !value.is_null()) {
                if !kind.is_object() {
                    return;
                }
                if kind
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| !kind.is_empty() && kind != "context")
                {
                    continue;
                }
                kind.get("size").and_then(Value::as_u64)
            } else {
                tier.get("above").and_then(Value::as_u64)
            };
            let Some(above) = above.filter(|above| *above > 0 && *above <= i64::MAX as u64) else {
                continue;
            };
            let Some(values) = rates(tier, base) else {
                return;
            };
            tiers.push(json!({"above":above,"input":values.input,"output":values.output,"cacheRead":values.read,"cacheWrite":values.write,"cacheWrite1h":values.hour}));
        }
    }
    if id.to_ascii_lowercase().contains("claude") && base.write > 0.0 && base.hour <= 0.0 {
        one_hour(id, &mut base);
        for tier in &mut tiers {
            if tier["cacheWrite1h"].as_f64().unwrap_or(0.0) <= 0.0 {
                tier["cacheWrite1h"] = json!(2.0 * tier["input"].as_f64().unwrap());
            }
        }
    }
    tiers.sort_by_key(|tier| tier["above"].as_u64().unwrap());
    model["cost"] = json!({"input":base.input,"output":base.output,"cacheRead":base.read,"cacheWrite":base.write});
    model["info"]["pricingKnown"] = json!(true);
    model["info"]["cacheWrite1h"] = json!(base.hour);
    model["info"]["pricingTiers"] = json!(tiers);
    model["info"]["free"] = json!(
        base.input == 0.0
            && base.output == 0.0
            && base.read == 0.0
            && base.write == 0.0
            && base.hour == 0.0
            && tiers.iter().all(|tier| [
                "input",
                "output",
                "cacheRead",
                "cacheWrite",
                "cacheWrite1h"
            ]
            .iter()
            .all(|key| tier[*key].as_f64() == Some(0.0)))
    );
}
#[cfg(test)]
#[path = "../test/pricing.test.rs"]
mod tests;
