use super::{request_json, validated_url, Kind, MAX_MODELS};
use crate::{
    definition::{DecisionDiscovery, Definition},
    models,
};
use nora_ai::{HttpClient, InputModality, Model, ResolvedProviderAuth};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

pub(super) fn uses_remote_metadata(definition: &Definition) -> bool {
    matches!(
        definition.config.decision_discovery,
        Some(DecisionDiscovery::Omlx | DecisionDiscovery::MlxServe | DecisionDiscovery::Ollama)
    )
}

pub(super) async fn fetch(
    definition: &Definition,
    credentials: &Value,
    resolved: &ResolvedProviderAuth,
    client: &dyn HttpClient,
    deadline: tokio::time::Instant,
) -> Option<PluginResult<Vec<Model>>> {
    if !uses_remote_metadata(definition) {
        return None;
    }
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Some(Err(PluginError::transport(
            "The decision model list request timed out.",
        )));
    }
    let kind = definition.config.decision_discovery?;
    let base = models::effective_endpoint(definition, Some(credentials));
    let task = async {
        let mut url = validated_url(&base)?;
        let path = url.path().trim_end_matches('/');
        let path = match kind {
            DecisionDiscovery::Omlx => format!("{path}/models/status"),
            DecisionDiscovery::MlxServe => format!("{path}/models"),
            DecisionDiscovery::Ollama => {
                format!("{}/api/tags", path.strip_suffix("/v1").unwrap_or(path))
            }
            _ => unreachable!(),
        };
        url.set_path(&path);
        let payload = request_json(
            client,
            url,
            definition,
            credentials,
            resolved,
            Kind::OpenAI,
            &mut None,
        )
        .await?;
        parse(kind, &payload, definition, credentials, &base)
    };
    Some(
        tokio::time::timeout(Duration::from_secs(8).min(remaining), task)
            .await
            .map_err(|_| PluginError::transport("The decision model list request timed out."))
            .and_then(|result| result),
    )
}

fn decision(definition: &Definition, credentials: Option<&Value>, id: &str) -> Model {
    let mut model = Model::new(id, "systemone", &definition.id);
    model.context_window = 0;
    model.max_tokens = 0;
    models::configure_model(definition, credentials, &mut model);
    mark(&mut model);
    model
}

fn mark(model: &mut Model) {
    model.api = "systemone".into();
    model.info.decision = Some(true);
    model.info.chat = Some(false);
    model.info.tool_calling = Some(false);
}

pub(super) fn supplement(
    definition: &Definition,
    credentials: Option<&Value>,
    models: &mut Vec<Model>,
) {
    match definition.config.decision_discovery {
        Some(DecisionDiscovery::Jev) => {
            for model in models {
                if model
                    .id
                    .to_ascii_lowercase()
                    .split('/')
                    .any(|part| part == "jev" || part.starts_with("jev-"))
                {
                    mark(model);
                }
            }
        }
        Some(DecisionDiscovery::Bailian) => {
            if let Some(model) = models
                .iter_mut()
                .find(|model| model.id == "decision-model-preview")
            {
                mark(model);
                return;
            }
            let mut model = decision(definition, credentials, "decision-model-preview");
            model.name = "Decision model (preview)".into();
            model.info.catalog_source = Some("preset".into());
            models.push(model);
        }
        _ => {}
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OmlxEntry {
    id: String,
    model_alias: String,
    model_type: String,
    engine_type: String,
    is_hidden: bool,
    max_context_window: i64,
    model_context_length: i64,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct MlxServeEntry {
    id: String,
    max_model_len: i64,
    context_length: i64,
    capabilities: Vec<String>,
    input_modalities: Vec<String>,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct OllamaEntry {
    name: String,
    model: String,
    remote_host: String,
    capabilities: Vec<String>,
    details: OllamaDetails,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct OllamaDetails {
    context_length: i64,
}

struct Facts {
    id: String,
    context: u64,
    input: Vec<InputModality>,
}
impl Facts {
    fn new(id: String, context: u64, images: bool) -> Self {
        let mut input = vec![InputModality::Text];
        if images {
            input.push(InputModality::Image);
        }
        Self { id, context, input }
    }
}

fn rows<T: serde::de::DeserializeOwned>(payload: &Value, fields: &[&str]) -> PluginResult<Vec<T>> {
    let invalid =
        || PluginError::transport("The provider returned an invalid decision model list.");
    let value = fields
        .iter()
        .find_map(|key| payload.get(key).filter(|value| !value.is_null()))
        .ok_or_else(invalid)?;
    let rows: Vec<T> = serde_json::from_value(value.clone()).map_err(|_| invalid())?;
    if rows.len() > MAX_MODELS {
        return Err(PluginError::transport(
            "The provider returned too many decision models.",
        ));
    }
    Ok(rows)
}

fn parse(
    kind: DecisionDiscovery,
    payload: &Value,
    definition: &Definition,
    credentials: &Value,
    base: &str,
) -> PluginResult<Vec<Model>> {
    let facts: Vec<Facts> = match kind {
        DecisionDiscovery::Omlx => rows::<OmlxEntry>(payload, &["models"])?
            .into_iter()
            .filter(|entry| {
                !entry.id.is_empty()
                    && !entry.is_hidden
                    && (entry.model_type == "decision" || entry.engine_type == "decision")
            })
            .map(|entry| {
                Facts::new(
                    if entry.model_alias.is_empty() {
                        entry.id
                    } else {
                        entry.model_alias
                    },
                    positive(entry.max_context_window, entry.model_context_length),
                    true,
                )
            })
            .collect(),
        DecisionDiscovery::MlxServe => rows::<MlxServeEntry>(payload, &["data", "models"])?
            .into_iter()
            .filter(|entry| {
                entry
                    .capabilities
                    .iter()
                    .any(|capability| capability == "decisions")
            })
            .map(|entry| {
                let mut facts = Facts::new(
                    entry.id,
                    positive(entry.max_model_len, entry.context_length),
                    false,
                );
                if !entry.input_modalities.is_empty() {
                    facts.input = entry
                        .input_modalities
                        .into_iter()
                        .filter_map(|input| serde_json::from_value(Value::String(input)).ok())
                        .collect();
                }
                facts
            })
            .collect(),
        DecisionDiscovery::Ollama => {
            let cloud = validated_url(base)?.host_str() == Some("ollama.com");
            rows::<OllamaEntry>(payload, &["models"])?
                .into_iter()
                .filter(|entry| {
                    entry
                        .capabilities
                        .iter()
                        .any(|capability| capability == "decision")
                })
                .map(|entry| {
                    Facts::new(
                        if entry.model.is_empty() {
                            entry.name
                        } else {
                            entry.model
                        },
                        positive(
                            entry.details.context_length,
                            if cloud || !entry.remote_host.is_empty() {
                                65536
                            } else {
                                0
                            },
                        ),
                        entry
                            .capabilities
                            .iter()
                            .any(|capability| capability == "vision"),
                    )
                })
                .collect()
        }
        _ => unreachable!(),
    };
    let mut models = Vec::<Model>::new();
    for facts in facts {
        if facts.id.trim().is_empty()
            || facts.id.len() > 512
            || facts.id.chars().any(char::is_control)
            || models.iter().any(|model| model.id == facts.id)
        {
            continue;
        }
        let mut model = decision(definition, Some(credentials), &facts.id);
        model.context_window = facts.context;
        model.input = facts.input;
        if crate::profiles::retain_model(definition, &model) {
            models.push(model);
        }
    }
    Ok(models)
}

fn positive(first: i64, second: i64) -> u64 {
    if first > 0 {
        first as u64
    } else {
        second.max(0) as u64
    }
}
