use crate::xai_auth::apply_xai_credentials_contract;
use nora_ai::{
    AuthScheme, HttpClient, Model, OAuthCredentials, OAuthError, OAuthProvider, OAuthStore,
    ReqwestHttpClient, ResolvedProviderAuth,
};
use std::collections::BTreeMap;
use std::sync::Arc;

// MARK: - Manager

pub struct OAuthManager {
    pub store: OAuthStore,
    pub client: Arc<dyn HttpClient>,
    providers: BTreeMap<String, Box<dyn OAuthProvider>>,
}

impl OAuthManager {
    pub fn new(
        store: OAuthStore,
        providers: Vec<Box<dyn OAuthProvider>>,
        client: Arc<dyn HttpClient>,
    ) -> Self {
        Self {
            store,
            client,
            providers: providers
                .into_iter()
                .map(|provider| (provider.id().to_string(), provider))
                .collect(),
        }
    }

    pub fn with_default_providers(store: OAuthStore, client: Arc<dyn HttpClient>) -> Self {
        Self::new(
            store,
            crate::oauth_providers::default_oauth_providers(),
            client,
        )
    }

    pub fn at_default_store() -> Self {
        Self::with_default_providers(
            OAuthStore::at_default_path(),
            Arc::new(
                ReqwestHttpClient::with_client(
                    nora_provider_network::default_http_client()
                        .expect("OAuth HTTP client could not be initialized"),
                )
                .with_observer_fn(|| async {
                    nora_provider_plugin_sdk::before_http_request()
                        .await
                        .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
                }),
            ),
        )
    }

    pub fn register(&mut self, provider: Box<dyn OAuthProvider>) {
        self.providers.insert(provider.id().to_string(), provider);
    }

    pub async fn api_key(&mut self, provider_id: &str) -> Result<String, OAuthError> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| OAuthError::UnknownProvider(provider_id.to_string()))?;
        let mut credentials = self
            .store
            .get(provider_id)
            .ok_or_else(|| OAuthError::Missing(provider_id.to_string()))?;

        if credentials.is_expired() {
            credentials = provider.refresh(&credentials, self.client.as_ref()).await?;
            self.store
                .set(provider_id.to_string(), credentials.clone())?;
        }

        provider.api_key(&credentials, self.client.as_ref()).await
    }

    pub async fn resolved_auth(
        &mut self,
        provider_id: &str,
    ) -> Result<ResolvedProviderAuth, OAuthError> {
        let token = self.api_key(provider_id).await?;
        let credentials = self.store.get(provider_id);
        let mut auth = ResolvedProviderAuth {
            token: Some(token),
            credential_provider_id: Some(provider_id.to_string()),
            scheme: AuthScheme::Bearer,
            headers: BTreeMap::new(),
            base_url: Self::base_url_for_oauth_id(provider_id, credentials.as_ref()),
            metadata: BTreeMap::new(),
        };
        if let Some(credentials) = credentials.as_ref() {
            apply_xai_credentials_contract(provider_id, credentials, &mut auth);
        }
        Ok(auth)
    }

    pub async fn refresh_resolved_auth(
        &mut self,
        provider_id: &str,
    ) -> Result<ResolvedProviderAuth, OAuthError> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| OAuthError::UnknownProvider(provider_id.to_string()))?;
        let credentials = self
            .store
            .get(provider_id)
            .ok_or_else(|| OAuthError::Missing(provider_id.to_string()))?;
        let refreshed = provider.refresh(&credentials, self.client.as_ref()).await?;
        self.store.set(provider_id.to_string(), refreshed.clone())?;
        let token = provider.api_key(&refreshed, self.client.as_ref()).await?;
        let mut auth = ResolvedProviderAuth {
            token: Some(token),
            credential_provider_id: Some(provider_id.to_string()),
            scheme: AuthScheme::Bearer,
            headers: BTreeMap::new(),
            base_url: Self::base_url_for_oauth_id(provider_id, Some(&refreshed)),
            metadata: BTreeMap::new(),
        };
        apply_xai_credentials_contract(provider_id, &refreshed, &mut auth);
        Ok(auth)
    }

    pub async fn resolved_auth_for_model(
        &mut self,
        model: &Model,
    ) -> Result<ResolvedProviderAuth, OAuthError> {
        let provider_id = Self::oauth_id_for_provider(&model.provider);
        self.resolved_auth(&provider_id).await
    }

    pub fn oauth_id_for_provider(provider: &str) -> String {
        match provider {
            "anthropic" => "anthropic",
            "github-copilot" => "github-copilot",
            "openai-codex" => "openai-codex",
            "xai" => "xai",
            other => other,
        }
        .to_string()
    }

    pub fn base_url_for_oauth_id(
        provider_id: &str,
        credentials: Option<&OAuthCredentials>,
    ) -> Option<String> {
        if provider_id != "github-copilot" {
            return None;
        }
        credentials
            .and_then(|credentials| credentials.extras.get("endpoint"))
            .and_then(|value| value.as_str())
            .filter(|endpoint| !endpoint.is_empty())
            .map(ToOwned::to_owned)
    }
}
