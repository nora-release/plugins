use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Definition {
    pub id: String,
    pub name: String,
    pub api: String,
    #[serde(default, rename = "baseURL")]
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<Value>,
    #[serde(default)]
    pub auth_methods: Vec<Value>,
    pub config: Config,
    #[serde(skip)]
    pub(crate) request_account: Option<String>,
    #[serde(skip, default = "crate::adapter::generic")]
    pub adapter: std::sync::Arc<dyn crate::ProviderAdapter>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DecisionDiscovery {
    Omlx,
    MlxServe,
    Ollama,
    Jev,
    Bailian,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub pass_user_agent: bool,
    /// Client-specific gateways sign requests as their own application.
    #[serde(default)]
    pub own_client: bool,
    #[serde(default, rename = "modelsURL")]
    pub models_url: String,
    #[serde(default, rename = "balanceURL")]
    pub balance_url: String,
    #[serde(default)]
    pub balance_path: String,
    #[serde(default)]
    pub balance_method: String,
    #[serde(default)]
    pub balance_token: String,
    #[serde(default, rename = "insecureTLS")]
    pub insecure_tls: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
    #[serde(default)]
    pub auth_header: String,
    #[serde(default)]
    pub header_hints: Vec<String>,
    #[serde(default)]
    pub forwarded_headers: Vec<String>,
    #[serde(default)]
    pub legacy_profile: Option<String>,
    #[serde(default)]
    pub profiles: Vec<crate::profiles::Profile>,
    #[serde(default)]
    pub model_routes: Vec<crate::profiles::ModelRoute>,
    #[serde(default)]
    pub only: Vec<String>,
    #[serde(default)]
    pub key_optional: bool,

    #[serde(default)]
    pub catalog: String,
    #[serde(default)]
    pub model_discovery: Option<ModelDiscovery>,
    #[serde(default)]
    pub decision_discovery: Option<DecisionDiscovery>,
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
    #[serde(default, rename = "baseURL")]
    pub base_url: String,
}

impl Definition {
    /// The generic HTTP adapter's view of a package manifest: its identity
    /// and its `contributes.provider`.
    pub fn parse(value: Value) -> PluginResult<Self> {
        let mut provider = value
            .pointer("/contributes/provider")
            .cloned()
            .ok_or_else(|| PluginError::invalid("The plugin manifest contributes no provider."))?;
        if let Some(provider) = provider.as_object_mut() {
            provider.insert("id".into(), value["id"].clone());
            provider.insert("name".into(), value["name"].clone());
        }
        Self::from_fields(provider)
    }

    /// A definition from its own fields: the provider declaration with the
    /// package's `id` and `name`.
    pub fn from_fields(provider: Value) -> PluginResult<Self> {
        let definition: Self = serde_json::from_value(provider)
            .map_err(|_| PluginError::invalid("The provider manifest configuration is invalid."))?;
        if definition.id.is_empty()
            || definition.name.trim().is_empty()
            || definition.config.key_provider.is_empty()
        {
            return Err(PluginError::invalid(
                "The provider manifest requires its identity and key provider.",
            ));
        }
        crate::profiles::validate(&definition)?;
        crate::account_headers::validate(&definition)?;
        crate::endpoint_overrides::validate(&definition)?;
        Ok(definition)
    }
}

pub fn validate_package(value: &Value) -> Result<String, String> {
    Ok(crate::package_manifest::parse(value)?.id)
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
                .join(nora_plugin_manifest::package::MANIFEST_FILE);
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
        let id = validate_package(&value)?;
        crate::build_capabilities::declare(&mut value);
        let directory = output.join(id);
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        std::fs::write(
            directory.join(nora_plugin_manifest::package::MANIFEST_FILE),
            serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../test/definition.test.rs"]
mod tests;

#[cfg(test)]
#[path = "../test/upstream_status.test.rs"]
mod upstream_status_tests;
