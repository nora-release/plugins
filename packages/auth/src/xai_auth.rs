use nora_ai::auth_headers::bearer_header_value;
use nora_ai::context::{AuthScheme, ResolvedProviderAuth};
use nora_ai::http::{HttpClient, HttpMethod, HttpRequest, HttpResponse};
use nora_ai::json_value::JsonValue;
use nora_ai::oauth::{OAuthCredentials, OAuthError, OAuthProviderId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

const XAI_AUXILIARY_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub const XAI_COMPAT_OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const XAI_GROK_COMPAT_CLIENT_VERSION: &str = "1.0.62";
pub const XAI_API_BASE_URL: &str = "https://api.x.ai/v1";
pub const XAI_OAUTH_PROXY_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
pub const XAI_OAUTH_USER_URL: &str = "https://cli-chat-proxy.grok.com/v1/user";
pub const XAI_OAUTH_MODELS_URL: &str = "https://cli-chat-proxy.grok.com/v1/models";
pub const XAI_OAUTH_BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
pub const XAI_API_MODELS_URL: &str = "https://api.x.ai/v1/models";
pub const XAI_TOKEN_AUTH_HEADER: &str = "X-XAI-Token-Auth";
pub const XAI_TOKEN_AUTH_VALUE: &str = "xai-grok-cli";
pub const XAI_AUTHENTICATE_RESPONSE_HEADER: &str = "x-authenticateresponse";
pub const XAI_AUTHENTICATE_RESPONSE_VALUE: &str = "authenticate-response";
pub const XAI_CLIENT_VERSION_HEADER: &str = "x-grok-client-version";
pub const XAI_CLIENT_MODE_HEADER: &str = "x-grok-client-mode";

pub fn xai_oauth_client_id() -> &'static str {
    if let Some(client_id) = option_env!("NORA_XAI_OAUTH_CLIENT_ID") {
        let client_id = client_id.trim();
        if !client_id.is_empty() {
            return client_id;
        }
    }
    XAI_COMPAT_OAUTH_CLIENT_ID
}

/// Version of the Grok account-proxy contract Nora implements. This is
/// intentionally independent from Nora's crate and app versions.
pub fn xai_grok_client_version() -> &'static str {
    if let Some(version) = option_env!("NORA_XAI_GROK_CLIENT_VERSION") {
        let version = version.trim();
        if !version.is_empty() {
            return version;
        }
    }
    XAI_GROK_COMPAT_CLIENT_VERSION
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XaiClientContext {
    pub version: String,
    pub mode: String,
}

impl XaiClientContext {
    pub fn new(version: impl Into<String>, mode: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            mode: mode.into(),
        }
    }
}

impl Default for XaiClientContext {
    fn default() -> Self {
        Self::new(xai_grok_client_version(), "interactive")
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiOAuthUser {
    pub user_id: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub profile_image_asset_id: Option<String>,
    pub principal_type: Option<String>,
    pub principal_id: Option<String>,
    pub team_id: Option<String>,
    pub team_name: Option<String>,
    pub team_role: Option<String>,
    pub organization_id: Option<String>,
    pub organization_name: Option<String>,
    pub organization_role: Option<String>,
    pub user_blocked_reason: Option<String>,
    pub team_blocked_reasons: Option<Vec<String>>,
    pub coding_data_retention_opt_out: Option<bool>,
    pub subscription_tier: Option<String>,
}

impl<'de> Deserialize<'de> for XaiOAuthUser {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(default)]
            user_id: Option<String>,
            #[serde(default, rename = "user_id")]
            user_id_snake: Option<String>,
            #[serde(default)]
            sub: Option<String>,
            #[serde(default)]
            principal_id: Option<String>,
            #[serde(default)]
            team_id: Option<String>,
            #[serde(default)]
            organization_id: Option<String>,
            #[serde(default)]
            email: Option<String>,
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            first_name: Option<String>,
            #[serde(default)]
            last_name: Option<String>,
            #[serde(default)]
            profile_image_asset_id: Option<String>,
            #[serde(default)]
            principal_type: Option<String>,
            #[serde(default)]
            team_name: Option<String>,
            #[serde(default)]
            team_role: Option<String>,
            #[serde(default)]
            organization_name: Option<String>,
            #[serde(default)]
            organization_role: Option<String>,
            #[serde(default)]
            user_blocked_reason: Option<String>,
            #[serde(default)]
            team_blocked_reasons: Option<Vec<String>>,
            #[serde(default)]
            coding_data_retention_opt_out: Option<bool>,
            #[serde(default)]
            subscription_tier: Option<String>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let user_id = normalized_owned(raw.user_id.clone())
            .or_else(|| normalized_owned(raw.user_id_snake.clone()))
            .or_else(|| normalized_owned(raw.sub.clone()))
            .or_else(|| normalized_owned(raw.principal_id.clone()))
            .or_else(|| normalized_owned(raw.team_id.clone()))
            .or_else(|| normalized_owned(raw.organization_id.clone()))
            .unwrap_or_default();
        Ok(XaiOAuthUser {
            user_id,
            email: raw.email,
            name: raw.name,
            first_name: raw.first_name,
            last_name: raw.last_name,
            profile_image_asset_id: raw.profile_image_asset_id,
            principal_type: raw.principal_type,
            principal_id: raw.principal_id,
            team_id: raw.team_id,
            team_name: raw.team_name,
            team_role: raw.team_role,
            organization_id: raw.organization_id,
            organization_name: raw.organization_name,
            organization_role: raw.organization_role,
            user_blocked_reason: raw.user_blocked_reason,
            team_blocked_reasons: raw.team_blocked_reasons,
            coding_data_retention_opt_out: raw.coding_data_retention_opt_out,
            subscription_tier: raw.subscription_tier,
        })
    }
}

impl XaiOAuthUser {
    pub fn display_name(&self) -> Option<String> {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                let name = [self.first_name.as_deref(), self.last_name.as_deref()]
                    .into_iter()
                    .flatten()
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                (!name.is_empty()).then_some(name)
            })
    }

    pub fn credential_extras(&self) -> BTreeMap<String, JsonValue> {
        let mut extras = BTreeMap::from([(
            "userId".to_string(),
            JsonValue::string(self.user_id.clone()),
        )]);
        insert_optional_extra(&mut extras, "email", self.email.as_deref());
        insert_optional_extra(&mut extras, "name", self.display_name().as_deref());
        insert_optional_extra(&mut extras, "firstName", self.first_name.as_deref());
        insert_optional_extra(&mut extras, "lastName", self.last_name.as_deref());
        insert_optional_extra(
            &mut extras,
            "profileImageAssetId",
            self.profile_image_asset_id.as_deref(),
        );
        insert_optional_extra(&mut extras, "principalType", self.principal_type.as_deref());
        insert_optional_extra(&mut extras, "principalId", self.principal_id.as_deref());
        insert_optional_extra(&mut extras, "teamId", self.team_id.as_deref());
        insert_optional_extra(&mut extras, "teamName", self.team_name.as_deref());
        insert_optional_extra(&mut extras, "teamRole", self.team_role.as_deref());
        insert_optional_extra(
            &mut extras,
            "organizationId",
            self.organization_id.as_deref(),
        );
        insert_optional_extra(
            &mut extras,
            "organizationName",
            self.organization_name.as_deref(),
        );
        insert_optional_extra(
            &mut extras,
            "organizationRole",
            self.organization_role.as_deref(),
        );
        insert_optional_extra(
            &mut extras,
            "userBlockedReason",
            self.user_blocked_reason.as_deref(),
        );
        insert_optional_extra(
            &mut extras,
            "subscriptionTier",
            self.subscription_tier.as_deref(),
        );
        if let Some(reasons) = self.team_blocked_reasons.as_ref() {
            extras.insert(
                "teamBlockedReasons".to_string(),
                JsonValue::array(
                    reasons
                        .iter()
                        .cloned()
                        .map(|reason| JsonValue::string(reason)),
                ),
            );
        }
        if let Some(value) = self.coding_data_retention_opt_out {
            extras.insert(
                "codingDataRetentionOptOut".to_string(),
                JsonValue::bool(value),
            );
        }
        extras
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct XaiModelsResponse {
    #[serde(default)]
    pub data: Vec<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiBillingCycle {
    pub year: i32,
    pub month: i32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct XaiCent {
    #[serde(default)]
    pub val: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiUsagePeriod {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub period_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiBillingPeriodUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_cycle: Option<XaiBillingCycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub included_used: Option<XaiCent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_demand_used: Option<XaiCent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_used: Option<XaiCent>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiBillingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_usage_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_period: Option<XaiUsagePeriod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_limit: Option<XaiCent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<XaiCent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_demand_cap: Option<XaiCent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_demand_used: Option<XaiCent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepaid_balance: Option<XaiCent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_unified_billing_user: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_period_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_period_end: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<XaiBillingPeriodUsage>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiBillingResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<XaiBillingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_demand_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_tier: Option<String>,
}

pub fn xai_oauth_session_headers() -> BTreeMap<String, String> {
    xai_oauth_inference_headers(&XaiClientContext::default())
}

pub fn xai_oauth_inference_headers(client: &XaiClientContext) -> BTreeMap<String, String> {
    let mut headers = xai_oauth_auxiliary_headers(client);
    headers.insert(
        XAI_AUTHENTICATE_RESPONSE_HEADER.to_string(),
        XAI_AUTHENTICATE_RESPONSE_VALUE.to_string(),
    );
    headers
}

pub fn xai_oauth_auxiliary_headers(client: &XaiClientContext) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            XAI_TOKEN_AUTH_HEADER.to_string(),
            XAI_TOKEN_AUTH_VALUE.to_string(),
        ),
        (
            XAI_CLIENT_VERSION_HEADER.to_string(),
            client.version.trim().to_string(),
        ),
        (
            XAI_CLIENT_MODE_HEADER.to_string(),
            client.mode.trim().to_string(),
        ),
    ])
}

pub fn xai_oauth_resolved_auth(token: impl Into<String>) -> ResolvedProviderAuth {
    let mut auth = ResolvedProviderAuth {
        token: non_empty_token(token.into()),
        credential_provider_id: None,
        scheme: AuthScheme::Bearer,
        headers: BTreeMap::new(),
        base_url: None,
        metadata: BTreeMap::new(),
    };
    apply_xai_oauth_session_contract(&mut auth);
    auth
}

pub fn xai_api_key_resolved_auth(token: impl Into<String>) -> ResolvedProviderAuth {
    let mut auth = ResolvedProviderAuth {
        token: non_empty_token(token.into()),
        credential_provider_id: None,
        scheme: AuthScheme::Bearer,
        headers: BTreeMap::new(),
        base_url: None,
        metadata: BTreeMap::new(),
    };
    apply_xai_api_key_contract(&mut auth);
    auth
}

pub fn apply_xai_oauth_session_contract(auth: &mut ResolvedProviderAuth) {
    apply_xai_oauth_session_contract_with_client(auth, &XaiClientContext::default());
}

pub fn apply_xai_oauth_session_contract_with_client(
    auth: &mut ResolvedProviderAuth,
    client: &XaiClientContext,
) {
    auth.scheme = AuthScheme::Bearer;
    auth.base_url = Some(XAI_OAUTH_PROXY_BASE_URL.to_string());
    remove_conflicting_auth_headers(&mut auth.headers);
    for (name, value) in xai_oauth_inference_headers(client) {
        set_header_case_insensitive(&mut auth.headers, name, value);
    }
}

pub fn apply_xai_api_key_contract(auth: &mut ResolvedProviderAuth) {
    auth.scheme = AuthScheme::Bearer;
    let should_use_public_api = match auth.base_url.as_deref().map(str::trim) {
        None => true,
        Some(base_url) => {
            base_url.is_empty()
                || base_url
                    .trim_end_matches('/')
                    .eq_ignore_ascii_case(XAI_OAUTH_PROXY_BASE_URL.trim_end_matches('/'))
        }
    };
    if should_use_public_api {
        auth.base_url = Some(XAI_API_BASE_URL.to_string());
    }
    remove_conflicting_auth_headers(&mut auth.headers);
    remove_header_case_insensitive(&mut auth.headers, XAI_TOKEN_AUTH_HEADER);
    remove_header_case_insensitive(&mut auth.headers, XAI_AUTHENTICATE_RESPONSE_HEADER);
    remove_header_case_insensitive(&mut auth.headers, XAI_CLIENT_VERSION_HEADER);
    remove_header_case_insensitive(&mut auth.headers, XAI_CLIENT_MODE_HEADER);
}

pub fn apply_xai_credentials_contract(
    provider_id: &str,
    credentials: &OAuthCredentials,
    auth: &mut ResolvedProviderAuth,
) -> bool {
    if !matches!(provider_id, "xai" | "xai-api-key") {
        return false;
    }
    if provider_id == "xai" && credentials.provider == OAuthProviderId::XAI {
        apply_xai_oauth_session_contract(auth);
    } else {
        apply_xai_api_key_contract(auth);
    }
    true
}

pub fn xai_oauth_user_request(access_token: &str) -> Result<HttpRequest, OAuthError> {
    xai_oauth_auxiliary_get_request(
        XAI_OAUTH_USER_URL,
        access_token,
        &XaiClientContext::default(),
    )
}

pub fn xai_oauth_models_request(access_token: &str) -> Result<HttpRequest, OAuthError> {
    xai_oauth_auxiliary_get_request(
        XAI_OAUTH_MODELS_URL,
        access_token,
        &XaiClientContext::default(),
    )
}

pub fn xai_oauth_models_request_for_user(
    access_token: &str,
    user_id: &str,
) -> Result<HttpRequest, OAuthError> {
    let user_id = require_user_id(user_id, "xAI model discovery")?;
    Ok(xai_oauth_models_request(access_token)?.with_header("x-userid", user_id))
}

pub fn xai_api_key_models_request(api_key: &str) -> Result<HttpRequest, OAuthError> {
    xai_bearer_get_request(XAI_API_MODELS_URL, api_key)
}

pub fn xai_oauth_billing_request(
    access_token: &str,
    user_id: &str,
) -> Result<HttpRequest, OAuthError> {
    let user_id = require_user_id(user_id, "xAI billing request")?;
    Ok(xai_oauth_auxiliary_get_request(
        XAI_OAUTH_BILLING_URL,
        access_token,
        &XaiClientContext::default(),
    )?
    .with_header("x-userid", user_id))
}

pub async fn validate_xai_oauth_session(
    client: &dyn HttpClient,
    access_token: &str,
) -> Result<XaiOAuthUser, OAuthError> {
    let response = request_with_timeout(
        client,
        xai_oauth_user_request(access_token)?,
        "xAI OAuth session verification",
        XAI_AUXILIARY_REQUEST_TIMEOUT,
    )
    .await?;
    ensure_success("xAI OAuth session verification", &response)?;
    let user = serde_json::from_slice::<XaiOAuthUser>(&response.body).map_err(|error| {
        OAuthError::InvalidResponse(format!("could not decode xAI /user response: {error}"))
    })?;
    if user.user_id.trim().is_empty() {
        return Err(OAuthError::InvalidResponse(
            "xAI /user response missing userId".to_string(),
        ));
    }
    Ok(user)
}

pub async fn fetch_xai_oauth_models(
    client: &dyn HttpClient,
    access_token: &str,
) -> Result<XaiModelsResponse, OAuthError> {
    let user = validate_xai_oauth_session(client, access_token).await?;
    fetch_xai_oauth_models_for_user(client, access_token, &user.user_id).await
}

pub async fn fetch_xai_oauth_models_for_user(
    client: &dyn HttpClient,
    access_token: &str,
    user_id: &str,
) -> Result<XaiModelsResponse, OAuthError> {
    let user_id = resolve_xai_user_id(client, access_token, user_id).await?;
    let response = request_with_timeout(
        client,
        xai_oauth_models_request_for_user(access_token, &user_id)?,
        "xAI OAuth model discovery",
        XAI_AUXILIARY_REQUEST_TIMEOUT,
    )
    .await?;
    decode_models_response("xAI OAuth model discovery", response)
}

pub async fn fetch_xai_api_key_models(
    client: &dyn HttpClient,
    api_key: &str,
) -> Result<XaiModelsResponse, OAuthError> {
    let response = request_with_timeout(
        client,
        xai_api_key_models_request(api_key)?,
        "xAI API-key model discovery",
        XAI_AUXILIARY_REQUEST_TIMEOUT,
    )
    .await?;
    decode_models_response("xAI API-key model discovery", response)
}

pub async fn fetch_xai_oauth_billing(
    client: &dyn HttpClient,
    access_token: &str,
    user_id: &str,
) -> Result<XaiBillingResponse, OAuthError> {
    let user_id = resolve_xai_user_id(client, access_token, user_id).await?;
    let response = request_with_timeout(
        client,
        xai_oauth_billing_request(access_token, &user_id)?,
        "xAI OAuth billing",
        XAI_AUXILIARY_REQUEST_TIMEOUT,
    )
    .await?;
    ensure_success("xAI OAuth billing", &response)?;
    serde_json::from_slice(&response.body).map_err(|error| {
        OAuthError::InvalidResponse(format!("could not decode xAI billing response: {error}"))
    })
}

fn xai_bearer_get_request(url: &str, token: &str) -> Result<HttpRequest, OAuthError> {
    let token = token.trim();
    if token.is_empty() {
        return Err(OAuthError::Missing("xai".to_string()));
    }
    Ok(HttpRequest::new(HttpMethod::Get, url)
        .with_header("accept", "application/json")
        .with_header("authorization", bearer_header_value(token)))
}

fn xai_oauth_auxiliary_get_request(
    url: &str,
    token: &str,
    client: &XaiClientContext,
) -> Result<HttpRequest, OAuthError> {
    let mut request = xai_bearer_get_request(url, token)?;
    for (name, value) in xai_oauth_auxiliary_headers(client) {
        request = request.with_header(name, value);
    }
    Ok(request)
}

fn decode_models_response(
    operation: &str,
    response: HttpResponse,
) -> Result<XaiModelsResponse, OAuthError> {
    ensure_success(operation, &response)?;
    serde_json::from_slice(&response.body).map_err(|error| {
        OAuthError::InvalidResponse(format!("could not decode xAI /models response: {error}"))
    })
}

async fn request_with_timeout(
    client: &dyn HttpClient,
    request: HttpRequest,
    operation: &str,
    timeout: Duration,
) -> Result<HttpResponse, OAuthError> {
    tokio::time::timeout(timeout, client.request(request))
        .await
        .map_err(|_| OAuthError::Transport(format!("{operation} timed out")))?
        .map_err(OAuthError::from)
}

fn ensure_success(operation: &str, response: &HttpResponse) -> Result<(), OAuthError> {
    if response.status < 400 {
        return Ok(());
    }
    Err(OAuthError::RefreshFailed(format!(
        "{operation} returned status {}",
        response.status
    )))
}

fn non_empty_token(token: String) -> Option<String> {
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn normalized_owned(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn resolve_xai_user_id(
    client: &dyn HttpClient,
    access_token: &str,
    user_id: &str,
) -> Result<String, OAuthError> {
    let user_id = user_id.trim();
    if !user_id.is_empty() {
        return Ok(user_id.to_string());
    }
    let user = validate_xai_oauth_session(client, access_token).await?;
    Ok(user.user_id)
}

fn insert_optional_extra(extras: &mut BTreeMap<String, JsonValue>, key: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        extras.insert(key.to_string(), JsonValue::string(value));
    }
}

fn remove_conflicting_auth_headers(headers: &mut BTreeMap<String, String>) {
    for name in ["authorization", "x-api-key", "api-key"] {
        remove_header_case_insensitive(headers, name);
    }
}

fn remove_header_case_insensitive(headers: &mut BTreeMap<String, String>, name: &str) {
    headers.retain(|candidate, _| !candidate.eq_ignore_ascii_case(name));
}

fn set_header_case_insensitive(
    headers: &mut BTreeMap<String, String>,
    name: String,
    value: String,
) {
    remove_header_case_insensitive(headers, &name);
    headers.insert(name, value);
}

fn require_user_id<'a>(user_id: &'a str, operation: &str) -> Result<&'a str, OAuthError> {
    let user_id = user_id.trim();
    if user_id.is_empty() {
        return Err(OAuthError::InvalidResponse(format!(
            "{operation} missing userId"
        )));
    }
    Ok(user_id)
}

#[cfg(test)]
#[path = "../test/xai_auth_private.test.rs"]
mod tests;
