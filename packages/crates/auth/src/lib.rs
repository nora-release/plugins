//! Provider-owned sign-in, renewal, and account endpoint contracts.

pub mod browser_sign_in;
pub mod codex_source;
pub mod copilot_client;
pub mod copilot_editors;
pub mod copilot_identity;
pub mod grok_cpa;
pub mod grok_source;
pub mod manager;
pub mod oauth_callback_server;
pub mod oauth_login;
pub mod oauth_providers;
pub mod xai_auth;
pub mod xai_oidc;

pub use manager::OAuthManager;
pub use oauth_callback_server::{
    OAuthCallbackInput, OAuthCallbackServer, OAuthCallbackServerConfig,
};
pub use oauth_login::{
    Browser, GitHubCopilotLoginConfig, OAuthLogin, OAuthLoginCallbacks, OAuthLoginRequest,
};
pub use oauth_providers::{
    default_oauth_providers, AnthropicOAuthProvider, GitHubCopilotOAuthProvider,
    OpenAICodexOAuthProvider, XaiOAuthProvider,
};
pub use xai_auth::{
    apply_xai_api_key_contract, apply_xai_credentials_contract, apply_xai_oauth_session_contract,
    apply_xai_oauth_session_contract_with_client, fetch_xai_api_key_models,
    fetch_xai_oauth_billing, fetch_xai_oauth_models, fetch_xai_oauth_models_for_user,
    validate_xai_oauth_session, xai_api_key_models_request, xai_api_key_resolved_auth,
    xai_grok_client_version, xai_oauth_auxiliary_headers, xai_oauth_billing_request,
    xai_oauth_inference_headers, xai_oauth_models_request, xai_oauth_models_request_for_user,
    xai_oauth_resolved_auth, xai_oauth_session_headers, xai_oauth_user_request, XaiBillingConfig,
    XaiBillingCycle, XaiBillingPeriodUsage, XaiBillingResponse, XaiCent, XaiClientContext,
    XaiModelsResponse, XaiOAuthUser, XaiUsagePeriod, XAI_API_BASE_URL, XAI_API_MODELS_URL,
    XAI_AUTHENTICATE_RESPONSE_HEADER, XAI_AUTHENTICATE_RESPONSE_VALUE, XAI_CLIENT_MODE_HEADER,
    XAI_CLIENT_VERSION_HEADER, XAI_GROK_COMPAT_CLIENT_VERSION, XAI_OAUTH_BILLING_URL,
    XAI_OAUTH_MODELS_URL, XAI_OAUTH_PROXY_BASE_URL, XAI_OAUTH_USER_URL, XAI_TOKEN_AUTH_HEADER,
    XAI_TOKEN_AUTH_VALUE,
};
pub use xai_oidc::{
    discover_xai_oidc, resolve_xai_login_identity, validate_xai_id_token, XaiOidcDiscovery,
    XAI_OIDC_ISSUER, XAI_TEAM_PRINCIPAL_TYPE,
};
