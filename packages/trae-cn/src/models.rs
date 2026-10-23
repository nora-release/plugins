use crate::*;
use nora_ai::{InputModality, Model};
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub id: String,
    pub name: String,
    pub function: String,
    pub model_name: String,
    pub most: u64,
    pub context: u64,
    pub sees: bool,
    pub max: Option<(String, u64)>,
}
#[derive(Default)]
pub(crate) struct State {
    pub entries: Vec<Entry>,
    pub updated: u64,
    pub last_function: Option<String>,
}
impl Entry {
    pub fn model(&self, site: Site, credentials: &Value) -> PluginResult<Value> {
        let mut model = Model::new(&self.id, "openai-completions", site.id());
        model.base_url = format!("{}/v1", site.api(credentials));
        model.name = self.name.clone();
        model.context_window = self.context;
        model.max_tokens = self.most;
        model.reasoning = true;
        model.input = if self.sees {
            vec![InputModality::Text, InputModality::Image]
        } else {
            vec![InputModality::Text]
        };
        model.info.chat = Some(true);
        model.info.tool_calling = Some(true);
        model.info.pricing_known = Some(false);
        serde_json::to_value(model).map_err(|_| invalid("The Trae model cannot be represented."))
    }
}
pub(crate) async fn fetch(
    provider: &Trae,
    client: &dyn HttpClient,
    credentials: &Value,
    again: bool,
) -> PluginResult<Vec<Entry>> {
    let account = provider.account(credentials).await?;
    let mut state = account.lock().await;
    if !again && !state.entries.is_empty() && now().saturating_sub(state.updated) < 300_000 {
        return Ok(state.entries.clone());
    }
    let mut lists = BTreeMap::new();
    let batch=post(client,format!("{}/api/ide/v1/batch_get_detail_param",provider.site.api(credentials)),credentials,json!({"functions":site::FUNCTIONS,"agent_type":"","current_config_info":{"config_name":"","is_custom_model":false},"mode_type":0,"access_type":0,"ab_force_vids":"","ab_autotest_advanced_mode":0,"show_custom_model":true})).await;
    if let Ok((status, value)) = batch {
        let (code, _) = error_of(&value);
        if status == 401 || provider.site.lapsed(&code) {
            return Err(signed_out());
        }
        if status == 200 {
            if let Some(groups) = value
                .get("function_configs")
                .or_else(|| value.pointer("/data/function_configs"))
                .and_then(Value::as_array)
                .filter(|groups| groups.len() <= 256)
            {
                for group in groups {
                    let function = text(group, "function");
                    if !site::FUNCTIONS.contains(&function) {
                        continue;
                    }
                    if let Some(entries) = group
                        .get("config_info_list")
                        .and_then(Value::as_array)
                        .filter(|entries| entries.len() <= 4096)
                    {
                        lists
                            .entry(function.to_owned())
                            .or_insert_with(Vec::new)
                            .extend(entries.clone());
                    }
                }
            }
        }
    }
    if lists.is_empty() {
        let replies=futures::future::join_all(site::FUNCTIONS.into_iter().map(|function|async move{let result=post(client,format!("{}/api/ide/v1/get_detail_param",provider.site.api(credentials)),credentials,json!({"function":function,"config_names":null,"need_prompt":false,"current_config_info":null,"poly_prompt":true,"mode_type":null,"agent_type":null})).await;(function,result)})).await;
        let mut reached = false;
        for (function, result) in replies {
            if let Ok((status, value)) = result {
                let (code, _) = error_of(&value);
                if status == 401 || provider.site.lapsed(&code) {
                    return Err(signed_out());
                }
                if status == 200 {
                    if let Some(entries) = value
                        .get("config_info_list")
                        .or_else(|| value.pointer("/data/config_info_list"))
                        .and_then(Value::as_array)
                        .filter(|entries| entries.len() <= 4096)
                    {
                        lists.insert(function.into(), entries.clone());
                        reached = true;
                    }
                }
            }
        }
        if !reached {
            return Err(transport(
                "Trae did not return any account model lists. The previous list was retained.",
            ));
        }
    }
    let entries = merge(provider.site, &lists)?;
    state.entries = entries.clone();
    state.updated = now();
    Ok(entries)
}
fn chat(value: &Value, site: Site) -> bool {
    let id = text(value, "config_name");
    !id.is_empty()
        && id.len() <= 512
        && !id.chars().any(char::is_control)
        && !id.to_ascii_lowercase().starts_with("custom_model")
        && (text(value, "usage").is_empty() || text(value, "usage") == "chat_completion")
        && value.get("config_switch").and_then(Value::as_bool) != Some(false)
        && value.get("is_custom_model").and_then(Value::as_bool) != Some(true)
        && value
            .pointer("/display_config/is_custom_model")
            .and_then(Value::as_bool)
            != Some(true)
        && !(site == Site::Global
            && value.get("is_invisible_to_user").and_then(Value::as_bool) == Some(true))
}
fn name(value: &Value) -> &str {
    value
        .pointer("/display_config/display_name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            ["display_name", "display_model_name", "config_name"]
                .into_iter()
                .map(|key| text(value, key))
                .find(|name| !name.is_empty())
        })
        .unwrap_or("")
}
fn detail<'a>(value: &'a Value, suffix: &str) -> Option<&'a Value> {
    value
        .get("model_detail_list")
        .and_then(Value::as_array)?
        .iter()
        .find(|detail| text(detail, "model_name").ends_with(suffix))
}
fn count(value: Option<&Value>) -> u64 {
    value
        .and_then(number)
        .filter(|value| *value > 0.0)
        .map(|value| value as u64)
        .unwrap_or(0)
}
pub(crate) fn merge(site: Site, lists: &BTreeMap<String, Vec<Value>>) -> PluginResult<Vec<Entry>> {
    let mut names: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for function in site::FUNCTIONS {
        for model in lists
            .get(function)
            .into_iter()
            .flatten()
            .filter(|model| chat(model, site))
        {
            *names
                .entry(text(model, "config_name").into())
                .or_default()
                .entry(name(model).into())
                .or_default() += 1;
        }
    }
    let agreed = names
        .iter()
        .map(|(id, names)| {
            let most = names.values().copied().max().unwrap_or(0);
            let highest = names
                .iter()
                .filter(|(_, count)| **count == most)
                .collect::<Vec<_>>();
            (
                id.clone(),
                if highest.len() == 1 {
                    highest[0].0.clone()
                } else {
                    String::new()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut chosen: Vec<(String, Value, u8)> = Vec::new();
    for function in site::FUNCTIONS {
        for model in lists
            .get(function)
            .into_iter()
            .flatten()
            .filter(|model| chat(model, site))
        {
            let id = text(model, "config_name");
            let misnamed = names.get(id).is_some_and(|names| names.len() > 1)
                && lists.get(function).into_iter().flatten().any(|other| {
                    chat(other, site)
                        && text(other, "config_name") != id
                        && name(other) == name(model)
                });
            let worth = (if detail(model, "__dev").is_some() {
                2
            } else {
                0
            }) + (if misnamed { 0 } else { 1 })
                + (if agreed.get(id).map(String::as_str) == Some(name(model)) {
                    1
                } else {
                    0
                });
            if let Some((by, was, score)) = chosen
                .iter_mut()
                .find(|(_, model, _)| text(model, "config_name") == id)
            {
                if worth > *score {
                    *by = function.into();
                    *was = model.clone();
                    *score = worth;
                }
            } else {
                chosen.push((function.into(), model.clone(), worth));
            }
        }
    }
    let all_ids = chosen
        .iter()
        .map(|(_, model, _)| text(model, "config_name").to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let mut entries = Vec::new();
    for (function, model, _) in chosen {
        let id = text(&model, "config_name");
        let dev = detail(&model, "__dev");
        let sees = model
            .get("multimodal")
            .or_else(|| model.pointer("/display_config/multimodal"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let context = count(model.pointer("/context_window_tokens/dev"));
        let context = if context > 0 {
            context
        } else {
            let size = model.pointer("/context_window_size/max");
            [
                count(size.and_then(|size| {
                    size.as_array()
                        .and_then(|array| array.first())
                        .or(Some(size))
                })),
                count(model.pointer("/context_window_tokens/max")),
                count(model.get("prompt_max_tokens")),
            ]
            .into_iter()
            .find(|value| *value > 0)
            .unwrap_or(128000)
        };
        let entry = Entry {
            id: id.into(),
            name: name(&model).into(),
            function: function.clone(),
            model_name: dev
                .map(|dev| text(dev, "model_name").to_owned())
                .unwrap_or_default(),
            most: count(dev.and_then(|dev| dev.get("max_tokens"))),
            context,
            sees,
            max: None,
        };
        entries.push(entry.clone());
        if all_ids.contains(&format!("{id}-max")) {
            continue;
        }
        let mut candidates = vec![(function.clone(), model.clone())];
        for by in site::FUNCTIONS {
            for value in lists
                .get(by)
                .into_iter()
                .flatten()
                .filter(|value| chat(value, site) && text(value, "config_name") == id)
            {
                candidates.push((by.to_owned(), value.clone()));
            }
        }
        candidates
            .sort_by_key(|(_, model)| agreed.get(id).map(String::as_str) != Some(name(model)));
        if let Some((by, value)) = candidates
            .iter()
            .find(|(_, model)| detail(model, "__max").is_some())
        {
            let max_detail = detail(value, "__max").unwrap();
            let window = std::iter::once(count(value.pointer("/context_window_tokens/max")))
                .chain(
                    candidates
                        .iter()
                        .map(|(_, model)| count(model.pointer("/context_window_tokens/max"))),
                )
                .find(|count| *count > 0)
                .unwrap_or(0);
            let dev_window = std::iter::once(count(value.pointer("/context_window_tokens/dev")))
                .chain(
                    candidates
                        .iter()
                        .map(|(_, model)| count(model.pointer("/context_window_tokens/dev"))),
                )
                .find(|count| *count > 0)
                .unwrap_or(0);
            if window > dev_window {
                let mut max = entry;
                max.id = format!("{id}-max");
                max.name.push_str(" (Max)");
                max.function = by.clone();
                max.model_name = text(max_detail, "model_name").into();
                max.most = count(max_detail.get("max_tokens"));
                max.context = window;
                max.max = Some((id.into(), window));
                entries.push(max);
            }
        }
    }
    if entries.is_empty() {
        return Err(transport(
            "Trae returned no enabled chat models. The previous list was retained.",
        ));
    }
    Ok(entries)
}
#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
