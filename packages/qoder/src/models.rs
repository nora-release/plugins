use crate::*;
use nora_ai::{InputModality, Model};
use std::collections::BTreeSet;
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub key: String,
    pub source: String,
    pub name: String,
    pub images: bool,
    pub context: u64,
    pub windows: Vec<u64>,
    pub default_window: u64,
    pub thinks: bool,
    pub always_thinks: bool,
    pub efforts: Vec<String>,
    pub default_effort: String,
    pub free: bool,
    pub rate: Option<f64>,
    pub previous: Option<f64>,
    pub config: Value,
}
impl Entry {
    fn parse(raw: &Value) -> PluginResult<Self> {
        let key = text(raw, "key");
        if key.is_empty() || key.len() > 512 || key.chars().any(char::is_control) {
            return Err(transport("Qoder returned an invalid model ID."));
        }
        let mut windows = Vec::new();
        let mut default_window = 0;
        if let Some(config) = raw
            .get("context_config")
            .or_else(|| raw.get("contextConfig"))
            .and_then(Value::as_object)
        {
            for value in config.values() {
                let tokens = value
                    .get("token_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if tokens == 0 {
                    continue;
                }
                windows.push(tokens);
                if value
                    .get("is_default")
                    .is_some_and(|value| value == true || value == 1 || value == "1")
                {
                    default_window = tokens;
                }
            }
        }
        if windows.is_empty() {
            windows = raw
                .get("available_context_windows")
                .or_else(|| raw.get("availableContextWindows"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
                .filter(|value| *value > 0)
                .collect();
            default_window = raw
                .get("default_context_window")
                .or_else(|| raw.get("defaultContextWindow"))
                .and_then(Value::as_u64)
                .filter(|value| windows.contains(value))
                .unwrap_or(0);
        }
        windows.sort_unstable();
        windows.dedup();
        let context = windows.last().copied().unwrap_or_else(|| {
            raw.get("max_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        });
        let mut thinks = raw.get("is_reasoning").and_then(Value::as_bool) == Some(true);
        let mut always_thinks = false;
        let mut default_effort = String::new();
        let mut efforts = raw
            .get("reasoning_efforts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if let Some(config) = raw
            .get("thinking_config")
            .filter(|config| config.is_object())
        {
            thinks = config.get("enabled").is_some_and(|value| !value.is_null());
            always_thinks = thinks && config.get("disabled").is_none_or(Value::is_null);
            efforts.clear();
            if thinks {
                if let Some(enabled) = config
                    .pointer("/enabled/efforts")
                    .and_then(Value::as_object)
                {
                    for (name, value) in enabled {
                        if name.len() > 64 {
                            continue;
                        }
                        efforts.push(name.clone());
                        if value.get("is_default").and_then(Value::as_bool) == Some(true) {
                            default_effort = name.clone();
                        }
                    }
                }
            }
        }
        let order = ["minimal", "low", "medium", "high", "xhigh", "max"];
        efforts.sort_by(|a, b| {
            order
                .iter()
                .position(|value| value == a)
                .unwrap_or(order.len())
                .cmp(
                    &order
                        .iter()
                        .position(|value| value == b)
                        .unwrap_or(order.len()),
                )
                .then_with(|| a.cmp(b))
        });
        efforts.dedup();
        let price = raw
            .get("price_factor")
            .or_else(|| raw.get("priceFactor"))
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite());
        let promotion = raw
            .get("promotion")
            .or_else(|| raw.get("prommotion"))
            .unwrap_or(&Value::Null);
        let before = promotion
            .get("before_promotion_price_factor")
            .or_else(|| promotion.get("beforePromotionPriceFactor"))
            .and_then(number)
            .unwrap_or(0.0);
        let discounted =
            promotion.get("active").and_then(Value::as_bool) == Some(true) && before > 0.0;
        let free = price
            .map(|price| price == 0.0 && !discounted)
            .unwrap_or_else(|| {
                raw.get("is_free")
                    .or_else(|| raw.get("isFree"))
                    .and_then(Value::as_bool)
                    == Some(true)
            });
        let mut rate = price.map(|price| price.max(0.0));
        let mut previous = raw
            .get("original_price_factor")
            .or_else(|| raw.get("originalPriceFactor"))
            .and_then(number)
            .filter(|value| *value > 0.0);
        if discounted {
            previous = Some(before);
            if rate == Some(0.0) {
                rate = Some(
                    before
                        * promotion
                            .get("discount_factor")
                            .or_else(|| promotion.get("discountFactor"))
                            .and_then(number)
                            .unwrap_or(0.0),
                );
            }
        }
        if previous
            .zip(rate)
            .is_some_and(|(previous, rate)| previous <= rate)
        {
            previous = None;
        }
        Ok(Self {
            key: key.into(),
            source: text(raw, "source").into(),
            name: if text(raw, "display_name").is_empty() {
                key
            } else {
                text(raw, "display_name")
            }
            .into(),
            images: raw.get("is_vl").and_then(Value::as_bool) == Some(true),
            context,
            windows,
            default_window,
            thinks,
            always_thinks,
            efforts,
            default_effort,
            free,
            rate,
            previous,
            config: raw.clone(),
        })
    }
    pub fn model(&self, site: Site, deployment: &Deployment) -> PluginResult<Value> {
        let mut model = Model::new(&self.key, "openai-completions", site.id());
        model.base_url = deployment.api.clone();
        model.name = self.name.clone();
        model.context_window = self.context;
        model.max_tokens = 32000;
        model.reasoning = self.thinks;
        model.input = if self.images {
            vec![InputModality::Text, InputModality::Image]
        } else {
            vec![InputModality::Text]
        };
        model.info.chat = Some(true);
        model.info.tool_calling = Some(true);
        model.info.free = Some(self.free);
        model.info.pricing_known = Some(false);
        if self.thinks {
            model.info.reasoning_efforts = self.efforts.clone();
        }
        let mut value = serde_json::to_value(model)
            .map_err(|_| invalid("The Qoder model cannot be represented."))?;
        if let Some(rate) = self.rate {
            value["info"]["creditMultiplier"] = json!(rate);
        }
        if let Some(rate) = self.previous {
            value["info"]["previousCreditMultiplier"] = json!(rate);
        }
        Ok(value)
    }
}
pub(crate) fn listing(value: &Value) -> PluginResult<Vec<Entry>> {
    if !value.is_object() {
        return Err(transport("Qoder returned no model catalog."));
    }
    let app = value
        .get("app")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let chat = value
        .get("chat")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if app.len() + chat.len() > 8192 {
        return Err(transport("The Qoder model catalog exceeds its size limit."));
    }
    let mut out = app;
    let mut after = 0;
    for model in chat {
        if let Some(index) = out
            .iter()
            .position(|entry| entry.get("key") == model.get("key"))
        {
            after = index + 1;
        } else {
            out.insert(after, model);
            after += 1;
        }
    }
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for model in out {
        if model.get("enable").and_then(Value::as_bool) != Some(true)
            || ["", "auto", "default"].contains(&text(&model, "key"))
            || !seen.insert(text(&model, "key").to_owned())
        {
            continue;
        }
        entries.push(Entry::parse(&model)?);
    }
    if entries.is_empty() {
        return Err(transport(
            "Qoder returned no enabled account models. The previous list was retained.",
        ));
    }
    Ok(entries)
}
pub(crate) fn effort(asked: &str, entry: &Entry) -> (bool, String) {
    let rank = [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ];
    let asked = asked.to_ascii_lowercase();
    let mut want = if rank.contains(&asked.as_str()) {
        asked.as_str()
    } else {
        ""
    };
    if !entry.thinks || (want == "none" && !entry.always_thinks) {
        return (false, String::new());
    }
    if entry.efforts.is_empty() {
        return (true, String::new());
    }
    if want.is_empty() {
        return (true, entry.default_effort.clone());
    }
    if want == "none" {
        return (true, entry.efforts[0].clone());
    }
    if want == "ultra" && !entry.efforts.iter().any(|effort| effort == want) {
        want = "max";
    }
    if entry.efforts.iter().any(|effort| effort == want) {
        return (true, want.into());
    }
    let at = rank.iter().position(|value| *value == want).unwrap_or(0);
    let fitted = entry
        .efforts
        .iter()
        .filter_map(|effort| {
            rank.iter()
                .position(|value| *value == effort)
                .filter(|index| *index > 0)
                .map(|index| (index.abs_diff(at), std::cmp::Reverse(index), effort))
        })
        .min_by_key(|(distance, rank, _)| (*distance, *rank))
        .map(|(_, _, effort)| effort.clone())
        .unwrap_or_else(|| want.into());
    (true, fitted)
}
#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
