use crate::oauth_callback_server::{OAuthCallbackInput, OAuthCallbackServer};
use crate::oauth_providers::{GitHubCopilotOAuthProvider, OpenAICodexOAuthProvider};
use crate::xai_auth::{xai_grok_client_version, xai_oauth_client_id, XAI_CLIENT_VERSION_HEADER};
use crate::xai_oidc::{discover_xai_oidc, resolve_xai_login_identity};
use nora_ai::http::{HttpClient, HttpMethod, HttpRequest};
use nora_ai::json_value::JsonValue;
use nora_ai::oauth::{
    decode_token_response, expires_from_now, url_encoded_form, OAuthCredentials, OAuthError,
    OAuthProvider, OAuthProviderId, OAuthTokenResponse,
};
use nora_ai::oauth_pkce::{random_hex, OAuthPkce};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::future::Future;
use std::io::{self, Write};
use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const OPENAI_CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_CODEX_DEFAULT_ORIGINATOR: &str = "nora-cli";
const OPENAI_CODEX_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
const GITHUB_COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const XAI_OAUTH_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write workspaces:read workspaces:write";

// MARK: - Request shape

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthLoginRequest {
    pub provider: OAuthProviderId,
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
}

// MARK: - Callbacks

pub type OAuthPromptFuture = Pin<Box<dyn Future<Output = Result<String, OAuthError>> + Send>>;

#[derive(Clone)]
pub struct OAuthLoginCallbacks {
    on_auth_url: Arc<dyn Fn(&str) + Send + Sync>,
    on_progress: Arc<dyn Fn(&str) + Send + Sync>,
    on_prompt: Arc<dyn Fn(String) -> OAuthPromptFuture + Send + Sync>,
    on_callback: Option<Arc<dyn Fn(OAuthCallbackInput, Option<String>) + Send + Sync>>,
}

impl OAuthLoginCallbacks {
    pub fn new(
        on_auth_url: impl Fn(&str) + Send + Sync + 'static,
        on_progress: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            on_auth_url: Arc::new(on_auth_url),
            on_progress: Arc::new(on_progress),
            on_prompt: default_prompt_callback(),
            on_callback: None,
        }
    }

    pub fn with_callback(
        mut self,
        callback: impl Fn(OAuthCallbackInput, Option<String>) + Send + Sync + 'static,
    ) -> Self {
        self.on_callback = Some(Arc::new(callback));
        self
    }

    pub fn with_prompt(
        mut self,
        on_prompt: impl Fn(String) -> OAuthPromptFuture + Send + Sync + 'static,
    ) -> Self {
        self.on_prompt = Arc::new(on_prompt);
        self
    }

    pub fn silent() -> Self {
        Self::new(|_| {}, |_| {}).with_prompt(|_| Box::pin(async { Ok(String::new()) }))
    }

    pub fn auth_url(&self, url: &str) {
        (self.on_auth_url)(url);
    }

    pub fn progress(&self, message: &str) {
        (self.on_progress)(message);
    }

    pub async fn prompt(&self, question: &str) -> Result<String, OAuthError> {
        (self.on_prompt)(question.to_string()).await
    }
}

impl Default for OAuthLoginCallbacks {
    fn default() -> Self {
        Self::new(
            |url| {
                eprintln!("open in your browser:\n  {url}\n");
                Browser::open(url);
            },
            |message| eprintln!("{message}"),
        )
    }
}

fn default_prompt_callback() -> Arc<dyn Fn(String) -> OAuthPromptFuture + Send + Sync> {
    Arc::new(|question| {
        Box::pin(async move {
            eprint!("{question} ");
            io::stderr()
                .flush()
                .map_err(|error| OAuthError::Transport(error.to_string()))?;
            let mut line = String::new();
            io::stdin()
                .read_line(&mut line)
                .map_err(|error| OAuthError::Transport(error.to_string()))?;
            Ok(line.trim().to_string())
        })
    })
}

// MARK: - Login orchestration

pub struct OAuthLogin;

impl OAuthLogin {
    pub async fn login_anthropic(
        callbacks: OAuthLoginCallbacks,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        let pkce = OAuthPkce::random();
        let mut server = OAuthCallbackServer::new(53692);
        server.start().await?;
        let redirect_uri = server.redirect_uri();
        let auth_url = Self::anthropic_authorize_url(&pkce, &redirect_uri)?;
        callbacks.auth_url(&auth_url);
        callbacks.progress(&format!(
            "waiting for Anthropic callback on {redirect_uri}..."
        ));

        let params = server.wait_for_callback().await?;
        let code = params.get("code").cloned().ok_or_else(|| {
            OAuthError::InvalidResponse("anthropic callback had no code".to_string())
        })?;
        if let Some(state) = params.get("state") {
            if state != &pkce.verifier {
                return Err(OAuthError::InvalidResponse(
                    "anthropic OAuth state mismatch".to_string(),
                ));
            }
        }

        callbacks.progress("exchanging authorization code...");
        let response = post_json(
            "https://platform.claude.com/v1/oauth/token",
            json!({
                "grant_type": "authorization_code",
                "client_id": ANTHROPIC_CLIENT_ID,
                "code": code,
                "state": params.get("state").unwrap_or(&pkce.verifier),
                "redirect_uri": redirect_uri,
                "code_verifier": pkce.verifier,
            }),
            client,
        )
        .await?;
        Ok(credentials_from_token(
            OAuthProviderId::Anthropic,
            response,
            None,
            BTreeMap::new(),
        ))
    }

    pub async fn login_openai_codex(
        callbacks: OAuthLoginCallbacks,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        Self::login_openai_codex_with_originator(callbacks, client, OPENAI_CODEX_DEFAULT_ORIGINATOR)
            .await
    }

    pub async fn login_openai_codex_with_originator(
        callbacks: OAuthLoginCallbacks,
        client: &dyn HttpClient,
        originator: &str,
    ) -> Result<OAuthCredentials, OAuthError> {
        let pkce = OAuthPkce::random();
        let state = random_hex(16);
        let mut server =
            OAuthCallbackServer::with_redirect_host(1455, "/auth/callback", "localhost");
        server.expect_state(&state);
        if let Some(callback) = &callbacks.on_callback {
            let blocked = server.start_with_paste().await?.map(str::to_owned);
            callback(
                server
                    .callback_input()
                    .expect("A Codex sign-in has a state"),
                blocked,
            );
        } else {
            server.start().await?;
        }
        let redirect_uri = server.redirect_uri();
        let auth_url = Self::openai_codex_authorize_url(&pkce, &state, &redirect_uri, originator)?;
        callbacks.auth_url(&auth_url);
        callbacks.progress(&format!(
            "waiting for ChatGPT callback on {redirect_uri}..."
        ));

        let params = server.wait_for_callback().await?;
        let code = params
            .get("code")
            .cloned()
            .ok_or_else(|| OAuthError::InvalidResponse("codex callback had no code".to_string()))?;
        if params.get("state") != Some(&state) {
            return Err(OAuthError::InvalidResponse(
                "codex OAuth state mismatch".to_string(),
            ));
        }

        callbacks.progress("exchanging authorization code...");
        let form = url_encoded_form(&BTreeMap::from([
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("code".to_string(), code),
            ("client_id".to_string(), OPENAI_CODEX_CLIENT_ID.to_string()),
            ("redirect_uri".to_string(), redirect_uri),
            ("code_verifier".to_string(), pkce.verifier),
        ]));
        let response = client
            .request(
                HttpRequest::new(HttpMethod::Post, "https://auth.openai.com/oauth/token")
                    .with_header("content-type", "application/x-www-form-urlencoded")
                    .with_header("accept", "application/json")
                    .with_body(form),
            )
            .await?;
        if response.status >= 400 {
            return Err(OAuthError::RefreshFailed(format!(
                "codex exchange {}: {}",
                response.status,
                String::from_utf8_lossy(&response.body)
            )));
        }

        let token = decode_token_response(&response.body)?;
        let mut extras = OpenAICodexOAuthProvider::profile_extras_from_tokens(
            &token.access_token,
            token.id_token.as_deref(),
        );
        if let Some(account_id) =
            OpenAICodexOAuthProvider::extract_account_id_from_jwt(&token.access_token)
        {
            extras.insert("accountId".to_string(), JsonValue::string(account_id));
        }
        Ok(credentials_from_token(
            OAuthProviderId::OpenAICodex,
            token,
            None,
            extras,
        ))
    }

    pub async fn login_xai(
        callbacks: OAuthLoginCallbacks,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        callbacks.progress("discovering xAI sign-in endpoints...");
        let discovery = discover_xai_oidc(client).await?;
        let client_id = xai_oauth_client_id();
        let pkce = OAuthPkce::random();
        let state = random_hex(16);
        let nonce = random_hex(16);
        let mut server = OAuthCallbackServer::with_path(0, "/callback");
        server.start().await?;
        let redirect_uri = server.redirect_uri();
        let auth_url = Self::xai_authorize_url_with_endpoint(
            &discovery.authorization_endpoint,
            &pkce,
            &state,
            &nonce,
            &redirect_uri,
        )?;
        callbacks.auth_url(&auth_url);
        callbacks.progress(&format!("waiting for xAI callback on {}...", redirect_uri));

        let params = tokio::time::timeout(Duration::from_secs(10 * 60), server.wait_for_callback())
            .await
            .map_err(|_| OAuthError::Transport("xAI OAuth callback timed out".to_string()))??;
        let code = params
            .get("code")
            .cloned()
            .ok_or_else(|| OAuthError::InvalidResponse("xai callback had no code".to_string()))?;
        if params.get("state") != Some(&state) {
            return Err(OAuthError::InvalidResponse(
                "xai OAuth state mismatch".to_string(),
            ));
        }

        callbacks.progress("exchanging authorization code...");
        let form_params = BTreeMap::from([
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("code".to_string(), code),
            ("client_id".to_string(), client_id.to_string()),
            ("redirect_uri".to_string(), redirect_uri),
            ("code_verifier".to_string(), pkce.verifier),
        ]);

        let form = url_encoded_form(&form_params);
        let response = tokio::time::timeout(
            Duration::from_secs(30),
            client.request(
                HttpRequest::new(HttpMethod::Post, discovery.token_endpoint.clone())
                    .with_header("content-type", "application/x-www-form-urlencoded")
                    .with_header("accept", "application/json")
                    .with_header(XAI_CLIENT_VERSION_HEADER, xai_grok_client_version())
                    .with_body(form),
            ),
        )
        .await
        .map_err(|_| {
            OAuthError::Transport("xAI authorization-code exchange timed out".to_string())
        })??;
        if response.status == 403 {
            return Err(OAuthError::RefreshFailed(
                "xAI did not authorize this account session. Sign in with a Grok/SuperGrok account that can use Grok Build, or use an xAI API key instead.".to_string(),
            ));
        }
        if response.status >= 400 {
            return Err(OAuthError::RefreshFailed(format!(
                "xai exchange {}: {}",
                response.status,
                String::from_utf8_lossy(&response.body)
            )));
        }

        let token = decode_token_response(&response.body)?;
        let user =
            resolve_xai_login_identity(client, &token, &discovery, client_id, &nonce).await?;
        Ok(credentials_from_token(
            OAuthProviderId::XAI,
            token,
            None,
            user.credential_extras(),
        ))
    }

    pub async fn login_github_copilot(
        callbacks: OAuthLoginCallbacks,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        Self::login_github_copilot_with_config(
            GitHubCopilotLoginConfig::default(),
            callbacks,
            client,
        )
        .await
    }

    pub async fn login_github_copilot_with_config(
        config: GitHubCopilotLoginConfig,
        callbacks: OAuthLoginCallbacks,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        callbacks.progress("requesting GitHub device code...");
        let device_response = client
            .request(
                HttpRequest::new(HttpMethod::Post, config.device_code_url.clone())
                    .with_header("accept", "application/json")
                    .with_header("content-type", "application/x-www-form-urlencoded")
                    .with_body(url_encoded_form(&BTreeMap::from([
                        ("client_id".to_string(), config.client_id.clone()),
                        ("scope".to_string(), "read:user".to_string()),
                    ]))),
            )
            .await?;
        if device_response.status >= 400 {
            return Err(OAuthError::RefreshFailed(format!(
                "copilot device code {}: {}",
                device_response.status,
                String::from_utf8_lossy(&device_response.body)
            )));
        }

        let device = decode_device_response(&device_response.body)?;
        callbacks.auth_url(&device.verification_uri);
        callbacks.progress(&format!("enter code in your browser: {}", device.user_code));

        let poll_interval = Duration::from_secs(device.interval.unwrap_or(5));
        let deadline = Instant::now() + config.timeout;
        while Instant::now() < deadline {
            tokio::time::sleep(poll_interval).await;
            let poll_response = client
                .request(
                    HttpRequest::new(HttpMethod::Post, config.access_token_url.clone())
                        .with_header("accept", "application/json")
                        .with_header("content-type", "application/x-www-form-urlencoded")
                        .with_body(url_encoded_form(&BTreeMap::from([
                            ("client_id".to_string(), config.client_id.clone()),
                            ("device_code".to_string(), device.device_code.clone()),
                            (
                                "grant_type".to_string(),
                                "urn:ietf:params:oauth:grant-type:device_code".to_string(),
                            ),
                        ]))),
                )
                .await?;
            if poll_response.status >= 400 {
                continue;
            }

            let polled = serde_json::from_slice::<Value>(&poll_response.body).map_err(|_| {
                OAuthError::InvalidResponse("copilot access token response".to_string())
            })?;
            if let Some(error) = polled.get("error").and_then(Value::as_str) {
                match error {
                    "authorization_pending" => continue,
                    "slow_down" => {
                        tokio::time::sleep(poll_interval.saturating_mul(2)).await;
                        continue;
                    }
                    other => {
                        return Err(OAuthError::RefreshFailed(format!(
                            "copilot device flow: {other}"
                        )))
                    }
                }
            }
            if let Some(pat) = polled.get("access_token").and_then(Value::as_str) {
                let provider = GitHubCopilotOAuthProvider {
                    token_url: config.copilot_token_url,
                    ..GitHubCopilotOAuthProvider::default()
                };
                let base = OAuthCredentials::new(
                    OAuthProviderId::GitHubCopilot,
                    "",
                    Some(pat.to_string()),
                    Some(0),
                    BTreeMap::new(),
                );
                let mut credentials = provider.refresh(&base, client).await?;
                crate::copilot_identity::enrich_at(
                    &mut credentials,
                    client,
                    &config.github_user_url,
                    &config.github_host,
                )
                .await?;
                return Ok(credentials);
            }
        }

        Err(OAuthError::Transport(
            "copilot device flow timed out".to_string(),
        ))
    }

    pub async fn enable_copilot_models(
        session_token: &str,
        base_url: &str,
        model_ids: &[String],
        callbacks: OAuthLoginCallbacks,
        client: &dyn HttpClient,
    ) {
        let base_url = base_url.trim_end_matches('/');
        let body = br#"{"state":"enabled"}"#.to_vec();
        for id in model_ids {
            let request =
                HttpRequest::new(HttpMethod::Post, format!("{base_url}/models/{id}/policy"))
                    .with_header("content-type", "application/json")
                    .with_header("authorization", format!("Bearer {session_token}"))
                    .with_header("editor-version", "vscode/1.107.0")
                    .with_header("editor-plugin-version", "copilot-chat/0.35.0")
                    .with_header("user-agent", "GitHubCopilotChat/0.35.0")
                    .with_header("copilot-integration-id", "vscode-chat")
                    .with_header("openai-intent", "chat-policy")
                    .with_header("x-interaction-type", "chat-policy")
                    .with_body(body.clone());

            match client.request(request).await {
                Ok(response) if response.status < 400 => {
                    callbacks.progress(&format!("  - {id}: enabled"));
                }
                Ok(response) => {
                    callbacks.progress(&format!("  - {id}: policy {} (skipped)", response.status));
                }
                Err(error) => {
                    callbacks.progress(&format!("  - {id}: {error}"));
                }
            }
        }
    }

    pub fn anthropic_authorize_url(
        pkce: &OAuthPkce,
        redirect_uri: &str,
    ) -> Result<String, OAuthError> {
        let scope = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
        url_with_query(
            "https://claude.ai/oauth/authorize",
            &[
                ("code", "true"),
                ("client_id", ANTHROPIC_CLIENT_ID),
                ("response_type", "code"),
                ("redirect_uri", redirect_uri),
                ("scope", scope),
                ("code_challenge", pkce.challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("state", pkce.verifier.as_str()),
            ],
        )
    }

    pub fn openai_codex_authorize_url(
        pkce: &OAuthPkce,
        state: &str,
        redirect_uri: &str,
        originator: &str,
    ) -> Result<String, OAuthError> {
        url_with_query(
            "https://auth.openai.com/oauth/authorize",
            &[
                ("response_type", "code"),
                ("client_id", OPENAI_CODEX_CLIENT_ID),
                ("redirect_uri", redirect_uri),
                ("scope", OPENAI_CODEX_SCOPE),
                ("code_challenge", pkce.challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("id_token_add_organizations", "true"),
                ("codex_cli_simplified_flow", "true"),
                ("state", state),
                ("originator", originator),
            ],
        )
    }

    pub fn xai_authorize_url(
        pkce: &OAuthPkce,
        state: &str,
        redirect_uri: &str,
    ) -> Result<String, OAuthError> {
        Self::xai_authorize_url_with_nonce(pkce, state, &random_hex(16), redirect_uri)
    }

    pub fn xai_authorize_url_with_nonce(
        pkce: &OAuthPkce,
        state: &str,
        nonce: &str,
        redirect_uri: &str,
    ) -> Result<String, OAuthError> {
        Self::xai_authorize_url_with_endpoint(
            "https://auth.x.ai/oauth2/authorize",
            pkce,
            state,
            nonce,
            redirect_uri,
        )
    }

    fn xai_authorize_url_with_endpoint(
        authorization_endpoint: &str,
        pkce: &OAuthPkce,
        state: &str,
        nonce: &str,
        redirect_uri: &str,
    ) -> Result<String, OAuthError> {
        let client_id = xai_oauth_client_id();
        url_with_query(
            authorization_endpoint,
            &[
                ("response_type", "code"),
                ("client_id", client_id),
                ("redirect_uri", redirect_uri),
                ("scope", XAI_OAUTH_SCOPE),
                ("code_challenge", pkce.challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("state", state),
                ("nonce", nonce),
                ("referrer", "nora"),
            ],
        )
    }
}

// MARK: - GitHub Copilot device flow

#[derive(Clone, Debug)]
pub struct GitHubCopilotLoginConfig {
    pub client_id: String,
    pub device_code_url: String,
    pub access_token_url: String,
    pub copilot_token_url: String,
    pub github_user_url: String,
    pub github_host: String,
    pub timeout: Duration,
}

impl Default for GitHubCopilotLoginConfig {
    fn default() -> Self {
        Self {
            client_id: GITHUB_COPILOT_CLIENT_ID.to_string(),
            device_code_url: "https://github.com/login/device/code".to_string(),
            access_token_url: "https://github.com/login/oauth/access_token".to_string(),
            copilot_token_url: "https://api.github.com/copilot_internal/v2/token".to_string(),
            github_user_url: "https://api.github.com/user".to_string(),
            github_host: String::new(),
            timeout: Duration::from_secs(15 * 60),
        }
    }
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    user_code: String,
    device_code: String,
    verification_uri: String,
    interval: Option<u64>,
}

fn decode_device_response(data: &[u8]) -> Result<DeviceCodeResponse, OAuthError> {
    serde_json::from_slice(data).map_err(|_| {
        OAuthError::InvalidResponse(format!(
            "copilot device code response: {}",
            String::from_utf8_lossy(data)
        ))
    })
}

// MARK: - Browser launcher

pub struct Browser;

impl Browser {
    pub fn open(url: &str) {
        #[cfg(target_os = "macos")]
        let opener = "/usr/bin/open";
        #[cfg(not(target_os = "macos"))]
        let opener = "/usr/bin/xdg-open";

        if Command::new(opener).arg(url).spawn().is_err() {
            eprintln!("please open manually:\n  {url}");
        }
    }
}

// MARK: - Helpers

async fn post_json(
    url: &str,
    body: Value,
    client: &dyn HttpClient,
) -> Result<OAuthTokenResponse, OAuthError> {
    let response = client
        .request(
            HttpRequest::new(HttpMethod::Post, url)
                .with_header("content-type", "application/json")
                .with_header("accept", "application/json")
                .with_body(
                    serde_json::to_vec(&body)
                        .map_err(|error| OAuthError::InvalidResponse(error.to_string()))?,
                ),
        )
        .await?;
    if response.status >= 400 {
        let host = url::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "oauth".to_string());
        return Err(OAuthError::RefreshFailed(format!(
            "{host} {}: {}",
            response.status,
            String::from_utf8_lossy(&response.body)
        )));
    }
    decode_token_response(&response.body)
}

fn credentials_from_token(
    provider: OAuthProviderId,
    response: OAuthTokenResponse,
    fallback_refresh: Option<String>,
    extras: BTreeMap<String, JsonValue>,
) -> OAuthCredentials {
    OAuthCredentials::new(
        provider,
        response.access_token,
        Some(
            response
                .refresh_token
                .or(fallback_refresh)
                .unwrap_or_default(),
        ),
        Some(expires_from_now(response.expires_in)),
        extras,
    )
}

fn url_with_query(base: &str, params: &[(&str, &str)]) -> Result<String, OAuthError> {
    let mut url =
        url::Url::parse(base).map_err(|error| OAuthError::InvalidResponse(error.to_string()))?;
    {
        let mut query = url.query_pairs_mut();
        query.clear();
        for (key, value) in params {
            query.append_pair(key, value);
        }
    }
    Ok(url.to_string())
}
