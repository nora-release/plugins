use crate::definition::{Definition, ModelBase, ModelDiscovery};
use nora_ai::{JsonValue, Model, OAuthCredentials};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub bases: Vec<ModelBase>,
    #[serde(default)]
    pub models: Vec<Value>,
    #[serde(default)]
    pub catalog: String,
    #[serde(default)]
    pub no_list: bool,
    #[serde(default)]
    pub key_optional: bool,
    #[serde(default)]
    pub custom_endpoint: bool,
    #[serde(default)]
    pub endpoint_required: bool,
    #[serde(default)]
    pub model_routes: Vec<ModelRoute>,
    #[serde(default)]
    pub only: Vec<String>,
    #[serde(default)]
    pub api_key_auth: Option<nora_ai::AuthScheme>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRoute {
    pub pattern: String,
    pub api: String,
}

fn invalid() -> PluginError {
    PluginError::invalid("The provider's account profile is invalid or no longer available.")
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}
fn endpoint(value: &str) -> PluginResult<url::Url> {
    let url = url::Url::parse(value).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url)
}
pub(crate) fn validate(definition: &Definition) -> PluginResult<()> {
    if definition.config.profiles.len() > 24 {
        return Err(invalid());
    }
    let mut ids = BTreeSet::new();
    for profile in &definition.config.profiles {
        if !valid_id(&profile.id)
            || !ids.insert(&profile.id)
            || profile.name.trim().is_empty()
            || profile.name.len() > 256
            || profile.bases.is_empty()
            || profile.bases.len() > 5
            || profile.models.len() > 4096
            || profile.only.len() > 32
        {
            return Err(invalid());
        }
        for base in &profile.bases {
            endpoint(&base.base_url)?;
            if !supported(&base.api) {
                return Err(invalid());
            }
        }
        for rule in &profile.model_routes {
            if rule.pattern.len() > 256
                || !profile.bases.iter().any(|base| base.api == rule.api)
                || globset::Glob::new(&rule.pattern).is_err()
            {
                return Err(invalid());
            }
        }
        if profile.model_routes.len() > 64
            || profile
                .only
                .iter()
                .any(|prefix| prefix.is_empty() || prefix.len() > 256)
        {
            return Err(invalid());
        }
    }
    Ok(())
}
fn supported(api: &str) -> bool {
    matches!(
        api,
        "openai-completions"
            | "openai-responses"
            | "anthropic-messages"
            | "google-generative-ai"
            | "systemone"
    )
}
pub(crate) fn by_method<'a>(definition: &'a Definition, method: &str) -> Option<&'a Profile> {
    if method == "apiKey" {
        return definition.config.profiles.first();
    }
    method.strip_prefix("profile-").and_then(|id| {
        definition
            .config
            .profiles
            .iter()
            .find(|profile| profile.id == id)
    })
}
pub(crate) fn assign(
    definition: &Definition,
    method: &str,
    inputs: &Value,
    credentials: &mut OAuthCredentials,
) -> PluginResult<()> {
    if definition.config.profiles.is_empty() {
        return Ok(());
    }
    let profile = by_method(definition, method).ok_or_else(invalid)?;
    credentials
        .extras
        .insert("profile".into(), JsonValue::string(&profile.id));
    credentials
        .extras
        .insert("plan".into(), JsonValue::string(&profile.name));
    if profile.custom_endpoint {
        let supplied = inputs
            .get("endpoint")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if profile.endpoint_required && supplied.is_none() {
            return Err(PluginError::invalid("Enter the address of your provider."));
        }
        if let Some(value) = supplied {
            endpoint(value)?;
            credentials.extras.insert(
                "endpoint".into(),
                JsonValue::string(value.trim_end_matches('/')),
            );
        }
    }
    Ok(())
}
pub(crate) fn configured(
    mut definition: Definition,
    value: Option<&Value>,
) -> PluginResult<Definition> {
    if definition.config.profiles.is_empty() {
        return Ok(definition);
    }
    let selected = value
        .and_then(|value| {
            value
                .get("profile")
                .or_else(|| value.pointer("/extras/profile"))
        })
        .and_then(Value::as_str);
    let selected = selected.or_else(|| value.and(definition.config.legacy_profile.as_deref()));
    let profile = if let Some(id) = selected {
        definition
            .config
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .ok_or_else(invalid)?
    } else {
        &definition.config.profiles[0]
    }
    .clone();
    let mut bases = profile.bases;
    if profile.custom_endpoint {
        if profile.endpoint_required
            && value.is_some()
            && value
                .and_then(|value| {
                    value
                        .get("endpoint")
                        .or_else(|| value.pointer("/extras/endpoint"))
                })
                .and_then(Value::as_str)
                .is_none()
        {
            return Err(PluginError::authentication(
                "Reconnect this account with its provider address.",
            ));
        }
        if let Some(value) = value
            .and_then(|value| {
                value
                    .get("endpoint")
                    .or_else(|| value.pointer("/extras/endpoint"))
            })
            .and_then(Value::as_str)
        {
            let supplied = endpoint(value)?;
            let default = endpoint(&bases[0].base_url)?;
            for base in &mut bases {
                let original = endpoint(&base.base_url)?;
                if original.origin() != default.origin() {
                    return Err(invalid());
                }
                let prefix = default.path().trim_end_matches('/');
                let suffix = original.path().strip_prefix(prefix).ok_or_else(invalid)?;
                let mut target = supplied.clone();
                target.set_path(&format!(
                    "{}{suffix}",
                    supplied.path().trim_end_matches('/')
                ));
                base.base_url = target.to_string().trim_end_matches('/').into();
            }
        }
    }
    definition.base_url = bases[0].base_url.clone();
    definition.api = bases[0].api.clone();
    if !profile.catalog.is_empty() {
        definition.config.catalog = profile.catalog;
    }
    definition.models = profile.models.clone();
    definition.config.model_definitions = Some(profile.models);
    definition.config.model_discovery = Some(ModelDiscovery {
        default_base_url: definition.base_url.clone(),
        bases,
        no_list: profile.no_list,
    });
    definition.config.model_routes = profile.model_routes;
    definition.config.only = profile.only;
    definition.config.key_optional = profile.key_optional;
    if let Some(auth) = profile.api_key_auth {
        definition.config.api_key_auth = Some(auth);
    }
    Ok(definition)
}
pub(crate) fn route_model(definition: &Definition, model: &mut Model) {
    for rule in &definition.config.model_routes {
        if !globset::GlobBuilder::new(&rule.pattern)
            .case_insensitive(true)
            .build()
            .ok()
            .is_some_and(|pattern| pattern.compile_matcher().is_match(&model.id))
        {
            continue;
        }
        let Some(base) = definition
            .config
            .model_discovery
            .as_ref()
            .and_then(|discovery| discovery.bases.iter().find(|base| base.api == rule.api))
        else {
            continue;
        };
        model.api = base.api.clone();
        model.base_url = base.base_url.clone();
        return;
    }
}
pub(crate) fn retain_model(definition: &Definition, model: &Model) -> bool {
    if !definition.config.only.is_empty()
        && !definition
            .config
            .only
            .iter()
            .any(|prefix| model.id.starts_with(prefix))
    {
        return false;
    }
    if !definition.config.profiles.is_empty()
        && definition
            .config
            .model_discovery
            .as_ref()
            .is_some_and(|discovery| discovery.no_list)
    {
        return definition.models.iter().any(|value| {
            value
                .as_str()
                .or_else(|| value.get("id").and_then(Value::as_str))
                == Some(model.id.as_str())
        });
    }
    true
}

#[cfg(test)]
#[path = "../test/profiles.test.rs"]
mod tests;
