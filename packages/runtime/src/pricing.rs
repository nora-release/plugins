use nora_ai::Model;
use serde_json::Value;

#[path = "pricing_parse.rs"]
mod parsing;

/// Supplier prices use either dollars per million tokens (`cost`) or per token
/// (`pricing`, as returned by OpenRouter). Missing prices remain unknown.
pub(crate) fn apply(entry: &Value, model: &mut Model) {
    if let Some(free) = ["/info/free", "/free", "/is_free", "/isFree"]
        .iter()
        .find_map(|path| entry.pointer(path).and_then(Value::as_bool))
    {
        model.info.free = Some(free);
    }
    let (prices, multiplier) = if let Some(cost) = entry
        .get("cost")
        .and_then(Value::as_object)
        .or_else(|| entry.get("magpie_price").and_then(Value::as_object))
    {
        (cost, 1.0)
    } else if let Some(prices) = entry.get("pricing").and_then(Value::as_object) {
        (prices, 1_000_000.0)
    } else {
        one_hour_for(model);
        mark_paid_extras(model);
        return;
    };
    let Ok(parsed) = parsing::read(prices, multiplier, &model.cost) else {
        model.info.pricing_known = Some(false);
        return;
    };
    let other_paid = prices.iter().any(|(name, value)| {
        !parsing::token_field(name) && parsing::raw_number(value).is_some_and(|value| value > 0.0)
    });
    model.cost = parsed.cost;
    model.info.cache_write_1h = parsed.one_hour;
    model.info.pricing_tiers = parsed.tiers;
    model.info.pricing_known = Some(parsed.complete && !other_paid);
    if other_paid && model.info.free.is_none() {
        model.info.free = Some(false);
    }
    one_hour_for(model);
    mark_paid_extras(model);
}

fn mark_paid_extras(model: &mut Model) {
    let extra_paid = model
        .info
        .cache_write_1h
        .as_ref()
        .and_then(serde_json::Number::as_f64)
        .is_some_and(|price| price > 0.0)
        || model.info.pricing_tiers.iter().any(|tier| {
            [
                &tier.input,
                &tier.output,
                &tier.cache_read,
                &tier.cache_write,
            ]
            .into_iter()
            .chain(tier.cache_write_1h.iter())
            .any(|price| price.as_f64().is_some_and(|price| price > 0.0))
        });
    if extra_paid {
        model.info.free = Some(false);
    }
}

fn one_hour_for(model: &mut Model) {
    if model.cost.cache_write <= 0.0
        || model
            .info
            .cache_write_1h
            .as_ref()
            .and_then(serde_json::Number::as_f64)
            .is_some_and(|price| price > 0.0)
        || !model.id.to_ascii_lowercase().contains("claude")
    {
        return;
    }
    model.info.cache_write_1h = serde_json::Number::from_f64(model.cost.input * 2.0)
        .filter(|price| price.as_f64().is_some_and(|value| value <= 1_000_000.0));
    for tier in &mut model.info.pricing_tiers {
        if tier
            .cache_write_1h
            .as_ref()
            .and_then(serde_json::Number::as_f64)
            .unwrap_or_default()
            <= 0.0
        {
            tier.cache_write_1h = tier
                .input
                .as_f64()
                .and_then(|input| serde_json::Number::from_f64(input * 2.0))
                .filter(|price| price.as_f64().is_some_and(|value| value <= 1_000_000.0));
        }
    }
}

#[cfg(test)]
#[path = "../test/pricing.test.rs"]
mod tests;
