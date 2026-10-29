use nora_ai::{ModelCost, ModelPricingTier};
use serde_json::{Map, Number, Value};
use std::collections::BTreeSet;

pub(super) struct Prices {
    pub cost: ModelCost,
    pub one_hour: Option<Number>,
    pub tiers: Vec<ModelPricingTier>,
    pub complete: bool,
}

const INPUT: &[&str] = &["input", "prompt"];
const OUTPUT: &[&str] = &["output", "completion"];
const READ: &[&str] = &["cache_read", "cacheRead", "input_cache_read"];
const WRITE: &[&str] = &["cache_write", "cacheWrite", "input_cache_write"];
const HOUR: &[&str] = &["cache_write_1h", "cacheWrite1h", "input_cache_write_1h"];

pub(super) fn read(
    values: &Map<String, Value>,
    unit: f64,
    fallback: &ModelCost,
) -> Result<Prices, ()> {
    let input = component(values, INPUT, unit)?;
    let output = component(values, OUTPUT, unit)?;
    let cost = ModelCost {
        input: input.unwrap_or(fallback.input),
        output: output.unwrap_or(fallback.output),
        cache_read: component(values, READ, unit)?.unwrap_or(fallback.cache_read),
        cache_write: component(values, WRITE, unit)?.unwrap_or(fallback.cache_write),
    };
    let one_hour = component(values, HOUR, unit)?.and_then(Number::from_f64);
    let mut tiers = Vec::new();
    let mut thresholds = BTreeSet::new();
    if let Some(raw) = values.get("tiers").filter(|value| !value.is_null()) {
        let rows = raw.as_array().filter(|rows| rows.len() <= 64).ok_or(())?;
        for row in rows {
            let row = row.as_object().ok_or(())?;
            let above = if let Some(tier) = row.get("tier").filter(|value| !value.is_null()) {
                let tier = tier.as_object().ok_or(())?;
                let kind = tier.get("type").and_then(Value::as_str).unwrap_or("");
                if !matches!(kind, "" | "context") {
                    continue;
                }
                tier.get("size").and_then(Value::as_u64)
            } else {
                row.get("above").and_then(Value::as_u64)
            };
            let Some(above) = above.filter(|above| *above > 0) else {
                continue;
            };
            if !thresholds.insert(above) {
                return Err(());
            }
            tiers.push(ModelPricingTier {
                above,
                input: number(component(row, INPUT, unit)?.unwrap_or(cost.input))?,
                output: number(component(row, OUTPUT, unit)?.unwrap_or(cost.output))?,
                cache_read: number(component(row, READ, unit)?.unwrap_or(cost.cache_read))?,
                cache_write: number(component(row, WRITE, unit)?.unwrap_or(cost.cache_write))?,
                // Magpie deliberately does not inherit the base 1-hour price.
                cache_write_1h: component(row, HOUR, unit)?.and_then(Number::from_f64),
            });
        }
    }
    tiers.sort_by_key(|tier| tier.above);
    Ok(Prices {
        cost,
        one_hour,
        tiers,
        complete: input.is_some() && output.is_some(),
    })
}

pub(super) fn token_field(name: &str) -> bool {
    [INPUT, OUTPUT, READ, WRITE, HOUR]
        .iter()
        .any(|keys| keys.contains(&name))
        || name == "tiers"
}

pub(super) fn raw_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn component(values: &Map<String, Value>, names: &[&str], unit: f64) -> Result<Option<f64>, ()> {
    let Some(value) = names
        .iter()
        .find_map(|name| values.get(*name))
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    let value = raw_number(value).ok_or(())? * unit;
    if !value.is_finite() || !(0.0..=1_000_000.0).contains(&value) {
        return Err(());
    }
    Ok(Some(value))
}

fn number(value: f64) -> Result<Number, ()> {
    if !(0.0..=1_000_000.0).contains(&value) {
        return Err(());
    }
    Number::from_f64(value).ok_or(())
}
