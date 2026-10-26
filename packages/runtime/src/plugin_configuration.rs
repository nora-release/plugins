use crate::{auth, definition::Definition, network_client, ProviderAdapter};
use nora_provider_plugin_sdk::{
    InitializeParams, NetworkProxy, PluginError, PluginResult, API_VERSION,
};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc};

pub(super) struct Configuration {
    pub definition: Definition,
    pub installed_definition: Definition,
    pub data_directory: PathBuf,
    connection_patch: Value,
    network_proxy: Option<NetworkProxy>,
}

impl Configuration {
    pub fn parse(
        params: InitializeParams,
        adapter: Option<&Arc<dyn ProviderAdapter>>,
    ) -> PluginResult<Self> {
        if params.api_version != API_VERSION {
            return Err(PluginError::invalid(
                "Unsupported provider plugin API version.",
            ));
        }
        let network_proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The network proxy configuration is invalid."))?;
        if let Some(proxy) = &network_proxy {
            proxy.validate()?;
        }
        if !params.runtime_config.is_null() && !params.runtime_config.is_object() {
            return Err(PluginError::invalid(
                "The provider connection configuration must be an object.",
            ));
        }
        let mut installed = Definition::parse(
            params
                .installed_manifest
                .unwrap_or_else(|| params.manifest.clone()),
        )?;
        let mut definition = Definition::parse(params.manifest)?;
        if installed.id != definition.id {
            return Err(PluginError::invalid(
                "The installed provider identity does not match its configuration.",
            ));
        }
        // Declared provider options apply to every account; connection overrides
        // remain specific to API-key accounts.
        let options = installed
            .config
            .extra
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|option| option.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect::<Vec<_>>();
        for id in options {
            if let Some(value) = definition
                .config
                .extra
                .get(&id)
                .filter(|value| value.is_boolean())
            {
                installed.config.extra.insert(id, value.clone());
            }
        }
        if let Some(adapter) = adapter {
            if adapter.id().is_some_and(|id| id != definition.id) {
                return Err(PluginError::invalid(
                    "The provider executable does not match its manifest.",
                ));
            }
            adapter.initialize_data_directory(&params.data_directory)?;
            definition.adapter = adapter.clone();
            installed.adapter = adapter.clone();
        }
        Ok(Self {
            definition,
            installed_definition: installed,
            data_directory: params.data_directory,
            connection_patch: params.runtime_config,
            network_proxy,
        })
    }

    pub fn account_definition(&self, credentials: Option<&Value>) -> PluginResult<Definition> {
        if credentials.is_some_and(|value| !auth::is_api(value)) {
            return Ok(self.installed_definition.clone());
        }
        let mut definition = crate::profiles::configured(self.definition.clone(), credentials)?;
        crate::connection_patch::apply(&mut definition, &self.connection_patch)?;
        Ok(definition)
    }

    pub fn effective_proxy(
        &self,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<Option<NetworkProxy>> {
        let proxy = proxy.or(self.network_proxy.as_ref()).cloned();
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        Ok(proxy)
    }

    pub fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<network_client::Client> {
        let proxy = self.effective_proxy(proxy)?;
        network_client::Client::new(None, proxy.as_ref(), false)
    }

    pub fn request_client(
        &self,
        definition: &Definition,
        proxy: Option<&NetworkProxy>,
        credentials: Option<&Value>,
    ) -> PluginResult<network_client::Client> {
        let proxy = self.effective_proxy(proxy)?;
        network_client::Client::new(
            Some(definition),
            proxy.as_ref(),
            credentials.is_some_and(auth::is_api),
        )
    }

    pub fn params_proxy(&self, params: &Value) -> PluginResult<Option<NetworkProxy>> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("The network proxy configuration is invalid."))?;
        self.effective_proxy(proxy.as_ref())
    }

    pub fn params_client(&self, params: &Value) -> PluginResult<network_client::Client> {
        self.client(self.params_proxy(params)?.as_ref())
    }
}
