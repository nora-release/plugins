use super::*;
use base64::Engine;
use serde_json::json;

fn instructions(model: &str, models: &[Value]) -> String {
    for entry in models {
        crate::provider::responses_body::remember_model(entry);
    }
    crate::provider::responses_body::instructions(model)
}

fn id(model: &Value) -> Option<&str> {
    model
        .as_str()
        .or_else(|| model["id"].as_str())
        .filter(|s| !s.is_empty())
}
fn pick<'a>(models: &'a [Value], prices: &Value) -> PluginResult<(&'a str, Option<&'a str>)> {
    let eligible = models
        .iter()
        .filter(|m| id(m).is_some())
        .collect::<Vec<_>>();
    let priced = eligible
        .iter()
        .filter_map(|model| {
            let cost = model.get("cost").filter(|cost| {
                model.pointer("/info/pricingKnown") == Some(&Value::Bool(true))
                    || cost["input"].as_f64().unwrap_or(0.) + cost["output"].as_f64().unwrap_or(0.)
                        > 0.
            });
            let price = model
                .get("price")
                .filter(|price| price.is_object())
                .or(cost)
                .unwrap_or(&prices["models"][id(model)?]["price"]);
            let cost =
                price["input"].as_f64().unwrap_or(0.) + price["output"].as_f64().unwrap_or(0.);
            (cost.is_finite() && cost > 0.).then_some((*model, cost))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1));
    let selected = priced
        .map(|p| p.0)
        .or_else(|| {
            ["mini", "luna"].into_iter().find_map(|word| {
                eligible
                    .iter()
                    .copied()
                    .find(|m| id(m).is_some_and(|id| id.contains(word)))
            })
        })
        .or_else(|| eligible.first().copied())
        .ok_or_else(|| {
            PluginError::invalid("Fetch the ChatGPT model list before warming this account.")
        })?;
    let efforts = selected
        .get("reasoningEfforts")
        .or_else(|| selected.get("reasoningEffortLevels"))
        .or_else(|| selected.get("efforts"))
        .or_else(|| selected.pointer("/info/reasoningEfforts"))
        .and_then(Value::as_array);
    let effort = efforts.and_then(|values| {
        values
            .iter()
            .any(|v| v == "low")
            .then_some("low")
            .or_else(|| values.first().and_then(Value::as_str))
    });
    Ok((id(selected).unwrap(), effort))
}
fn request(
    params: &Value,
    model: &str,
    effort: Option<&str>,
    instructions: &str,
) -> PluginResult<FetchRequest> {
    let mut body = json!({
        "model":model,
        "input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}],
        "instructions":instructions,
        "store":false,"stream":true,"tool_choice":"auto","parallel_tool_calls":true,
        "text":{"verbosity":"medium"},
        // Codex's conversation key for this fixed first user message and no client instructions.
        "prompt_cache_key":"c12dc832-a353-6f7c-97db-6d52e8e861f1"
    });
    if let Some(effort) = effort {
        body["reasoning"] = json!({"effort":if effort == "ultra" { "max" } else { effort }});
        if effort != "none" {
            body["reasoning"]["summary"] = json!("auto");
        }
    }
    if effort != Some("none") {
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    let bytes = serde_json::to_vec(&body)
        .map_err(|_| PluginError::invalid("The warm-up request could not be prepared."))?;
    serde_json::from_value(json!({"account":params["account"],"credentials":params["credentials"],"networkProxy":params.get("networkProxy"),"url":"https://chatgpt.com/backend-api/codex/responses","method":"POST","headers":{"content-type":"application/json"},"body":base64::engine::general_purpose::STANDARD.encode(bytes)})).map_err(|_|PluginError::invalid("The warm-up account is invalid."))
}
async fn consume(mut response: FetchResponse) -> PluginResult<Value> {
    if !(200..300).contains(&response.status) {
        return Err(PluginError::transport(format!(
            "ChatGPT warm-up returned HTTP {}.",
            response.status
        )));
    }
    let mut size = 0;
    while let Some(chunk) = std::future::poll_fn(|cx| response.body.as_mut().poll_next(cx)).await {
        size += chunk?.len();
        if size >= 1024 * 1024 {
            break;
        }
    }
    Ok(json!({"outcome":"warmed"}))
}
impl OpenAIPlugin {
    async fn warmup_prices(&self, models: &[Value]) -> PluginResult<Value> {
        let mut seen = std::collections::BTreeSet::new();
        let queries = models
            .iter()
            .filter_map(|model| {
                let id = id(model)?;
                if !seen.insert(id) {
                    return None;
                }
                let mut query = json!({"id":id});
                if let Some(same) = model
                    .pointer("/info/canonicalModelId")
                    .and_then(Value::as_str)
                {
                    query["sameAs"] = json!(same);
                }
                Some(query)
            })
            .collect::<Vec<_>>();
        self.inner
            .call("modelMetadata", json!({"models":queries}))
            .await
    }

    pub(super) async fn warmup(&self, params: &Value) -> PluginResult<Value> {
        if params["confirmed"] != true {
            return Err(PluginError::invalid(
                "Confirm the saved account before warming its subscription allowance.",
            ));
        }
        nora_provider_plugin_sdk::require_subscription_accounts()?;
        if params["subscriptionAccountsEnabled"] == false {
            return Err(PluginError::unsupported(
                "subscription warm-up while subscriptions are disabled",
            ));
        }
        if params["account"].as_str().is_none_or(|s| {
            s.is_empty() || s.len() > 128 || s.trim() != s || s.chars().any(char::is_control)
        }) {
            return Err(PluginError::invalid("Choose a saved ChatGPT account."));
        }
        let saved = params
            .get("credentials")
            .ok_or_else(|| PluginError::authentication("Connect a ChatGPT account."))?;
        if auth::is_api(saved) {
            return Err(PluginError::unsupported(
                "subscription warm-up for API keys",
            ));
        }
        let mut models = params["models"].as_array().cloned().unwrap_or_default();
        if models.is_empty() {
            let (definition, _) = self
                .config
                .read()
                .map_err(|_| PluginError::invalid("The provider configuration is unavailable."))?
                .clone()
                .ok_or_else(|| PluginError::invalid("Initialize OpenAI before using it."))?;
            let catalog = self
                .warmup_catalog
                .read()
                .ok()
                .and_then(|path| path.clone())
                .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() <= 64 * 1024 * 1024))
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .and_then(|v| nora_provider::models::models_dev_catalog(&v).ok())
                .unwrap_or_default();
            models = nora_provider::models::catalog_models_with_catalog(
                &definition,
                Some(saved),
                &catalog,
            )
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        }
        let prices = self.warmup_prices(&models).await.unwrap_or(Value::Null);
        let (model, effort) = pick(&models, &prices)?;
        let instructions = instructions(model, &models);
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            consume(
                self.fetch(request(params, model, effort, &instructions)?)
                    .await?,
            )
            .await
        })
        .await
        .map_err(|_| PluginError::transport("ChatGPT warm-up timed out."))?
    }
}
#[cfg(test)]
#[path = "../test/warmup_request.test.rs"]
mod tests;
