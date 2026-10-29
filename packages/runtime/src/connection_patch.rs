use crate::definition::{Definition, ModelBase, ModelDiscovery};
use nora_ai::AuthScheme;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::Value;

/// Account profiles choose vendor defaults first. Only settings explicitly
/// supplied by the host may replace that profile's address or key scheme.
pub(crate) fn apply(definition: &mut Definition, patch: &Value) -> PluginResult<()> {
    let Some(patch) = patch.as_object() else {
        return Ok(());
    };
    if let Some(base) = patch.get("baseURL").and_then(Value::as_str) {
        definition.base_url = base.into();
    }
    if let Some(api) = patch.get("api").and_then(Value::as_str) {
        definition.api = api.into();
    }
    if let Some(header) = patch.get("authHeader").and_then(Value::as_str) {
        definition.config.auth_header = header.into();
        definition.config.api_key_auth = match header {
            "" | "auto" => definition.config.api_key_auth.clone(),
            "none" => Some(AuthScheme::None),
            "bearer" => Some(AuthScheme::Bearer),
            name => Some(AuthScheme::ApiKeyHeader { name: name.into() }),
        };
    }
    if let Some(endpoints) = patch
        .get("endpoints")
        .and_then(Value::as_object)
        .filter(|endpoints| !endpoints.is_empty())
    {
        let overrides = endpoints
            .iter()
            .map(|(api, base)| {
                base.as_str()
                    .map(|base| (api.as_str(), base))
                    .ok_or_else(|| PluginError::invalid("A provider endpoint must be text."))
            })
            .collect::<PluginResult<Vec<_>>>()?;
        let discovery = definition.config.model_discovery.as_ref();
        let defaults = discovery
            .into_iter()
            .flat_map(|discovery| discovery.bases.iter())
            .map(|base| (base.api.as_str(), base.base_url.as_str()));
        let bases = nora_provider_plugin_sdk::connection::api_endpoints(
            &definition.api,
            &definition.base_url,
            discovery
                .map(|discovery| discovery.default_base_url.as_str())
                .unwrap_or_default(),
            defaults,
            overrides,
        )
        .into_iter()
        .map(|(api, base_url)| ModelBase { api, base_url })
        .collect();
        definition.config.model_discovery = Some(ModelDiscovery {
            default_base_url: definition.base_url.clone(),
            bases,
            no_list: false,
        });
    }
    Ok(())
}
