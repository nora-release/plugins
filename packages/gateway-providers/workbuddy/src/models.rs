use super::*;
use nora_plugin_runtime_sdk::provider::{Cost, Model};
use std::collections::BTreeSet;

pub(super) async fn fetch(
    provider: &WorkBuddy,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Vec<Model>> {
    let mut headers = request::headers(provider.site, credentials);
    headers.insert("user-agent".into(), "CLI/2.0.0 WorkBuddy/5.5.6".into());
    let value = envelope(
        send(
            client,
            HttpRequest {
                method: HttpMethod::Get,
                url: format!("{}/v3/config", provider.site.endpoint()),
                headers,
                body: None,
            },
            Duration::from_secs(15),
        )
        .await?,
    )
    .await?;
    parse(provider.site, &value, now())
}

/// The models the account's CLI agent offers, as the router lists them. A
/// model free all day costs nothing; WorkBuddy's credit rates are not token
/// prices, so the others are left to models.dev.
pub(super) fn parse(site: Site, config: &Value, at: u64) -> PluginResult<Vec<Model>> {
    let agents = config
        .get("agents")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("WorkBuddy returned no account model list."))?;
    let details = config
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("WorkBuddy returned no model descriptions."))?;
    if agents.len() > 512 || details.len() > 4096 {
        return Err(invalid("The WorkBuddy model list is too large."));
    }
    let mut known = BTreeSet::new();
    let mut names = BTreeMap::new();
    let mut models = Vec::new();
    for agent in agents.iter().filter(|agent| text(agent, "name") == "cli") {
        for id in agent
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if id.is_empty()
                || id.len() > 256
                || id.chars().any(char::is_control)
                || !known.insert(id)
            {
                continue;
            }
            let detail = details
                .iter()
                .find(|detail| text(detail, "id") == id)
                .ok_or_else(|| invalid("WorkBuddy listed a model without its capabilities."))?;
            let mut name = if text(detail, "name").is_empty() {
                id.to_owned()
            } else {
                text(detail, "name").to_owned()
            };
            if let Some(first) = names.get(&name) {
                let suffix = id
                    .strip_prefix(&format!("{first}-"))
                    .map(str::to_uppercase)
                    .unwrap_or_else(|| id.into());
                name = format!("{name} ({suffix})");
            } else {
                names.insert(name.clone(), id.to_owned());
            }
            let (_, _, free) = price(
                detail,
                config.get("modelPromotions").unwrap_or(&Value::Null),
                at,
            );
            models.push(Model {
                name: Some(name),
                context: detail.get("maxInputTokens").and_then(Value::as_u64),
                output: detail.get("maxOutputTokens").and_then(Value::as_u64),
                images: detail.get("supportsImages").and_then(Value::as_bool) == Some(true)
                    && !(site == Site::International && id.starts_with("glm-")),
                reasoning: !efforts(detail).is_empty(),
                cost: (free == Some(true)).then(Cost::default),
                ..Model::new(id)
            });
        }
    }
    if models.is_empty() {
        return Err(invalid("WorkBuddy returned an empty account model list."));
    }
    Ok(models)
}

fn efforts(detail: &Value) -> Vec<String> {
    let supported = detail
        .pointer("/reasoning/supportedEfforts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|effort| ["low", "medium", "high", "xhigh", "max"].contains(effort))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut efforts = if !supported.is_empty() {
        supported
    } else if detail.get("supportsReasoning").and_then(Value::as_bool) == Some(true) {
        ["low", "medium", "high", "xhigh", "max"]
            .into_iter()
            .filter(|effort| {
                ["low", "high", "max"].contains(effort)
                    || *effort
                        == detail
                            .pointer("/reasoning/effort")
                            .and_then(Value::as_str)
                            .unwrap_or("")
            })
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };
    if !efforts.is_empty()
        && detail.get("onlyReasoning").and_then(Value::as_bool) != Some(true)
        && detail
            .pointer("/reasoning/canDisableThinking")
            .and_then(Value::as_bool)
            != Some(false)
    {
        efforts.insert(0, "none".into());
    }
    efforts
}

pub(super) fn credits(value: &Value) -> Option<f64> {
    if let Some(value) = value.as_f64() {
        return (value.is_finite() && value >= 0.0).then_some(value);
    }
    let value = value.as_str()?.trim().to_ascii_lowercase();
    let value = value
        .strip_suffix("credits")
        .or_else(|| value.strip_suffix("credit"))
        .unwrap_or(&value)
        .trim()
        .trim_start_matches('x')
        .trim_end_matches('x');
    if value.is_empty() {
        return None;
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

pub(super) fn price(
    detail: &Value,
    promotions: &Value,
    at: u64,
) -> (Option<f64>, Option<f64>, Option<bool>) {
    let base = detail.get("credits").and_then(credits);
    let mine = promotions
        .as_array()
        .into_iter()
        .flatten()
        .filter(|promotion| {
            text(promotion, "kind") == "discount"
                && promotion.get("enabled").and_then(Value::as_bool) != Some(false)
                && promotion
                    .get("modelIds")
                    .and_then(Value::as_array)
                    .is_some_and(|models| {
                        models
                            .iter()
                            .any(|model| model.as_str() == Some(text(detail, "id")))
                    })
                && promotion
                    .pointer("/schedule/validFrom")
                    .and_then(Value::as_str)
                    .and_then(date)
                    .is_none_or(|start| start <= at)
                && promotion
                    .pointer("/schedule/validUntil")
                    .and_then(Value::as_str)
                    .and_then(date)
                    .is_none_or(|until| until > at)
        })
        .collect::<Vec<_>>();
    if mine.iter().any(|promotion| {
        promotion
            .pointer("/schedule/daily")
            .and_then(Value::as_array)
            .is_some_and(|daily| !daily.is_empty())
    }) {
        return (base.filter(|rate| *rate > 0.0), None, Some(false));
    }
    let discounted = mine
        .iter()
        .filter_map(|promotion| {
            promotion
                .pointer("/discount/discountedCredits")
                .and_then(credits)
                .or_else(|| {
                    promotion
                        .pointer("/discount/factor")
                        .and_then(number)
                        .filter(|factor| *factor >= 0.0)
                        .zip(base)
                        .map(|(factor, base)| factor * base)
                })
        })
        .reduce(f64::min);
    let rate = discounted.or(base);
    let previous = base
        .zip(discounted)
        .filter(|(base, rate)| base > rate)
        .map(|(base, _)| base);
    (rate, previous, rate.map(|rate| rate == 0.0))
}

pub(super) fn date(text: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.timestamp_millis())
        .filter(|time| *time > 0)
        .map(|time| time as u64)
}

#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
