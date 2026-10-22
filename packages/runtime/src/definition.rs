use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Definition {
    pub id: String,
    pub name: String,
    pub api: String,
    #[serde(rename = "baseURL")]
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<Value>,
    #[serde(default)]
    pub auth_methods: Vec<Value>,
    pub config: Config,
    #[serde(skip, default = "crate::adapter::generic")]
    pub adapter: std::sync::Arc<dyn crate::ProviderAdapter>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub catalog: String,
    #[serde(default)]
    pub model_discovery: Option<ModelDiscovery>,
    pub key_provider: String,
    #[serde(default)]
    pub api_key_auth: Option<nora_ai::AuthScheme>,
    pub oauth_provider: Option<String>,
    #[serde(default)]
    pub source_auth_methods: Vec<Value>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub query_params: BTreeMap<String, String>,
    #[serde(default)]
    pub provider_options: BTreeMap<String, Value>,
    #[serde(default)]
    pub endpoint_kind: Option<String>,
    #[serde(default)]
    pub legacy: Value,
    #[serde(default)]
    pub model_definitions: Option<Vec<Value>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDiscovery {
    #[serde(default, rename = "defaultBaseURL")]
    pub default_base_url: String,
    #[serde(default)]
    pub bases: Vec<ModelBase>,
    #[serde(default)]
    pub no_list: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelBase {
    pub api: String,
    #[serde(rename = "baseURL")]
    pub base_url: String,
}

impl Definition {
    pub fn parse(value: Value) -> PluginResult<Self> {
        let definition: Self = serde_json::from_value(value)
            .map_err(|_| PluginError::invalid("The provider manifest configuration is invalid."))?;
        if definition.id.is_empty()
            || definition.name.trim().is_empty()
            || definition.config.key_provider.is_empty()
        {
            return Err(PluginError::invalid(
                "The provider manifest requires its identity and key provider.",
            ));
        }
        Ok(definition)
    }
}

pub fn prepare(source: &Path, output: &Path) -> Result<(), String> {
    std::fs::create_dir_all(output).map_err(|error| error.to_string())?;
    let mut definitions = Vec::new();
    if source.is_file() {
        definitions.push(source.to_path_buf());
    } else {
        for entry in std::fs::read_dir(source).map_err(|error| error.to_string())? {
            let path = entry
                .map_err(|error| error.to_string())?
                .path()
                .join("nora-provider.json");
            if path.is_file() {
                definitions.push(path);
            }
        }
    }
    definitions.sort();
    if definitions.is_empty() {
        return Err("No provider manifests were found.".into());
    }
    for path in definitions {
        let mut value: Value =
            serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let definition = Definition::parse(value.clone()).map_err(|error| error.to_string())?;
        value["config"]["modelDefinitions"] = json!(definition.models);
        value["models"] = json!(definition.models);
        let directory = output.join(&definition.id);
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        std::fs::write(
            directory.join("nora-provider.json"),
            serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}
