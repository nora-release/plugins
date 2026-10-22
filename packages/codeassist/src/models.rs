use super::*;
use nora_ai::{InputModality, Model, ModelsCatalog};
use std::collections::{BTreeMap, BTreeSet};

const LEVELS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];
#[derive(Clone, Default)]
pub(crate) struct Flags {
    pro31: bool,
    no_pro: bool,
    flash: bool,
    lite: bool,
}
impl Flags {
    pub fn parse(value: &Value) -> Self {
        let enabled = |id| {
            value["flags"].as_array().is_some_and(|flags| {
                flags
                    .iter()
                    .any(|flag| flag["flagId"] == id && flag["boolValue"] == true)
            })
        };
        Self {
            pro31: enabled(json!(45760185)),
            no_pro: enabled(json!(45768879)),
            flash: enabled(json!(45842815)),
            lite: enabled(json!(45827489)),
        }
    }
    pub fn wire(&self, id: &str) -> String {
        match id {
            "gemini-3.5-flash" | "gemini-3-flash" if self.flash => "gemini-3.8-flash",
            "gemini-3.5-flash" | "gemini-3.8-flash" if !self.flash => "gemini-3-flash",
            "gemini-3.1-flash-lite" if self.lite => "gemini-3.5-flash-lite",
            "gemini-3.5-flash-lite" if !self.lite => "gemini-3.1-flash-lite",
            _ => id,
        }
        .into()
    }
    pub fn shown(&self, id: &str) -> String {
        if id == "gemini-3-flash" {
            return if self.flash {
                "gemini-3.8-flash"
            } else {
                "gemini-3.5-flash"
            }
            .into();
        }
        id.into()
    }
    pub fn offered(&self, quota: &Value) -> Vec<Model> {
        let preview = quota["buckets"].as_array().is_some_and(|rows| {
            rows.iter().any(|row| {
                row["modelId"]
                    .as_str()
                    .is_some_and(|id| id.contains("-preview"))
            })
        });
        let known = ModelsCatalog::bundled().unwrap_or_default();
        let ids = [
            "gemini-3.1-pro-preview",
            "gemini-3-pro-preview",
            "gemini-3-flash-preview",
            "gemini-3.8-flash",
            "gemini-3.5-flash",
            "gemini-3.5-flash-lite",
            "gemini-3.1-flash-lite",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
        ];
        ids.into_iter()
            .filter(|id| match *id {
                "gemini-3.1-pro-preview" => self.pro31 && preview && !self.no_pro,
                "gemini-3-pro-preview" => !self.pro31 && preview && !self.no_pro,
                "gemini-3-flash-preview" => preview,
                "gemini-3.8-flash" => self.flash,
                "gemini-3.5-flash" => !self.flash,
                "gemini-3.5-flash-lite" => self.lite,
                "gemini-3.1-flash-lite" => !self.lite,
                "gemini-2.5-pro" => !self.no_pro,
                "gemini-2.5-flash" => !self.flash,
                _ => true,
            })
            .map(|id| {
                let mut model = known.model("google", id).cloned().unwrap_or_else(|| {
                    let mut model = Model::new(id, "google-generative-ai", App::GeminiCLI.id());
                    model.context_window = 0;
                    model.max_tokens = 0;
                    model
                });
                model.provider = App::GeminiCLI.id().into();
                model.api = "google-generative-ai".into();
                model.base_url = PROD.into();
                model.cost = Default::default();
                model.input = vec![InputModality::Text, InputModality::Image];
                model.info.tool_calling = Some(true);
                model.reasoning = true;
                if model.info.reasoning_efforts.is_empty() {
                    model.info.reasoning_efforts =
                        vec!["low".into(), "medium".into(), "high".into()];
                }
                model
            })
            .collect()
    }
}

#[derive(Clone, Default)]
pub(crate) struct Catalog {
    pub models: Vec<Model>,
    pub variants: BTreeMap<String, BTreeMap<String, String>>,
}
impl Catalog {
    pub fn wire(&self, asked: &str, effort: Option<&str>) -> String {
        let family = self.variants.get(asked).or_else(|| {
            self.variants
                .values()
                .find(|levels| levels.values().any(|id| id == asked))
        });
        let Some(family) = family else {
            return asked.into();
        };
        if let Some(effort) = effort {
            if let Some(id) = family.get(effort) {
                return id.clone();
            }
        }
        if effort.is_none() && family.values().any(|id| id == asked) {
            return asked.into();
        }
        if let Some(want) = effort.and_then(|want| LEVELS.iter().position(|level| *level == want)) {
            let nearest = family
                .iter()
                .filter(|(level, _)| !level.is_empty())
                .min_by_key(|(level, _)| {
                    LEVELS
                        .iter()
                        .position(|item| *item == level.as_str())
                        .unwrap_or(4)
                        .abs_diff(want)
                });
            if let Some((_, id)) = nearest {
                return id.clone();
            }
        }
        family.get("").cloned().unwrap_or_else(|| asked.into())
    }
}
fn split(id: &str) -> (&str, &str) {
    for (word, level) in [
        ("extra-high", "xhigh"),
        ("extra-low", "minimal"),
        ("xhigh", "xhigh"),
        ("minimal", "minimal"),
        ("low", "low"),
        ("medium", "medium"),
        ("high", "high"),
    ] {
        if let Some(base) = id.strip_suffix(&format!("-{word}")) {
            if !base.is_empty() {
                return (base, level);
            }
        }
    }
    (id, "")
}
fn named(name: &str) -> (&str, &str) {
    let Some((base, level)) = name.rsplit_once(" (") else {
        return (name, "");
    };
    let Some(level) = level.strip_suffix(')') else {
        return (name, "");
    };
    let level = match level.to_ascii_lowercase().as_str() {
        "minimal" | "extra low" => "minimal",
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "extra high" | "xhigh" => "xhigh",
        _ => return (name, ""),
    };
    (base, level)
}
pub(crate) fn visible(id: &str) -> bool {
    !id.starts_with("chat_")
        && !id.starts_with("tab_")
        && !["gemini-2.5-flash-thinking", "gemini-2.5-pro"].contains(&id)
}
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

pub(crate) fn antigravity(payload: &Value) -> PluginResult<Catalog> {
    let raw = payload["models"]
        .as_object()
        .filter(|rows| !rows.is_empty() && rows.len() <= 4096)
        .ok_or_else(|| PluginError::transport("Antigravity returned no model catalog."))?;
    let order = payload["agentModelSorts"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|sort| sort["groups"].as_array().into_iter().flatten())
        .flat_map(|group| group["modelIds"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .enumerate()
        .map(|(index, id)| (id, index))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    for (id, row) in raw {
        if !valid_id(id) || !visible(id) {
            continue;
        }
        let mut model = Model::new(id, "google-generative-ai", App::Antigravity.id());
        model.base_url = App::Antigravity.base().into();
        model.name = text(row.get("displayName"), 512).unwrap_or(id).into();
        let own_enum = format!(
            "MODEL_GOOGLE_{}",
            id.replace(['-', '.'], "_").to_ascii_uppercase()
        );
        if row["model"].as_str() == Some(&own_enum)
            && named(&model.name)
                .0
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("-")
                .to_ascii_lowercase()
                != split(id).0.to_ascii_lowercase()
        {
            model.name = id.clone();
        }
        model.context_window = row["maxTokens"].as_u64().unwrap_or(0);
        model.max_tokens = row["maxOutputTokens"].as_u64().unwrap_or(0);
        if row["supportsImages"]
            .as_bool()
            .unwrap_or(!id.starts_with("gpt-oss"))
        {
            model.input.push(InputModality::Image);
        }
        model.reasoning = row["supportsThinking"]
            .as_bool()
            .unwrap_or(id.contains("thinking") || id.starts_with("gemini"));
        model.info.tool_calling = Some(true);
        rows.push(model);
    }
    rows.sort_by_key(|model| {
        (
            order.get(model.id.as_str()).copied().unwrap_or(usize::MAX),
            model.id.clone(),
        )
    });
    let mut groups: Vec<(String, Vec<Model>)> = Vec::new();
    for model in rows {
        let family = split(&model.id).0.to_owned();
        if let Some((_, models)) = groups.iter_mut().find(|(id, _)| *id == family) {
            models.push(model);
        } else {
            groups.push((family, vec![model]));
        }
    }
    let mut catalog = Catalog::default();
    for (family, variants) in groups {
        if variants.len() == 1 {
            catalog.models.extend(variants);
            continue;
        }
        let names = variants
            .iter()
            .map(|model| named(&model.name).1)
            .collect::<BTreeSet<_>>();
        let use_names = !names.contains("") && names.len() == variants.len();
        let mut levels = BTreeMap::new();
        for model in &variants {
            let level = if use_names {
                named(&model.name).1
            } else {
                split(&model.id).1
            };
            levels
                .entry(level.to_owned())
                .or_insert_with(|| model.id.clone());
        }
        if !levels.contains_key("") {
            let default = levels
                .get("high")
                .cloned()
                .or_else(|| {
                    LEVELS
                        .iter()
                        .rev()
                        .find_map(|level| levels.get(*level).cloned())
                })
                .unwrap_or_else(|| variants[0].id.clone());
            levels.insert("".into(), default);
        }
        let mut model = variants
            .iter()
            .find(|model| Some(&model.id) == levels.get(""))
            .unwrap_or(&variants[0])
            .clone();
        model.id = family.clone();
        model.name = named(&model.name).0.into();
        model.context_window = variants
            .iter()
            .map(|model| model.context_window)
            .filter(|n| *n > 0)
            .min()
            .unwrap_or(0);
        model.max_tokens = variants
            .iter()
            .map(|model| model.max_tokens)
            .filter(|n| *n > 0)
            .min()
            .unwrap_or(0);
        if variants
            .iter()
            .any(|model| !model.input.contains(&InputModality::Image))
        {
            model.input = vec![InputModality::Text];
        }
        model.info.reasoning_efforts = LEVELS
            .into_iter()
            .filter(|level| levels.contains_key(*level))
            .map(str::to_owned)
            .collect();
        model.reasoning = !model.info.reasoning_efforts.is_empty();
        catalog.variants.insert(family, levels);
        catalog.models.push(model);
    }
    let base_models = catalog
        .models
        .iter()
        .map(|model| (model.id.clone(), model.clone()))
        .collect::<BTreeMap<_, _>>();
    for model in &mut catalog.models {
        if let Some(base) = model
            .id
            .strip_suffix("-tiered")
            .and_then(|id| base_models.get(id))
        {
            if model.info.reasoning_efforts.is_empty() {
                model.info.reasoning_efforts = base.info.reasoning_efforts.clone();
                model.reasoning = !model.info.reasoning_efforts.is_empty();
            }
            if model.name == model.id {
                model.name = format!("{} (Tiered)", base.name);
            }
        }
    }
    if catalog.models.is_empty() {
        return Err(PluginError::transport("Antigravity listed no chat models."));
    }
    Ok(catalog)
}

pub(crate) fn encode(app: App, models: Vec<Model>) -> Value {
    json!({"models":models,"source":"remote","allowUnknownModels":false,"defaultAPI":"google-generative-ai","defaultBaseURL":app.base()})
}
#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
