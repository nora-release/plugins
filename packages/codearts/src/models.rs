use crate::provider::*;
use nora_ai::{HttpClient, HttpMethod, Model};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

// These exact free billing IDs are the supplier defaults used by Magpie.
// They only select the billing header before a live list has answered; they
// never populate the model picker, and a live empty list replaces them.
pub(super) const BENEFIT_DEFAULTS: [&str; 4] = [
    "glm-5.3-flash",
    "deepseek-v4.1-flash",
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro-0813",
];

#[derive(Default)]
pub(super) struct State {
    pub benefit: Option<BTreeSet<String>>,
    pub regular: Option<BTreeSet<String>>,
    pub refused: BTreeSet<String>,
    pub agent: String,
    claim_day: String,
    claim_ok: bool,
    claim_at: u64,
    pub catalog: Option<Value>,
    pub updated: u64,
    pub batches: Option<Vec<crate::usage::Batch>>,
    pub batches_at: u64,
}
pub(super) fn benefit_for(state: &State, model: &str) -> bool {
    state.benefit.as_ref().map_or_else(
        || BENEFIT_DEFAULTS.contains(&model),
        |ids| ids.contains(model),
    ) && !state.refused.contains(model)
}

pub(super) async fn fetch(
    provider: &CodeArts,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Value> {
    let account = provider.account(credentials).await?;
    let mut state = account.models.lock().await;
    refresh(provider, client, credentials, &mut state).await
}
pub(super) async fn ensure(
    provider: &CodeArts,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<()> {
    let account = provider.account(credentials).await?;
    let mut state = account.models.lock().await;
    if state.catalog.is_some() && now().saturating_sub(state.updated) < 300_000 {
        return Ok(());
    }
    refresh(provider, client, credentials, &mut state)
        .await
        .map(|_| ())
}

async fn refresh(
    provider: &CodeArts,
    client: &dyn HttpClient,
    credentials: &Value,
    state: &mut State,
) -> PluginResult<Value> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    let mut answered = false;
    if let Ok(gateway) = provider
        .get(
            client,
            credentials,
            &format!("{OPEN}/api/v1/gateway/config"),
            BTreeMap::new(),
        )
        .await
    {
        if let Some(models) = gateway
            .pointer("/result/models")
            .and_then(Value::as_array)
            .filter(|models| models.len() <= 4096)
        {
            answered = true;
            let claimed = !models.is_empty() && claim(provider, client, credentials, state).await;
            let mut benefit = BTreeSet::new();
            for model in models.iter().filter(|_| claimed) {
                if let Some(mut model) = parse_model(model, &mut seen) {
                    benefit.insert(text(&model, "id").to_owned());
                    model["benefit"] = json!(true);
                    entries.push(model);
                }
            }
            state.benefit = Some(benefit);
        }
    }
    if let Ok(builtin) = provider
        .get(
            client,
            credentials,
            &format!("{SNAP}/v1/model/builtin"),
            snap_headers(),
        )
        .await
    {
        if let Some(models) = builtin
            .get("builtinModels")
            .and_then(Value::as_array)
            .filter(|models| models.len() <= 4096)
        {
            answered = true;
            let mut regular = BTreeSet::new();
            for model in models {
                if !text(model, "model_id").is_empty() {
                    regular.insert(text(model, "model_id").into());
                }
                if let Some(model) = parse_model(model, &mut seen) {
                    entries.push(model);
                }
            }
            state.regular = Some(regular);
        }
    }
    if let Some(agent) = agent_models(provider, client, credentials, state).await {
        answered = true;
        for model in agent {
            let parameters = model.get("model_parameters").unwrap_or(&Value::Null);
            let id = if text(parameters, "model_id").is_empty() {
                text(&model, "model_id")
            } else {
                text(parameters, "model_id")
            };
            if id.is_empty() {
                continue;
            }
            let rate = credit_rate(model.get("credit").or_else(|| parameters.get("credit")));
            let position = entries.iter().position(|entry| text(entry, "id") == id);
            let position = if let Some(position) = position {
                position
            } else {
                if ["third", "custom"].contains(&text(&model, "model_type"))
                    || model.get("is_custom_model").and_then(Value::as_bool) == Some(true)
                    || model.get("display_enabled").and_then(Value::as_bool) == Some(false)
                    || parameters.get("display_enabled").and_then(Value::as_bool) == Some(false)
                    || vision(id)
                {
                    continue;
                }
                let name = if text(&model, "display_name").is_empty() {
                    text(&model, "model_name")
                } else {
                    text(&model, "display_name")
                };
                let Some(entry) = parse_model(&json!({"model_id":id,"model_name":name}), &mut seen)
                else {
                    continue;
                };
                entries.push(entry);
                state
                    .regular
                    .get_or_insert_with(BTreeSet::new)
                    .insert(id.into());
                entries.len() - 1
            };
            let entry = &mut entries[position];
            if entry.get("benefit").and_then(Value::as_bool) != Some(true) {
                if let Some(rate) = rate {
                    entry["rate"] = json!(rate);
                }
            }
            if parameters
                .get("thinking_type")
                .is_some_and(|value| !value.is_null())
            {
                entry["reasoning"] = json!(true);
            }
            for (field, key) in [("context", "context_window"), ("output", "max_tokens")] {
                if let Some(value) = parameters
                    .get(key)
                    .and_then(number)
                    .filter(|value| *value > 0.0)
                {
                    entry[field] =
                        json!((value as u64).min(if field == "output" { 65536 } else { u64::MAX }));
                }
            }
        }
    }
    if !answered || entries.is_empty() {
        return Err(PluginError::transport(
            "CodeArts did not return an account model list. The previous list was retained.",
        ));
    }
    let mut models = Vec::new();
    for entry in entries {
        let id = text(&entry, "id");
        let mut model = Model::new(id, "openai-completions", "codearts");
        model.base_url = BASE.into();
        model.name = text(&entry, "name").into();
        model.context_window = entry.get("context").and_then(Value::as_u64).unwrap_or(0);
        model.max_tokens = entry
            .get("output")
            .and_then(Value::as_u64)
            .unwrap_or(65536)
            .min(65536);
        model.reasoning = entry.get("reasoning").and_then(Value::as_bool) == Some(true);
        model.info.chat = Some(true);
        model.info.tool_calling = Some(true);
        model.info.free = Some(
            entry.get("benefit").and_then(Value::as_bool) == Some(true)
                && !state.refused.contains(id),
        );
        model.info.pricing_known = Some(false);
        let mut value = serde_json::to_value(model)
            .map_err(|_| invalid("The CodeArts model cannot be represented."))?;
        if let Some(rate) = entry.get("rate") {
            value["info"]["creditMultiplier"] = rate.clone();
        } else if value.pointer("/info/free").and_then(Value::as_bool) == Some(true) {
            value["info"]["creditMultiplier"] = json!(0);
        }
        models.push(value);
    }
    let value = json!({"models":models,"source":"remote","allowUnknownModels":false,"defaultAPI":"openai-completions","defaultBaseURL":BASE});
    state.catalog = Some(value.clone());
    state.updated = now();
    Ok(value)
}

async fn claim(
    provider: &CodeArts,
    client: &dyn HttpClient,
    credentials: &Value,
    state: &mut State,
) -> bool {
    let day = crate::usage::day(now());
    if state.claim_day == day
        && (state.claim_ok || now().saturating_sub(state.claim_at) < 3_600_000)
    {
        return state.claim_ok;
    }
    let response = provider
        .signed(
            client,
            credentials,
            HttpMethod::Post,
            &format!("{OPEN}/api/v1/benefit/claim"),
            Some(json!({})),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .await;
    let Ok(response) = response else {
        return state.claim_ok && state.claim_day == day;
    };
    let Ok(value) = json_body(response).await else {
        return false;
    };
    state.claim_day = day;
    state.claim_ok = text(&value, "error_code") == "0000";
    state.claim_at = now();
    state.claim_ok
}

async fn agent_models(
    provider: &CodeArts,
    client: &dyn HttpClient,
    credentials: &Value,
    state: &mut State,
) -> Option<Vec<Value>> {
    let headers = BTreeMap::from([
        ("content-type".into(), "application/json".into()),
        ("agent-type".into(), "AgentCenter".into()),
        ("x-language".into(), "zh-cn".into()),
    ]);
    let mut ids = Vec::new();
    if !state.agent.is_empty() {
        if let Some(models) =
            read_agent(provider, client, credentials, &state.agent, headers.clone()).await
        {
            return Some(models);
        }
        state.agent.clear();
    }
    if let Ok(value) = provider
        .get(
            client,
            credentials,
            &format!("{SNAP}/v1/agent-center/agents/useragents?offset=0&limit=100"),
            headers.clone(),
        )
        .await
    {
        let mut agents = value
            .get("agents")
            .or_else(|| value.pointer("/data/agents"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        agents.sort_by_key(|agent| {
            match if text(agent, "original_id").is_empty() {
                text(agent, "agent_id")
            } else {
                text(agent, "original_id")
            } {
                "build" => 0,
                "CodeAgent" => 1,
                _ => 2,
            }
        });
        ids.extend(agents.into_iter().filter_map(|agent| {
            agent
                .get("agent_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 512)
                .map(str::to_owned)
        }));
    }
    ids.push("a8bcb36232554267a5142361cc25a393".into());
    let mut seen = BTreeSet::new();
    for id in ids.into_iter().filter(|id| seen.insert(id.clone())).take(3) {
        if let Some(models) = read_agent(provider, client, credentials, &id, headers.clone()).await
        {
            state.agent = id;
            return Some(models);
        }
    }
    None
}
async fn read_agent(
    provider: &CodeArts,
    client: &dyn HttpClient,
    credentials: &Value,
    id: &str,
    headers: BTreeMap<String, String>,
) -> Option<Vec<Value>> {
    let url = format!(
        "{SNAP}/v1/agent-center/agents/detail?agent_id={}",
        crate::sign::encode(id)
    );
    let value = provider
        .get(client, credentials, &url, headers)
        .await
        .ok()?;
    value
        .pointer("/gpts/models")
        .or_else(|| value.pointer("/data/gpts/models"))
        .and_then(Value::as_array)
        .filter(|models| !models.is_empty() && models.len() <= 4096)
        .cloned()
}

pub(super) fn parse_model(value: &Value, seen: &mut BTreeSet<String>) -> Option<Value> {
    let id = text(value, "model_id");
    if id.is_empty()
        || id.len() > 512
        || id.chars().any(char::is_control)
        || vision(id)
        || !seen.insert(id.into())
    {
        return None;
    }
    let base = normalize(id);
    let name = if text(value, "model_name").is_empty() {
        base
    } else {
        normalize(text(value, "model_name"))
    };
    let context = value
        .get("context_window")
        .and_then(number)
        .filter(|value| *value > 0.0)
        .map(|value| value as u64)
        .unwrap_or(match base {
            "GLM-5.2" => 202752,
            "glm-5.3-flash" | "deepseek-v4-flash" | "deepseek-v4-pro" => 1048576,
            "deepseek-v4.1-flash" => 1000000,
            _ => 0,
        });
    let output = value
        .get("max_tokens")
        .and_then(number)
        .filter(|value| *value > 0.0)
        .map(|value| (value as u64).min(65536));
    Some(json!({"id":id,"name":name,"context":context,"output":output}))
}
fn vision(id: &str) -> bool {
    id.to_ascii_lowercase()
        .split('-')
        .any(|piece| piece == "vl")
}
pub(super) fn normalize(id: &str) -> &str {
    if id.len() > 5
        && id.as_bytes()[id.len() - 5] == b'-'
        && id.as_bytes()[id.len() - 4..].iter().all(u8::is_ascii_digit)
    {
        &id[..id.len() - 5]
    } else {
        id
    }
}
pub(super) fn credit_rate(credit: Option<&Value>) -> Option<f64> {
    let credit = credit?;
    let tier = credit
        .as_array()
        .and_then(|credit| credit.first())
        .unwrap_or(credit);
    let value = tier
        .get("ratio_display")
        .or_else(|| tier.get("ratioDisplay"))?;
    let rate = number(value).or_else(|| {
        value
            .as_str()?
            .trim()
            .trim_end_matches('x')
            .parse::<f64>()
            .ok()
    })?;
    (rate.is_finite() && rate >= 0.0).then_some(rate)
}

#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
